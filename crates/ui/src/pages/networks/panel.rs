//! The Networks panel of a resource page: the networks the resource is on (its environment's
//! first), the names it answers to there, and attaching or detaching a network. The
//! application, database, model and stack pages put [`content`] in their own frame; the two
//! actions here are shared by all four.
//!
//! A change is saved at once and applies on the next deploy, so a saved change offers to
//! redeploy right away, like every other setting of a resource.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	INPUT, LABEL, SELECT, Size, Tone, Variant, badge, button_class, card, card_content,
	card_section, empty_state, icon,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::sse::{self, js_string};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{NetworkId, ProjectId, TeamId};
use podkit_core::networks::service::{self, ResourceNetworks};
use podkit_core::networks::{Network, ResourceKind};
use podkit_core::state::AppState;

const PANEL: &str = "resource-networks";

pub fn router() -> Router<AppState> {
	const R: &str = "/teams/{team_id}/projects/{project_id}/networks/{kind}/{resource_id}";
	Router::new()
		.route(&format!("{R}/attach"), post(attach))
		.route(&format!("{R}/detach/{{network_id}}"), post(detach))
}

/// What a person must be allowed to change a resource's networks.
fn can_change(ctx: &TeamCtx, kind: ResourceKind) -> bool {
	ctx.perms.can(Action::Update, kind.permission())
}

/// The action that deploys the resource again, and who may start it.
fn redeploy(ctx: &TeamCtx, project: ProjectId, kind: ResourceKind, id: i64) -> Option<String> {
	let base = format!("/teams/{}/projects/{project}", ctx.team.id);
	let (path, allowed) = match kind {
		ResourceKind::Application => (
			format!("{base}/apps/{id}/deploy"),
			ctx.perms.can(Action::Create, Resource::Deployment),
		),
		ResourceKind::Stack => (
			format!("{base}/stacks/{id}/deploy"),
			ctx.perms.can(Action::Update, Resource::Stack),
		),
		ResourceKind::Database => (
			format!("{base}/databases/{id}/restart"),
			ctx.perms.can(Action::Update, Resource::Database),
		),
		ResourceKind::Model => (
			format!("{base}/models/{id}/restart"),
			ctx.perms.can(Action::Update, Resource::Model),
		),
	};
	allowed.then_some(path)
}

fn kind_tag(network: &Network) -> Markup {
	if network.is_environment() {
		badge(Tone::Progress, "Environment")
	} else if network.imported {
		badge(Tone::Neutral, "Imported")
	} else {
		badge(Tone::Neutral, "User")
	}
}

fn saved_slot(redeploy: Option<&str>, saved: bool) -> Markup {
	html! {
		div id="networks-saved" {
			@if saved {
				div class="flex flex-wrap items-center justify-between gap-3 rounded-[length:var(--r-md)] border border-success/30 bg-success/15 px-4 py-3 text-sm" role="status" {
					span { "Saved. Changes apply on the next deploy." }
					@if let Some(url) = redeploy {
						button type="button" class=(button_class(Variant::Outline, Size::Sm))
							"data-on:click"={ "@post(" (js_string(url)) ")" } { "Redeploy now" }
					}
				}
			}
		}
	}
}

const ROW: &str = "ui-card-row";

fn joined_list(base: &str, view: &ResourceNetworks, can_change: bool) -> Markup {
	let kind = view.kind;
	html! {
		ul class="ui-card-rows" aria-label="Networks this resource is on" {
			@for (network, aliases) in &view.joined {
				@let removable = can_change && (network.is_user() || (kind == ResourceKind::Stack && network.is_environment()));
				li class=(ROW) {
					div class="flex min-w-0 flex-col gap-1" {
						span class="flex flex-wrap items-center gap-2" {
							span class="font-medium" { (network.name) }
							(kind_tag(network))
							@if network.internal { (badge(Tone::Warning, "No outbound")) }
						}
						code class="font-mono text-xs break-all text-muted-foreground" { (network.podman_name) }
						@if !aliases.is_empty() {
							span class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground" {
								"Answers to"
								@for alias in aliases { code class="font-mono" { (alias) } }
							}
						} @else if network.is_environment() && kind != ResourceKind::Stack {
							span class="text-xs text-muted-foreground" { "Names are set at the next deploy." }
						}
					}
					@if removable {
						button type="button" class=(button_class(Variant::Ghost, Size::Icon))
							"data-on:click"={ "@post(" (js_string(&format!("{base}/detach/{}", network.id))) ")" }
							"data-attr:disabled"="$_busy" {
							(icon("x")) span class="sr-only" { "Detach " (network.name) }
						}
					}
				}
			}
		}
	}
}

