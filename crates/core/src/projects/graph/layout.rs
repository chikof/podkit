//! The canvas layout: where every node, frame and link goes, computed on the server so the
//! client never measures anything. Deterministic: the same graph gives the same pixels, and
//! a status change moves nothing.
//!
//! Layers (columns) are the longest path from a node nothing points to, so a consumer is
//! always left of its provider. Rows are banded per network: each network gets a band of
//! rows, plus one band for resources on none, and a network frame is the bounding box of its
//! band, so a frame can never overlap a node that is not in it. Cycles are broken by edge
//! order. Ported from the layout of the canvas design prototype (the spec is
//! `design/canvas/README.md`; the prototype itself is no longer in the repository).

use std::collections::HashMap;

use super::{EdgeKind, Graph, NetRef, Node, NodeKey};
use crate::ids::EnvironmentId;

/// Node size, at zoom 1.
pub const NODE_W: i32 = 208;
pub const NODE_H: i32 = 92;
const ROW_GAP: i32 = 16;
/// Between two columns; the link label sits in it.
pub const COL_GAP: i32 = 112;
const ENV_PAD: i32 = 16;
const NET_PAD: i32 = 14;
const ENV_HEAD: i32 = 44;
const NET_HEAD: i32 = 30;
const BAND_GAP: i32 = 20;
const ENV_GAP: i32 = 28;
/// Environments side by side would need more than this at 60 % zoom on a 1440 px screen:
/// stack them instead.
const SIDE_BY_SIDE_MAX: i32 = 2400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rect {
	pub x: i32,
	pub y: i32,
	pub w: i32,
	pub h: i32,
}

#[cfg(test)]
impl Rect {
	#[must_use]
	pub fn overlaps(&self, other: &Self) -> bool {
		self.x < other.x + other.w
			&& other.x < self.x + self.w
			&& self.y < other.y + other.h
			&& other.y < self.y + self.h
	}

