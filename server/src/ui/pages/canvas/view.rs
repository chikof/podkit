//! The canvas, drawn: the world (frames, links, nodes), the list view, the toolbar and the
//! popover shells. Everything here is a pure function of the graph and its layout, so the
//! page, the status poll and the post-wire patch draw the same markup.
//!
//! Every name, label and variable goes through maud, which escapes it; nothing user-made is
//! ever `PreEscaped`. A secret is never here: the graph holds the names of env vars only.

use maud::{Markup, html};

use crate::auth::permission::Action;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::projects::graph::layout::{COL_GAP, Layout};
use crate::projects::graph::{EdgeKind, Graph, Node, NodeKey, rules};
use crate::projects::overview::ResourceKind;
use crate::ui::components::{
	LogoSize, Priority, Size, Variant, button_class, icon, image_logo, table,
};
use crate::ui::pages::apps::app_url;
use crate::ui::pages::projects::cards::{in_flight, resource_logo, status_badge, status_data};
use crate::ui::sse::js_string;

/// What drawing needs besides the graph.
pub struct Cx<'a> {
	pub team: TeamId,
	pub graph: &'a Graph,
	pub layout: &'a Layout,
	/// The caller may change what is wired: handles, Connect and Unlink are drawn.
	pub can_wire: bool,
}

impl Cx<'_> {
	fn project(&self) -> ProjectId {
		self.graph.project.id
	}

	/// The page of the resource.
	pub fn node_href(&self, key: NodeKey) -> String {
		let base = format!("/teams/{}/projects/{}", self.team, self.project());
		match key.kind {
			ResourceKind::Application => app_url(self.team, self.project(), ApplicationId(key.id)),
			ResourceKind::Database => format!("{base}/databases/{}", key.id),
			ResourceKind::Model => format!("{base}/models/{}", key.id),
			ResourceKind::Stack => format!("{base}/stacks/{}", key.id),
		}
	}

	fn env_href(&self, slug: &str) -> String {
		format!("/teams/{}/projects/{}/{slug}", self.team, self.project())
	}

	/// Where "open" goes for a link: the variables of the consumer, the model, the networks.
	fn edge_href(&self, e: &crate::projects::graph::Edge) -> String {
		match e.kind {
			EdgeKind::Variable => format!("{}/env", self.node_href(e.from)),
			EdgeKind::Gateway => self.node_href(e.to),
			EdgeKind::Network => self.graph.node(e.from).map_or_else(String::new, |n| {
				format!("/teams/{}/servers/{}/networks", self.team, n.server)
			}),
		}
	}

	pub fn url(&self, tail: &str) -> String {
		format!(
			"/teams/{}/projects/{}/canvas{tail}",
			self.team,
			self.project()
		)
	}
}

