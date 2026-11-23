//! The project canvas: a live map of a project's resources, environments, networks and links,
//! where a person wires things together. An Operate surface (see `design/canvas/README.md`),
//! so there is no hero: a toolbar, the map, and a list of the same facts.
//!
//! The graph, its layout and every wiring rule live in `projects::graph`; this module only
//! draws them and turns a Datastar request into a call of that service. Nothing here touches
//! the database or decides who may do what.

mod popover;
#[cfg(test)]
mod tests;
mod view;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use self::popover::Mode;
use self::view::{Cx, can_wire, panel, seen_of};
use crate::error::ServerError;
use crate::ids::{ProjectId, TeamId};
use crate::projects::graph::{self, Edge, EdgeKind, Graph, NodeKey, layout::layout, wire};
use crate::state::AppState;
use crate::ui::components::{Size, Variant, button_class, empty_state, icon};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	const P: &str = "/teams/{team_id}/projects/{project_id}/canvas";
	Router::new()
		.route(P, get(page))
		.route(&format!("{P}/status"), get(status))
		.route(&format!("{P}/wire/preview"), get(wire_preview))
		.route(&format!("{P}/wire"), post(wire_apply))
		.route(&format!("{P}/unlink/preview"), get(unlink_preview))
		.route(&format!("{P}/unlink"), post(unlink_apply))
}

/// What the page and the canvas send back: the two ends, and for an unlink the link's kind and
/// label. `seen` is what the page last showed of each node.
#[derive(Debug, Default, Deserialize)]
struct Signals {
	#[serde(default, rename = "cvFrom")]
	from: String,
	#[serde(default, rename = "cvTo")]
	to: String,
	#[serde(default, rename = "cvKind")]
	kind: String,
	#[serde(default, rename = "cvLabel")]
	label: String,
	#[serde(default, rename = "cvPick")]
	pick: bool,
	#[serde(default)]
	seen: String,
}

impl Signals {
	fn ends(&self) -> Result<(NodeKey, Option<NodeKey>), ServerError> {
		let from = NodeKey::parse(&self.from).ok_or(ServerError::NotFound)?;
		Ok((from, NodeKey::parse(&self.to)))
	}

	/// The link an unlink names, as the graph will know it.
	fn edge(&self) -> Result<Edge, ServerError> {
		let (from, to) = self.ends()?;
		Ok(Edge {
			from,
			to: to.ok_or(ServerError::NotFound)?,
			kind: EdgeKind::from_slug(&self.kind).ok_or(ServerError::NotFound)?,
			label: self.label.clone(),
			network: None,
		})
	}
}

fn crumbs(graph: &Graph) -> Vec<Crumb<'_>> {
	vec![
		("Projects", None),
		(graph.project.name.as_str(), None),
		("Canvas", None),
	]
}

fn resources_url(team: TeamId, project: ProjectId, graph: &Graph) -> String {
	let slug = graph.environments.first().map_or("", |e| e.slug.as_str());
	format!("/teams/{team}/projects/{project}/{slug}")
}

fn lede(graph: &Graph) -> String {
	let n = graph.nodes.len();
	let envs = graph
		.environments
		.iter()
		.filter(|e| graph.nodes.iter().any(|node| node.environment == e.id))
		.count();
	format!(
		"{n} {} in {envs} {}",
		if n == 1 { "resource" } else { "resources" },
		if envs == 1 {
			"environment"
		} else {
			"environments"
		},
	)
}

