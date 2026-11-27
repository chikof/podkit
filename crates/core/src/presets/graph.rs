//! A preset as the project canvas would draw it: the [`Graph`] of what applying it makes.
//!
//! Nothing of it exists yet, so the ids are made up here (they only tell the nodes apart)
//! and every status is "not deployed". The canvas layout and its markup take it as they take
//! the graph of a real project.

use std::collections::HashMap;

use time::OffsetDateTime;

use crate::environments::Environment;
use crate::iac::doc::{Doc, ProjectDoc};
use crate::ids::{EnvironmentId, NetworkId, ProjectId, ServerId, TeamId};
use crate::projects::Project;
use crate::projects::graph::{
	Edge, EdgeKind, Graph, NetRef, Node, NodeKey, engine_var, network_edges,
};
use crate::projects::overview::{ResourceKind, ResourceStatus};

fn database_mark(engine: &str) -> Option<&'static str> {
	["postgres", "mysql", "mongodb", "redis"]
		.into_iter()
		.find(|known| *known == engine)
}

struct Draft<'a> {
	graph: Graph,
	/// A node by where the file has it: environment slug, kind, name.
	at: HashMap<(&'a str, ResourceKind, &'a str), NodeKey>,
}

impl<'a> Draft<'a> {
	fn add(
		&mut self,
		env: (&'a str, EnvironmentId),
		kind: ResourceKind,
		name: &'a str,
		detail: String,
		mark: Option<&'static str>,
	) {
		let key = NodeKey::new(kind, i64::try_from(self.graph.nodes.len()).unwrap_or(0) + 1);
		self.at.insert((env.0, kind, name), key);
		self.graph.nodes.push(Node {
			key,
			name: name.to_owned(),
			detail,
			mark,
			logo_url: None,
			status: ResourceStatus::NotDeployed,
			environment: env.1,
			server: ServerId(0),
			nets: Vec::new(),
			var: None,
			env_keys: Vec::new(),
			scan: None,
		});
	}
}

/// The graph of the one project a preset holds; `None` when it holds none.
#[must_use]
pub fn graph(doc: &Doc) -> Option<Graph> {
	let (slug, project) = doc.projects.iter().next()?;
	let mut draft = Draft {
		graph: Graph {
			project: Project {
				id: ProjectId(0),
				team_id: TeamId(0),
				name: project.name.clone(),
				slug: slug.clone(),
			},
			environments: Vec::new(),
			nodes: Vec::new(),
			edges: Vec::new(),
		},
		at: HashMap::new(),
	};
	nodes(project, &mut draft);
	let Draft { mut graph, at } = draft;

	// A network a person made, and who is on it.
	for (n, network) in project.networks.iter().enumerate() {
		let net = NetRef {
			id: NetworkId(i64::try_from(n).unwrap_or(0) + 1),
			label: network.name.clone(),
			auto: false,
		};
		for member in &network.members {
			let key = ResourceKind::from_slug(&member.kind).and_then(|kind| {
				at.get(&(member.environment.as_str(), kind, member.name.as_str()))
			});
			if let Some(node) = key.and_then(|key| graph.nodes.iter_mut().find(|n| n.key == *key)) {
				node.nets.push(net.clone());
			}
		}
	}

	// An application reaches the models it is linked to through the gateway.
	for (env, e) in &project.environments {
		for (name, app) in &e.apps {
			let Some(from) = at.get(&(env.as_str(), ResourceKind::Application, name.as_str()))
			else {
				continue;
			};
			for link in &app.models {
				let model = at
					.iter()
					.filter(|((_, kind, n), _)| *kind == ResourceKind::Model && *n == link.model)
					// The one of its own environment first.
					.min_by_key(|((model_env, ..), key)| (*model_env != env.as_str(), **key));
				if let Some((_, to)) = model {
					graph.edges.push(Edge {
						from: *from,
						to: *to,
						kind: EdgeKind::Gateway,
						label: "gateway".to_owned(),
						network: None,
					});
				}
			}
		}
	}

	graph.edges.extend(network_edges(&graph.nodes));
	graph
		.edges
		.sort_by(|a, b| (a.from, a.to, a.kind, &a.label).cmp(&(b.from, b.to, b.kind, &b.label)));
	graph.edges.dedup();
	Some(graph)
}

fn nodes<'a>(project: &'a ProjectDoc, draft: &mut Draft<'a>) {
	for (position, (slug, e)) in project.environments.iter().enumerate() {
		let position = i32::try_from(position).unwrap_or(0);
		let id = EnvironmentId(i64::from(position) + 1);
		draft.graph.environments.push(Environment {
			id,
			project_id: ProjectId(0),
			name: e.name.clone(),
			slug: slug.clone(),
			position,
			created_at: OffsetDateTime::UNIX_EPOCH,
		});
		let env = (slug.as_str(), id);
		for (name, app) in &e.apps {
			let detail = app.image.clone().or_else(|| app.repo.clone());
			let mark = app.image.is_some().then_some("docker");
			draft.add(
				env,
				ResourceKind::Application,
				name,
				detail.unwrap_or_default(),
				mark,
			);
		}
		for (name, db) in &e.databases {
			let detail = match &db.version {
				Some(version) => format!("{} {version}", db.engine),
				None => db.engine.clone(),
			};
			draft.add(
				env,
				ResourceKind::Database,
				name,
				detail,
				database_mark(&db.engine),
			);
			if let Some(node) = draft.graph.nodes.last_mut() {
				node.var = Some(engine_var(&db.engine).to_owned());
			}
		}
		for (name, model) in &e.models {
			let mark = if model.engine == "ollama" {
				"ollama"
			} else {
				"huggingface"
			};
			draft.add(
				env,
				ResourceKind::Model,
				name,
				model.repo.clone(),
				Some(mark),
			);
		}
		for name in e.stacks.keys() {
			draft.add(
				env,
				ResourceKind::Stack,
				name,
				"compose file".to_owned(),
				Some("docker"),
			);
		}
	}
}