/// The inline symbols the canvas needs beyond the shared sprite.
pub fn sprite() -> Markup {
	html! {
		svg width="0" height="0" style="position:absolute" aria-hidden="true" focusable="false" {
			defs {
				symbol id="cv-arrow" viewBox="0 0 24 24" { path d="M5 12h14" {} path d="m12 5 7 7-7 7" {} }
				symbol id="cv-fit" viewBox="0 0 24 24" { path d="M8 3H5a2 2 0 0 0-2 2v3" {} path d="M21 8V5a2 2 0 0 0-2-2h-3" {} path d="M3 16v3a2 2 0 0 0 2 2h3" {} path d="M16 21h3a2 2 0 0 0 2-2v-3" {} }
				symbol id="cv-list" viewBox="0 0 24 24" { path d="M3 12h.01" {} path d="M3 18h.01" {} path d="M3 6h.01" {} path d="M8 12h13" {} path d="M8 18h13" {} path d="M8 6h13" {} }
				symbol id="cv-plug" viewBox="0 0 24 24" { path d="M12 22v-5" {} path d="M9 8V2" {} path d="M15 8V2" {} path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z" {} }
				symbol id="cv-ban" viewBox="0 0 24 24" { circle cx="12" cy="12" r="10" {} path d="m4.9 4.9 14.2 14.2" {} }
				symbol id="cv-unlink" viewBox="0 0 24 24" { path d="m18.84 12.25 1.72-1.71h-.02a5.004 5.004 0 0 0-.12-7.07 5.006 5.006 0 0 0-6.95 0l-1.72 1.71" {} path d="m5.17 11.75-1.71 1.71a5.004 5.004 0 0 0 .12 7.07 5.006 5.006 0 0 0 6.95 0l1.71-1.71" {} path d="M8 2v3" {} path d="M2 8h3" {} path d="M16 19v3" {} path d="M19 16h3" {} }
				symbol id="cv-zoom-focus" viewBox="0 0 24 24" { circle cx="11" cy="11" r="8" {} path d="m21 21-4.3-4.3" {} path d="M11 8v6" {} path d="M8 11h6" {} }
			}
		}
	}
}

/// A symbol of [`sprite`].
pub fn cv_icon(name: &str) -> Markup {
	html! {
		svg class="ui-icon" aria-hidden="true" focusable="false" { use href={ "#cv-" (name) } {} }
	}
}

fn node_logo(n: &Node) -> Markup {
	match &n.logo_url {
		Some(url) => image_logo(url, LogoSize::Sm, Priority::Normal),
		None => resource_logo(n.mark, &n.name, LogoSize::Sm),
	}
}

/// `1 critical finding`, for a screen reader.
fn scan_words(n: &Node) -> Option<String> {
	n.scan.map(|s| {
		format!(
			"{} {} scan finding{}",
			s.count,
			s.level.slug(),
			if s.count == 1 { "" } else { "s" }
		)
	})
}

fn scan_badge(n: &Node) -> Markup {
	html! {
		@if let Some(scan) = n.scan {
			span class="ui-sev ui-cv-sev" data-sev=(scan.level.slug()) title=(scan_words(n).unwrap_or_default()) {
				span class="ui-cv-sev-w" { (scan.level.label()) }
				span class="ui-cv-sev-n" { (scan.count) }
				span class="sr-only" { " scan findings" }
			}
		}
	}
}

/// One node card at its place in the world. `flip` plays the status change animation.
pub fn node(c: &Cx<'_>, n: &Node, flip: bool) -> Markup {
	let (x, y) = c.layout.pos.get(&n.key).copied().unwrap_or((0, 0));
	let id = n.key.to_string();
	let desc = format!(
		"{}, {}{}. ",
		n.key.kind.label(),
		status_data(n.status),
		scan_words(n).map(|s| format!(", {s}")).unwrap_or_default()
	);
	let targets = if c.can_wire {
		rules::targets(c.graph, n)
			.iter()
			.map(ToString::to_string)
			.collect::<Vec<_>>()
			.join(" ")
	} else {
		String::new()
	};
	let env = c.graph.environment(n.environment);
	html! {
		li class="ui-cv-node" id={ "n-" (id) } data-id=(id) data-name=(n.name)
			data-kind=(n.key.kind.slug()) data-env=(n.environment) data-env-name=[env.map(|e| e.name.as_str())]
			data-status=(status_data(n.status)) data-href=(c.node_href(n.key))
			data-x=(x) data-y=(y) data-targets=(targets) style={ "--x:" (x) "px;--y:" (y) "px" } {
			div class="ui-cv-head" {
				span class="ui-cv-ic" { (node_logo(n)) }
				a class="ui-cv-open" href=(c.node_href(n.key)) aria-describedby={ "d-" (id) } { (n.name) }
				button type="button" class={ (button_class(Variant::Ghost, Size::Icon)) " ui-button-sm ui-cv-more" } tabindex="-1"
					aria-label={ "Actions for " (n.name) } aria-haspopup="menu" { (icon("ellipsis")) }
			}
			span class="ui-cv-sub" id={ "d-" (id) } { span class="sr-only" { (desc) } (n.detail) }
			div class="ui-cv-foot" { (status_badge(n.status, flip)) (scan_badge(n)) }
			span class="ui-cv-port ui-cv-port-in" aria-hidden="true" {}
			span class="ui-cv-port ui-cv-port-out" aria-hidden="true" {}
		}
	}
}

/// `6 resources · 4 running · 1 failed`.
fn env_summary(graph: &Graph, env: crate::ids::EnvironmentId) -> String {
	use std::fmt::Write as _;
	let nodes: Vec<&Node> = graph
		.nodes
		.iter()
		.filter(|n| n.environment == env)
		.collect();
	let mut out = format!(
		"{} {}",
		nodes.len(),
		if nodes.len() == 1 {
			"resource"
		} else {
			"resources"
		}
	);
	for key in [
		"running",
		"building",
		"deploying",
		"failed",
		"stopped",
		"pending",
	] {
		let n = nodes
			.iter()
			.filter(|n| status_data(n.status) == key)
			.count();
		if n > 0 {
			let _ = write!(out, " · {n} {key}");
		}
	}
	out
}

/// Reading order: environment, then network band, then layer, then row.
fn reading_order<'a>(c: &Cx<'a>) -> Vec<&'a Node> {
	let band = |n: &Node| {
		n.frame_net().and_then(|net| {
			c.layout
				.nets
				.iter()
				.position(|f| f.env == n.environment && f.net.id == net.id)
		})
	};
	let env_pos = |n: &Node| {
		c.graph
			.environments
			.iter()
			.position(|e| e.id == n.environment)
	};
	let mut nodes: Vec<&Node> = c.graph.nodes.iter().collect();
	nodes.sort_by_key(|n| {
		let (x, y) = c.layout.pos.get(&n.key).copied().unwrap_or((0, 0));
		(env_pos(n), band(n), x, y, n.key)
	});
	nodes
}