	#[must_use]
	pub fn contains(&self, other: &Self) -> bool {
		self.x <= other.x
			&& self.y <= other.y
			&& other.x + other.w <= self.x + self.w
			&& other.y + other.h <= self.y + self.h
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFrame {
	pub env: EnvironmentId,
	pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetFrame {
	pub env: EnvironmentId,
	pub net: NetRef,
	pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeGeom {
	/// The SVG path, from the consumer's out handle to the provider's in handle.
	pub d: String,
	/// Where the label sits.
	pub mid: (i32, i32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
	pub width: i32,
	pub height: i32,
	pub envs: Vec<EnvFrame>,
	pub nets: Vec<NetFrame>,
	pub pos: HashMap<NodeKey, (i32, i32)>,
	/// One per edge of the graph, in the graph's order.
	pub edges: Vec<EdgeGeom>,
}

#[cfg(test)]
impl Layout {
	#[must_use]
	pub fn rect_of(&self, key: NodeKey) -> Option<Rect> {
		self.pos.get(&key).map(|&(x, y)| Rect {
			x,
			y,
			w: NODE_W,
			h: NODE_H,
		})
	}
}

/// One environment laid out around its own origin.
struct Local {
	env: EnvironmentId,
	w: i32,
	h: i32,
	pos: Vec<(NodeKey, i32, i32)>,
	nets: Vec<(NetRef, Rect)>,
}

fn depth(
	key: NodeKey,
	feeds: &HashMap<NodeKey, Vec<NodeKey>>,
	done: &mut HashMap<NodeKey, i32>,
	visiting: &mut Vec<NodeKey>,
) -> i32 {
	if let Some(&d) = done.get(&key) {
		return d;
	}
	visiting.push(key);
	let mut best = -1;
	for &from in feeds.get(&key).map_or(&[][..], Vec::as_slice) {
		// An edge back into a node being resolved closes a cycle: it does not count.
		if visiting.contains(&from) {
			continue;
		}
		best = best.max(depth(from, feeds, done, visiting));
	}
	visiting.pop();
	done.insert(key, best + 1);
	best + 1
}

fn layers(nodes: &[&Node], graph: &Graph) -> HashMap<NodeKey, i32> {
	let ids: Vec<NodeKey> = nodes.iter().map(|n| n.key).collect();
	// Provider -> consumers feeding it, in edge order (the graph's edges are sorted).
	let mut feeds: HashMap<NodeKey, Vec<NodeKey>> = HashMap::new();
	for e in graph
		.edges
		.iter()
		.filter(|e| e.kind != EdgeKind::Network && ids.contains(&e.from) && ids.contains(&e.to))
	{
		feeds.entry(e.to).or_default().push(e.from);
	}
	let mut done = HashMap::new();
	for &key in &ids {
		depth(key, &feeds, &mut done, &mut Vec::new());
	}
	done
}

fn local(graph: &Graph, env: EnvironmentId) -> Local {
	let nodes: Vec<&Node> = graph
		.nodes
		.iter()
		.filter(|n| n.environment == env)
		.collect();
	let layer = layers(&nodes, graph);
	let ncols = layer.values().copied().max().map_or(1, |m| m + 1);

	// Bands: resources on no network first, then one per network by name.
	let mut bands: Vec<Option<&NetRef>> = vec![None];
	let mut named: Vec<&NetRef> = nodes.iter().filter_map(|n| n.frame_net()).collect();
	named.sort_by(|a, b| (&a.label, a.id.0).cmp(&(&b.label, b.id.0)));
	named.dedup_by_key(|n| n.id);
	bands.extend(named.into_iter().map(Some));

	let mut rowof: HashMap<NodeKey, usize> = HashMap::new();
	let mut pos = Vec::new();
	let mut nets = Vec::new();
	let mut y = ENV_HEAD;
	for band in bands {
		let members: Vec<&&Node> = nodes
			.iter()
			.filter(|n| n.frame_net().map(|f| f.id) == band.map(|b| b.id))
			.collect();
		if members.is_empty() {
			continue;
		}
		let mut rows = 0usize;
		for col in 0..ncols {
			let mut cell: Vec<&&Node> = members
				.iter()
				.copied()
				.filter(|n| layer[&n.key] == col)
				.collect();
			// Close to the consumers already placed, then by key: fewer crossing links.
			let mean = |n: &Node| -> f64 {
				let rows: Vec<f64> = graph
					.edges
					.iter()
					.filter(|e| e.kind != EdgeKind::Network && e.to == n.key)
					.filter_map(|e| rowof.get(&e.from))
					.map(|&r| r as f64)
					.collect();
				if rows.is_empty() {
					0.0
				} else {
					rows.iter().sum::<f64>() / rows.len() as f64
				}
			};
			cell.sort_by(|a, b| mean(a).total_cmp(&mean(b)).then_with(|| a.key.cmp(&b.key)));
			for (i, n) in cell.iter().enumerate() {
				rowof.insert(n.key, i);
			}
			rows = rows.max(cell.len());
		}
		let top = y + if band.is_some() { NET_HEAD } else { 0 };
		let mut cmin = i32::MAX;
		let mut cmax = 0;
		for n in &members {
			let col = layer[&n.key];
			cmin = cmin.min(col);
			cmax = cmax.max(col);
			pos.push((
				n.key,
				ENV_PAD + NET_PAD + col * (NODE_W + COL_GAP),
				top + n32(rowof[&n.key]) * (NODE_H + ROW_GAP),
			));
		}
		let band_h = n32(rows) * NODE_H + (n32(rows) - 1) * ROW_GAP;
		if let Some(net) = band {
			nets.push((
				net.clone(),
				Rect {
					x: ENV_PAD + cmin * (NODE_W + COL_GAP),
					y,
					w: (cmax - cmin) * (NODE_W + COL_GAP) + NODE_W + 2 * NET_PAD,
					h: NET_HEAD + band_h + NET_PAD,
				},
			));
			y += NET_HEAD + band_h + NET_PAD + BAND_GAP;
		} else {
			y += band_h + BAND_GAP;
		}
	}
	Local {
		env,
		w: 2 * ENV_PAD + 2 * NET_PAD + ncols * NODE_W + (ncols - 1) * COL_GAP,
		h: (y - BAND_GAP + ENV_PAD).max(ENV_HEAD + NODE_H + ENV_PAD),
		pos,
		nets,
	}
}

/// The path from `from`'s out handle to `to`'s in handle: a straight line on one row, else a
/// cubic with horizontal tangents.
fn route(from: (i32, i32), to: (i32, i32)) -> EdgeGeom {
	let (x0, y0) = (from.0 + NODE_W, from.1 + NODE_H / 2);
	let (x1, y1) = (to.0, to.1 + NODE_H / 2);
	let d = if y0 == y1 {
		format!("M{x0} {y0}L{x1} {y1}")
	} else {
		let dx = 40.max((x1 - x0) / 2);
		format!("M{x0} {y0}C{} {y0} {} {y1} {x1} {y1}", x0 + dx, x1 - dx)
	};
	EdgeGeom {
		d,
		mid: (i32::midpoint(x0, x1), i32::midpoint(y0, y1)),
	}
}

/// Lays out `graph`.
#[must_use]
pub fn layout(graph: &Graph) -> Layout {
	let locals: Vec<Local> = graph
		.environments
		.iter()
		.map(|e| local(graph, e.id))
		.filter(|l| !l.pos.is_empty())
		.collect();
	let total: i32 =
		locals.iter().map(|l| l.w).sum::<i32>() + ENV_GAP * (n32(locals.len()) - 1).max(0);
	let stacked = total > SIDE_BY_SIDE_MAX;

	let mut out = Layout::default();
	let (mut ox, mut oy) = (0, 0);
	for l in &locals {
		out.envs.push(EnvFrame {
			env: l.env,
			rect: Rect {
				x: ox,
				y: oy,
				w: l.w,
				h: l.h,
			},
		});
		for (key, x, y) in &l.pos {
			out.pos.insert(*key, (ox + x, oy + y));
		}
		for (net, r) in &l.nets {
			out.nets.push(NetFrame {
				env: l.env,
				net: net.clone(),
				rect: Rect {
					x: ox + r.x,
					y: oy + r.y,
					w: r.w,
					h: r.h,
				},
			});
		}
		out.width = out.width.max(ox + l.w);
		out.height = out.height.max(oy + l.h);
		if stacked {
			oy += l.h + ENV_GAP;
		} else {
			ox += l.w + ENV_GAP;
		}
	}
	out.edges = graph
		.edges
		.iter()
		.map(|e| match (out.pos.get(&e.from), out.pos.get(&e.to)) {
			(Some(&a), Some(&b)) => route(a, b),
			_ => EdgeGeom {
				d: String::new(),
				mid: (0, 0),
			},
		})
		.collect();
	out
}

fn n32(n: usize) -> i32 {
	i32::try_from(n).unwrap_or(i32::MAX)
}