fn body(ctx: &TeamCtx, graph: &Graph) -> Markup {
	let lay = layout(graph);
	let c = Cx {
		team: ctx.team.id,
		graph,
		layout: &lay,
		can_wire: can_wire(ctx),
	};
	let signals = json!({
		"cvFrom": "", "cvTo": "", "cvKind": "", "cvLabel": "", "cvPick": false,
		"seen": seen_of(graph), "_error": "", "_busy": false,
	});
	let back = resources_url(ctx.team.id, graph.project.id, graph);
	html! {
		div class="ui-wide ui-cv-page" data-signals=(signals.to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Project" }
					h1 { (graph.project.name) }
					p class="ui-lede" { (lede(graph)) }
				}
				div class="ui-page-actions" {
					a class=(button_class(Variant::Ghost, Size::Md)) href=(back) { (icon("layout-grid")) span { "Resource list" } }
				}
			}
			@if graph.nodes.is_empty() {
				(empty_state(
					"network",
					"Nothing to connect yet",
					Some("Resources in this project show up here as a map. Drag from one to another to wire them together and inject the connection variables."),
					Some(&html! {
						a class=(button_class(Variant::Default, Size::Md)) href={ (back) "/create" } { (icon("plus")) span { "Create resource" } }
					}),
				))
			} @else {
				(panel(&c))
				script defer src=(crate::ui::assets::CANVAS_JS_HREF.as_str()) {}
			}
		}
	}
}

async fn load(s: &AppState, ctx: &TeamCtx, project: ProjectId) -> Result<Graph, ServerError> {
	graph::build(s, ctx.user.id, ctx.team.id, project).await
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let graph = load(&s, &ctx, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	Ok(app_page(
		&ctx,
		&format!("{} canvas", graph.project.name),
		&crumbs_for(&ctx, &graph),
		&body(&ctx, &graph),
	))
}

fn crumbs_for<'a>(ctx: &TeamCtx, graph: &'a Graph) -> Vec<Crumb<'a>> {
	let mut crumbs = crumbs(graph);
	crumbs[0].1 = Some(format!("/teams/{}/projects", ctx.team.id));
	crumbs[1].1 = Some(resources_url(ctx.team.id, graph.project.id, graph));
	crumbs
}

/// The live status: the nodes whose status moved since the page last drew them, the
/// environment summaries, and whether to keep polling.
async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Option<Signals>>,
) -> Response {
	let sig = sig.unwrap_or_default();
	let graph = match load(&s, &ctx, project).await {
		Ok(g) => g,
		// A real 404: a poll that lost its project stops.
		Err(e) => return error::page(&ctx, &e),
	};
	let lay = layout(&graph);
	let c = Cx {
		team: ctx.team.id,
		graph: &graph,
		layout: &lay,
		can_wire: can_wire(&ctx),
	};
	let moved = view::changed(&graph, &sig.seen);
	let mut out = sse::patch(&view::poll(&c));
	for key in &moved {
		if let Some(n) = graph.node(*key) {
			out = out.and(sse::patch(&view::node(&c, n, true)));
		}
	}
	if !moved.is_empty() {
		for summary in view::summaries(&graph) {
			out = out.and(sse::patch(&summary));
		}
	}
	out.and(sse::signals(&json!({ "seen": seen_of(&graph) })))
		.into_response()
}

/// A refusal reads as a toast; a missing page or resource is a real 404, like the page.
fn failure(ctx: &TeamCtx, e: &ServerError) -> Response {
	match e {
		ServerError::ProjectNotFound | ServerError::NotFound => error::page(ctx, e),
		_ => sse::toast(&error::message(e)).into_response(),
	}
}

const SHOW: &str =
	"(p => p.matches(':popover-open') || p.showPopover())(document.getElementById('cv-pop'))";