fn edge_label(c: &Cx<'_>, e: &crate::projects::graph::Edge) -> String {
	let name = |k: NodeKey| c.graph.node(k).map_or("?", |n| n.name.as_str()).to_owned();
	match e.kind {
		EdgeKind::Variable => format!("{} variable: {} to {}", e.label, name(e.from), name(e.to)),
		EdgeKind::Gateway => format!("Model gateway link: {} to {}", name(e.from), name(e.to)),
		EdgeKind::Network => format!("Network {}: {} to {}", e.label, name(e.from), name(e.to)),
	}
}

/// The world: frames, links, nodes and labels, in one element the viewport transforms.
/// `flip` lists the nodes whose status just changed.
pub fn world(c: &Cx<'_>, flip: &[NodeKey]) -> Markup {
	let (w, h) = (c.layout.width, c.layout.height);
	html! {
		div class="ui-cv-world" id="cv-world" data-w=(w) data-h=(h)
			style={ "width:" (w) "px;height:" (h) "px;--cv-gap:" (COL_GAP) "px" } {
			@for frame in &c.layout.envs {
				@if let Some(env) = c.graph.environment(frame.env) {
					section class="ui-cv-env" aria-label={ (env.name) " environment" }
						style={ "--x:" (frame.rect.x) "px;--y:" (frame.rect.y) "px;--w:" (frame.rect.w) "px;--h:" (frame.rect.h) "px" } {
						div class="ui-cv-env-h" {
							h3 class="ui-cv-env-t" { a href=(c.env_href(&env.slug)) { (env.name) } }
							span class="ui-cv-env-s" id={ "cv-es-" (env.id) } { (env_summary(c.graph, env.id)) }
						}
						button type="button" class={ (button_class(Variant::Ghost, Size::Icon)) " ui-button-sm ui-cv-env-zoom" }
							aria-label={ "Zoom to " (env.name) } { (cv_icon("zoom-focus")) }
					}
				}
			}
			@for frame in &c.layout.nets {
				div class="ui-cv-net" data-net=(frame.net.label)
					style={ "--x:" (frame.rect.x) "px;--y:" (frame.rect.y) "px;--w:" (frame.rect.w) "px;--h:" (frame.rect.h) "px" } {
					span class="ui-cv-net-l" {
						(icon("network")) (frame.net.label)
						small { @if frame.net.auto { "auto" } @else { "user" } }
					}
				}
			}
			svg class="ui-cv-edges" id="cv-edges" width=(w) height=(h) aria-hidden="true" {
				defs {
					marker id="cv-arrowhead" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto" {
						path class="ui-cv-arrow" d="M1 1L9 5L1 9" fill="none" stroke="context-stroke" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" {}
					}
				}
				@for (e, geom) in c.graph.edges.iter().zip(&c.layout.edges) {
					@if !geom.d.is_empty() {
						path class="ui-cv-edge" id=(e.dom_id()) d=(geom.d) data-from=(e.from) data-to=(e.to)
							data-kind=(e.kind.slug()) data-label=(e.label) {}
					}
				}
			}
			ul class="ui-cv-nodes" aria-label="Resources" {
				@for n in reading_order(c) { (node(c, n, flip.contains(&n.key))) }
			}
			div class="ui-cv-elabels" {
				@for (e, geom) in c.graph.edges.iter().zip(&c.layout.edges) {
					@if !geom.d.is_empty() {
						button type="button" class="ui-cv-elabel" data-from=(e.from) data-to=(e.to) data-kind=(e.kind.slug())
							data-label=(e.label) data-href=(c.edge_href(e)) aria-haspopup="menu" aria-label=(edge_label(c, e))
							style={ "--x:" (geom.mid.0) "px;--y:" (geom.mid.1) "px" } {
							@if e.kind == EdgeKind::Gateway { (icon("brain")) }
							(e.label)
						}
					}
				}
			}
			div class="ui-cv-tip" id="cv-tip" role="presentation" {}
		}
	}
}

// --- the list view -----------------------------------------------------------------------