/// The canvas of the preset `file` as numbers: every frame, tile and line where the canvas
/// layout puts it. It is what a page that is not podkit's draws the same picture from.
/// `None` when the file is not a preset or holds nothing.
#[must_use]
pub fn drawing(file: &str) -> Option<serde_json::Value> {
	use serde_json::json;

	use crate::projects::graph::layout::{NODE_H, NODE_W, Rect, layout};

	let graph = crate::iac::doc::parse(file)
		.ok()
		.and_then(|doc| graph(&doc))
		.filter(|g| !g.nodes.is_empty())?;
	let l = layout(&graph);
	let rect = |r: &Rect| json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h });
	Some(json!({
		"width": l.width,
		"height": l.height,
		"node": { "w": NODE_W, "h": NODE_H },
		"environments": l.envs.iter().filter_map(|f| {
			let name = &graph.environment(f.env)?.name;
			Some(json!({ "name": name, "rect": rect(&f.rect) }))
		}).collect::<Vec<_>>(),
		"networks": l.nets.iter()
			.map(|f| json!({ "name": f.net.label, "rect": rect(&f.rect) }))
			.collect::<Vec<_>>(),
		"nodes": graph.nodes.iter().map(|n| {
			let (x, y) = l.pos.get(&n.key).copied().unwrap_or((0, 0));
			json!({
				"kind": n.key.kind.slug(), "name": n.name, "detail": n.detail, "mark": n.mark,
				"x": x, "y": y,
			})
		}).collect::<Vec<_>>(),
		"links": graph.edges.iter().zip(&l.edges).filter(|(_, g)| !g.d.is_empty())
			.map(|(e, g)| json!({
				"kind": e.kind.slug(), "label": e.label, "d": g.d, "x": g.mid.0, "y": g.mid.1,
			}))
			.collect::<Vec<_>>(),
	}))
}

/// `server preset-canvas <dir>`: the [`drawing`] of every `presets/<id>/preset.toml` under
/// `dir`, as one JSON object by id on standard output. The exit code when that is what the
/// process was started for.
#[must_use]
pub fn canvas_cli() -> Option<i32> {
	let args: Vec<String> = std::env::args().collect();
	if args.get(1).map(String::as_str) != Some("preset-canvas") {
		return None;
	}
	let Some(dir) = args.get(2) else {
		eprintln!("usage: server preset-canvas <dir with a presets/ folder>");
		return Some(2);
	};
	let mut out = serde_json::Map::new();
	let folders = std::fs::read_dir(std::path::Path::new(dir).join("presets"))
		.map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
		.unwrap_or_default();
	for folder in folders {
		let drawn = std::fs::read_to_string(folder.join("preset.toml"))
			.ok()
			.and_then(|text| drawing(&text));
		if let (Some(id), Some(drawn)) = (folder.file_name().and_then(|n| n.to_str()), drawn) {
			out.insert(id.to_owned(), drawn);
		}
	}
	println!("{}", serde_json::Value::Object(out));
	Some(0)
}