fn attach_form(base: &str, view: &ResourceNetworks) -> Markup {
	let submit = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/attach"))
	);
	html! {
		(card(&html! {
			(card_section("Attach a network", Some("The resource answers to these names on it. Leave them empty to use the resource's own name."), None))
			(card_content(&html! {
				form class="flex flex-col gap-4" "data-on:submit"=(submit) {
					div class="ui-field-group" {
						label for="attach-network" class=(LABEL) { "Network" }
						div class="ui-select-wrap" {
							select id="attach-network" class=(SELECT) required "data-bind:net_id" {
								button { selectedcontent {} }
								option value="" { "Choose a network" }
								@for n in &view.available {
									option value=(n.id) { (n.name) }
								}
							}
							(icon("chevron-down"))
						}
					}
					div class="ui-field-group" {
						label for="attach-aliases" class=(LABEL) { "Names on this network" }
						input id="attach-aliases" class=(INPUT) type="text" autocomplete="off" spellcheck="false"
							placeholder="api, backend" "data-bind:net_aliases";
						small class="text-xs text-muted-foreground" { "Comma separated; lowercase letters, digits, dots, hyphens and underscores." }
					}
					(form_error())
					div {
						button type="submit" class=(button_class(Variant::Default, Size::Md))
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Attach network" }
					}
				}
			}))
		}))
	}
}

/// The panel: what the resource is on, and the form to attach more.
pub(crate) fn panel(
	ctx: &TeamCtx,
	project: ProjectId,
	view: &ResourceNetworks,
	saved: bool,
) -> Markup {
	let kind = view.kind;
	let id = view.resource.id;
	let base = format!(
		"/teams/{}/projects/{project}/networks/{}/{id}",
		ctx.team.id,
		kind.as_str()
	);
	let can = can_change(ctx, kind);
	let redeploy = redeploy(ctx, project, kind, id);
	html! {
		div id=(PANEL) class="flex flex-col gap-6"
			data-signals=(json!({ "net_id": "", "net_aliases": "", "_error": "", "_busy": false }).to_string()) {
			(saved_slot(redeploy.as_deref(), saved))
			(card(&html! {
				(card_section(
					"Networks",
					Some("Containers on the same network reach each other by name. A resource is always on its environment's network; attach others to connect it to resources elsewhere."),
					None,
				))
				@if view.joined.is_empty() {
					(empty_state("network", "Not on any network yet.", Some("It joins its environment's network with its first deploy."), None))
				} @else {
					(joined_list(&base, view, can))
				}
			}))
			@if can && !view.available.is_empty() {
				(attach_form(&base, view))
			}
		}
	}
}

/// The Networks tab of a resource page, ready to go inside its frame.
pub(crate) async fn content(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	kind: ResourceKind,
	id: i64,
) -> Result<Markup, Response> {
	let view = service::for_resource(s, ctx.user.id, ctx.team.id, kind, id)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	if view.resource.project_id != project.0 {
		return Err(error::page(ctx, &kind.not_found()));
	}
	Ok(panel(ctx, project, &view, false))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AttachSignals {
	net_id: String,
	net_aliases: String,
}

fn parse_aliases(raw: &str) -> Vec<String> {
	raw.split([',', ' '])
		.map(str::trim)
		.filter(|a| !a.is_empty())
		.map(str::to_owned)
		.collect()
}

fn kind_of(raw: &str) -> Result<ResourceKind, ServerError> {
	ResourceKind::parse(raw).ok_or(ServerError::NotFound)
}

/// Runs `change`, then answers with the panel as it is now.
async fn respond(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	kind: ResourceKind,
	id: i64,
	change: impl std::future::Future<Output = Result<(), ServerError>>,
) -> Response {
	let result = async {
		change.await?;
		let view = service::for_resource(s, ctx.user.id, ctx.team.id, kind, id).await?;
		if view.resource.project_id != project.0 {
			return Err(kind.not_found());
		}
		Ok(view)
	}
	.await;
	match result {
		Ok(view) => sse::patch(&panel(ctx, project, &view, true)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn attach(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, kind, id)): Path<(TeamId, ProjectId, String, i64)>,
	ReadSignals(sig): ReadSignals<AttachSignals>,
) -> Response {
	let Ok(kind) = kind_of(&kind) else {
		return error::action(&ServerError::NotFound);
	};
	let Ok(network) = sig.net_id.trim().parse::<i64>() else {
		return error::action(&ServerError::Validation("Choose a network.".to_owned()));
	};
	let aliases = parse_aliases(&sig.net_aliases);
	respond(
		&s,
		&ctx,
		project,
		kind,
		id,
		service::attach(
			&s,
			ctx.user.id,
			ctx.team.id,
			NetworkId(network),
			kind,
			id,
			&aliases,
		),
	)
	.await
}

async fn detach(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, kind, id, network)): Path<(TeamId, ProjectId, String, i64, NetworkId)>,
) -> Response {
	let Ok(kind) = kind_of(&kind) else {
		return error::action(&ServerError::NotFound);
	};
	respond(
		&s,
		&ctx,
		project,
		kind,
		id,
		service::detach(&s, ctx.user.id, ctx.team.id, network, kind, id),
	)
	.await
}