fn connected_to(c: &Cx<'_>, n: &Node) -> Markup {
	let name = |k: NodeKey| c.graph.node(k).map_or("?", |m| m.name.as_str());
	let has = c.graph.edges_of(n.key).next().is_some();
	html! {
		@if has {
			ul class="ui-cv-links" {
				@for e in c.graph.edges.iter().filter(|e| e.from == n.key) {
					li {
						"to " b { (name(e.to)) } " "
						code { @if e.kind == EdgeKind::Gateway { "model gateway" } @else { (e.label) } }
					}
				}
				@for e in c.graph.edges.iter().filter(|e| e.to == n.key) {
					li { "from " b { (name(e.from)) } }
				}
			}
		} @else {
			span class="ui-cv-none" { "No links" }
		}
	}
}

fn list_row(c: &Cx<'_>, n: &Node, flip: bool) -> Vec<Markup> {
	let id = n.key.to_string();
	let targets = if c.can_wire {
		rules::targets(c.graph, n)
	} else {
		Vec::new()
	};
	let pick = format!(
		"$cvFrom = {}; $cvTo = ''; $cvPick = true; @get({})",
		js_string(&id),
		js_string(&c.url("/wire/preview"))
	);
	vec![
		html! {
			span class="ui-cv-cell" data-row=(id) {
				span class="ui-cv-ic" { (node_logo(n)) }
				span {
					a href=(c.node_href(n.key)) class="hover:underline" { (n.name) }
					small { (n.key.kind.label()) " · " (n.detail) }
				}
			}
		},
		status_badge(n.status, flip),
		html! {
			@if n.nets.is_empty() { span class="ui-cv-none" { "none" } } @else {
				span class="ui-cv-nets" { @for net in &n.nets { code { (net.label) } } }
			}
		},
		connected_to(c, n),
		html! {
			@if n.scan.is_some() { (scan_badge(n)) }
			@else if matches!(n.key.kind, ResourceKind::Application | ResourceKind::Stack) { span class="ui-cv-none" { "Not scanned" } }
			@else { span class="ui-cv-none" { "n/a" } }
		},
		html! {
			@if c.can_wire && !targets.is_empty() {
				button type="button" class=(button_class(Variant::Outline, Size::Sm)) aria-haspopup="dialog"
					aria-label={ "Connect " (n.name) " to…" } "data-on:click"=(pick) {
					(cv_icon("plug")) span { "Connect…" }
				}
			}
		},
	]
}

/// One table per environment with every fact the canvas has: the view for a screen reader,
/// for the keyboard and for a phone.
pub fn list(c: &Cx<'_>, flip: &[NodeKey]) -> Markup {
	html! {
		div class="ui-cv-list" id="cv-list" {
			@for env in &c.graph.environments {
				@let nodes: Vec<&Node> = c.graph.nodes.iter().filter(|n| n.environment == env.id).collect();
				@if !nodes.is_empty() {
					section aria-labelledby={ "l-" (env.id) } {
						h3 id={ "l-" (env.id) } { (env.name) " " small { (env_summary(c.graph, env.id)) } }
						(table(
							&format!("cv-table-{}", env.id),
							&format!("Resources in {} and how they are connected", env.name),
							&["Resource", "Status", "Network", "Connected to", "Scan", ""],
							&nodes.iter().map(|n| list_row(c, n, flip.contains(&n.key))).collect::<Vec<_>>(),
						))
					}
				}
			}
		}
	}
}

// --- the page ----------------------------------------------------------------------------

fn toolbar(c: &Cx<'_>) -> Markup {
	html! {
		div class="ui-cv-bar" role="toolbar" aria-label="Canvas tools" {
			div class="ui-tabs" role="radiogroup" aria-label="View" {
				input type="radio" name="cvv" id="cv-v-canvas" value="canvas" checked;
				label for="cv-v-canvas" class="ui-tab" { "Canvas" }
				input type="radio" name="cvv" id="cv-v-list" value="list";
				label for="cv-v-list" class="ui-tab" { "List" }
			}
			span class="ui-cv-tools" {
				button type="button" class="ui-chip" aria-pressed="true" data-cv-toggle="labels" { "Link labels" }
				button type="button" class="ui-chip" aria-pressed="true" data-cv-toggle="nets" { "Networks" }
				@if c.can_wire {
					button type="button" class=(button_class(Variant::Secondary, Size::Sm)) aria-pressed="false" data-cv-connect {
						(cv_icon("plug")) span { "Connect" }
					}
				}
				span class="ui-cv-sp" {}
				span class="ui-cv-zoom" role="group" aria-label="Zoom" {
					button type="button" class={ (button_class(Variant::Ghost, Size::Icon)) " ui-button-sm" } data-cv-zoom="out" aria-label="Zoom out" { (icon("minus")) }
					output id="cv-zoom-out" aria-label="Zoom level" { "100%" }
					button type="button" class={ (button_class(Variant::Ghost, Size::Icon)) " ui-button-sm" } data-cv-zoom="in" aria-label="Zoom in" { (icon("plus")) }
					button type="button" class={ (button_class(Variant::Ghost, Size::Icon)) " ui-button-sm" } data-cv-zoom="fit" aria-label="Fit to screen" { (cv_icon("fit")) }
				}
			}
		}
	}
}