/// The confirm popover for a wire: the exact changes, or the reason there are none. With
/// `cvPick` (the list view) it carries a select of the valid targets.
async fn wire_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let (from, to) = match sig.ends() {
		Ok(ends) => ends,
		Err(e) => return failure(&ctx, &e),
	};
	let graph = match load(&s, &ctx, project).await {
		Ok(g) => g,
		Err(e) => return failure(&ctx, &e),
	};
	let Some(source) = graph.node(from) else {
		return failure(&ctx, &ServerError::NotFound);
	};
	// The list view starts on the first valid target.
	let to = to.filter(|t| graph.node(*t).is_some()).or_else(|| {
		sig.pick
			.then(|| graph::rules::targets(&graph, source).first().copied())
			.flatten()
	});
	let lay = layout(&graph);
	let c = Cx {
		team: ctx.team.id,
		graph: &graph,
		layout: &lay,
		can_wire: can_wire(&ctx),
	};
	let plan = match to {
		Some(to) => match wire::preview(&s, ctx.user.id, ctx.team.id, project, from, to).await {
			Ok((_, plan)) => Some(plan),
			Err(e) if !sig.pick => return failure(&ctx, &e),
			Err(_) => None,
		},
		None if sig.pick => None,
		None => return failure(&ctx, &ServerError::NotFound),
	};
	sse::patch(&popover::popover(
		&c,
		&Mode::Wire,
		from,
		to,
		plan.as_ref(),
		sig.pick,
	))
	.and(sse::script(SHOW))
	.into_response()
}

/// Applies the wire: re-validates on the server, runs the plan, and patches the world.
async fn wire_apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let (from, Some(to)) = (match sig.ends() {
		Ok(ends) => ends,
		Err(e) => return error::action(&e),
	}) else {
		return error::action(&ServerError::NotFound);
	};
	match wire::wire(&s, ctx.user.id, ctx.team.id, project, from, to).await {
		Ok(done) => done_response(&s, &ctx, project, &done, false).await,
		Err(e) => error::action(&e),
	}
}

async fn unlink_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let edge = match sig.edge() {
		Ok(e) => e,
		Err(e) => return failure(&ctx, &e),
	};
	let (graph, plan) = match wire::unpreview(&s, ctx.user.id, ctx.team.id, project, &edge).await {
		Ok(r) => r,
		Err(e) => return failure(&ctx, &e),
	};
	let lay = layout(&graph);
	let c = Cx {
		team: ctx.team.id,
		graph: &graph,
		layout: &lay,
		can_wire: can_wire(&ctx),
	};
	sse::patch(&popover::popover(
		&c,
		&Mode::Unlink,
		edge.from,
		Some(edge.to),
		Some(&plan),
		false,
	))
	.and(sse::script(SHOW))
	.into_response()
}

async fn unlink_apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let edge = match sig.edge() {
		Ok(e) => e,
		Err(e) => return error::action(&e),
	};
	match wire::unwire(&s, ctx.user.id, ctx.team.id, project, &edge).await {
		Ok(done) => done_response(&s, &ctx, project, &done, true).await,
		Err(e) => error::action(&e),
	}
}

/// After a change: close the popover, redraw the world and the list, say what happened, and
/// start polling when something is deploying.
async fn done_response(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	done: &wire::Applied,
	unlink: bool,
) -> Response {
	let graph = match load(s, ctx, project).await {
		Ok(g) => g,
		Err(e) => return error::action(&e),
	};
	let lay = layout(&graph);
	let c = Cx {
		team: ctx.team.id,
		graph: &graph,
		layout: &lay,
		can_wire: can_wire(ctx),
	};
	let (from, to) = done.ends;
	let name = |k: NodeKey| graph.node(k).map_or("resource", |n| n.name.as_str());
	let mut message = if unlink {
		format!("{} unlinked from {}.", name(from), name(to))
	} else {
		format!("{} linked to {}.", name(from), name(to))
	};
	if !done.redeployed.is_empty() {
		message.push_str(" Redeploying.");
	}
	sse::script("document.getElementById('cv-pop')?.hidePopover()")
		.and(sse::patch(&view::world(&c, &[])))
		.and(sse::patch(&view::list(&c, &[])))
		.and(sse::patch(&view::poll(&c)))
		.and(sse::signals(
			&json!({ "seen": seen_of(&graph), "_error": "" }),
		))
		.and(sse::script(&format!(
			"document.getElementById('cv-live').textContent = {}",
			js_string(&message)
		)))
		.and(sse::toast(&message))
		.into_response()
}