/// The status poll: present only while something is building or deploying.
pub fn poll(c: &Cx<'_>) -> Markup {
	let busy = c.graph.nodes.iter().any(|n| in_flight(n.status));
	html! {
		div id="cv-poll" { (crate::ui::status::poll(&c.url("/status"), busy)) }
	}
}

/// What the page last showed of each node, as `a-1=running` pairs.
pub fn seen_of(graph: &Graph) -> String {
	graph
		.nodes
		.iter()
		.map(|n| format!("{}={}", n.key, status_data(n.status)))
		.collect::<Vec<_>>()
		.join(";")
}

/// The nodes whose status differs from what the page last showed.
pub fn changed(graph: &Graph, seen: &str) -> Vec<NodeKey> {
	graph
		.nodes
		.iter()
		.filter(|n| {
			let key = format!("{}=", n.key);
			seen.split(';')
				.find_map(|pair| pair.strip_prefix(&key))
				.is_some_and(|before| before != status_data(n.status))
		})
		.map(|n| n.key)
		.collect()
}

/// The environment summaries, to patch after a status change.
pub fn summaries(graph: &Graph) -> Vec<Markup> {
	graph
		.environments
		.iter()
		.map(
			|e| html! { span class="ui-cv-env-s" id={ "cv-es-" (e.id) } { (env_summary(graph, e.id)) } },
		)
		.collect()
}

/// The canvas panel: toolbar, viewport, list view, and the shells the server fills in.
pub fn panel(c: &Cx<'_>) -> Markup {
	let n = c.graph.nodes.len();
	let wire = format!(
		"$cvFrom = evt.detail.from; $cvTo = evt.detail.to; $cvPick = false; @get({})",
		js_string(&c.url("/wire/preview"))
	);
	let unlink = format!(
		"$cvFrom = evt.detail.from; $cvTo = evt.detail.to; $cvKind = evt.detail.kind; $cvLabel = evt.detail.label; @get({})",
		js_string(&c.url("/unlink/preview"))
	);
	html! {
		section class="ui-cv" id="cv" aria-labelledby="cv-h" data-labels="on" data-nets="on"
			data-wire[c.can_wire] "data-on:cv-wire"=(wire) "data-on:cv-unlink"=(unlink) {
			h2 class="sr-only" id="cv-h" { "Project map" }
			(toolbar(c))
			div class="ui-cv-view" id="cv-view" tabindex="0" role="group" data-lod="full" aria-describedby="cv-keys"
				aria-label={ "Project canvas, " (n) @if n == 1 { " resource" } @else { " resources" } ". Tab visits resources, arrow keys follow links, C connects, plus and minus zoom, F fits, Shift and arrows pan." } {
				(world(c, &[]))
				ul class="ui-cv-legend" aria-label="Link types" {
					li { svg viewBox="0 0 28 8" aria-hidden="true" { path d="M0 4h28" {} } "Variable" }
					li { svg viewBox="0 0 28 8" aria-hidden="true" { path class="g" d="M0 4h28" {} } "Model gateway" }
					li { svg viewBox="0 0 28 8" aria-hidden="true" { path class="n" d="M0 4h28" {} } "Network" }
				}
				p class="ui-cv-hint" id="cv-keys" {
					@if c.can_wire { "Drag from a handle to connect " }
					kbd class="ui-kbd" { "Ctrl" } "+ scroll to zoom"
				}
				p class="sr-only" id="cv-live" role="status" aria-live="polite" {}
			}
			(list(c, &[]))
		}
		div id="cv-pop" class="ui-cv-pop" popover aria-label="Connect resources" {}
		div id="cv-menu" class="ui-menu" popover {}
		(poll(c))
		(sprite())
	}
}

/// Whether the caller can change what is wired. The services check again on every action.
pub fn can_wire(perms: &crate::ui::extract::TeamCtx) -> bool {
	use crate::auth::permission::Resource;
	perms.perms.can(Action::Update, Resource::Application)
		|| perms.perms.can(Action::Update, Resource::Stack)
}
