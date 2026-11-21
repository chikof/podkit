//! Networks in the web UI: a server's Networks tab (every network on it, who is on each,
//! create, import and delete, and the one-shot migration of running containers) and the
//! Networks panel that the application, database, model and stack pages embed
//! (see [`panel`]).
//!
//! Every action calls `networks::service`, which authorizes and scopes; nothing here
//! reads the database or decides who may do what.

pub mod panel;
pub mod policy;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::servers::{ServerTab, header as server_header};
use crate::auth::permission::{Action, Resource};
use crate::ids::{NetworkId, ServerId, TeamId};
use crate::networks::service::{self, NetworkView};
use crate::networks::{Member, ResourceKind};
use crate::servers::service as servers;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, Size, Tone, Variant, badge, button_class, dialog,
	dialog_cancel, dialog_open_button, empty_state, icon, table,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::{self, js_string};
use crate::ui::{error, pages::servers as server_pages};

const NEW_DIALOG: &str = "new-network";
const IMPORT_DIALOG: &str = "import-network";
const MIGRATE_DIALOG: &str = "migrate-networks";
const SUGGESTIONS: &str = "importable-networks";

pub fn router() -> Router<AppState> {
	const S: &str = "/teams/{team_id}/servers/{server_id}/networks";
	Router::new()
		.route(S, get(page).post(create))
		.route(&format!("{S}/import"), post(import))
		.route(&format!("{S}/migrate"), post(migrate))
		.route(&format!("{S}/{{network_id}}/delete"), post(delete))
		.merge(panel::router())
}

fn networks_url(team: TeamId, server: ServerId) -> String {
	format!("/teams/{team}/servers/{server}/networks")
}

/// Where a member's own page is.
fn member_url(team: TeamId, member: &Member) -> Option<String> {
	let (project, id) = (member.project_id, member.resource_id);
	let base = format!("/teams/{team}/projects/{project}");
	Some(match member.kind()? {
		ResourceKind::Application => format!("{base}/apps/{id}"),
		ResourceKind::Database => format!("{base}/databases/{id}"),
		ResourceKind::Model => format!("{base}/models/{id}"),
		ResourceKind::Stack => format!("{base}/stacks/{id}"),
	})
}

fn kind_tag(view: &NetworkView) -> Markup {
	let n = &view.network;
	let (tone, label) = if n.is_environment() {
		(Tone::Progress, "Environment")
	} else if n.imported {
		(Tone::Neutral, "Imported")
	} else {
		(Tone::Neutral, "User")
	};
	badge(tone, label)
}

fn egress_tag(internal: bool) -> Markup {
	if internal {
		badge(Tone::Warning, "No outbound")
	} else {
		badge(Tone::Neutral, "Outbound")
	}
}

fn members_cell(team: TeamId, view: &NetworkView) -> Markup {
	html! {
		@if view.members.is_empty() {
			span class="text-muted-foreground" { "Nobody yet" }
		} @else {
			ul class="flex flex-wrap gap-x-3 gap-y-1" aria-label={ "On " (view.network.name) } {
				@for m in &view.members {
					li {
						@if let Some(url) = member_url(team, m) {
							a class="hover:underline" href=(url) { (m.name) }
						} @else {
							(m.name)
						}
					}
				}
			}
		}
	}
}

fn delete_dialog_id(network: NetworkId) -> String {
	format!("delete-network-{network}")
}

fn delete_dialog(base: &str, view: &NetworkView) -> Markup {
	let n = &view.network;
	let post = format!("@post({})", js_string(&format!("{base}/{}/delete", n.id)));
	dialog(
		&delete_dialog_id(n.id),
		&format!("Delete {}?", n.name),
		None,
		&html! {
			div class="flex flex-col gap-3" {
				@if view.members.is_empty() {
					p {
						@if n.imported {
							"podkit forgets this network. The network itself stays on the server."
						} @else {
							"The network is removed from the server."
						}
					}
				} @else {
					p { "Detach the resources on it first: " (view.members.len()) " still " @if view.members.len() == 1 { "is" } @else { "are" } " on this network." }
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" { button class=(BUTTON_OUTLINE) autofocus { "Cancel" } }
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(post)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete network" }
		},
	)
}

fn list(ctx: &TeamCtx, base: &str, views: &[NetworkView], can_update: bool) -> Markup {
	let team = ctx.team.id;
	let rows: Vec<Vec<Markup>> = views
		.iter()
		.map(|v| {
			let n = &v.network;
			vec![
				html! {
					span class="flex flex-col" {
						span class="font-medium" { (n.name) }
						code class="font-mono text-xs text-muted-foreground" { (n.podman_name) }
					}
				},
				kind_tag(v),
				egress_tag(n.internal),
				members_cell(team, v),
				html! {
					@if can_update && n.is_user() {
						(dialog_open_button(
							&delete_dialog_id(n.id),
							&button_class(Variant::Ghost, Size::Icon),
							&html! { (icon("trash-2")) span class="sr-only" { "Delete " (n.name) } },
						))
					}
				},
			]
		})
		.collect();
	html! {
		div id="network-list" {
			@if views.is_empty() {
				(empty_state(
					"network",
					"No networks on this server yet.",
					Some("A network appears here with the first deploy of an environment, or when you create one."),
					None,
				))
			} @else {
				(table("networks-table", "Networks on this server", &["Network", "Kind", "Outbound", "Used by", ""], &rows))
			}
			@if can_update {
				@for v in views.iter().filter(|v| v.network.is_user()) { (delete_dialog(base, v)) }
			}
		}
	}
}

fn new_dialog(base: &str) -> Markup {
	let create = format!("evt.preventDefault(); @post({})", js_string(base));
	dialog(
		NEW_DIALOG,
		"New network",
		Some("Resources you attach to it reach each other by name, across environments."),
		&html! {
			form id="new-network-form" class="flex flex-col gap-4" "data-on:submit"=(create) {
				div class="ui-field-group" {
					label for="net-name" class=(LABEL) { "Name" }
					input id="net-name" class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus "data-bind:net_name";
				}
				div class="ui-check-row" {
					input id="net-internal" type="checkbox" switch role="switch" class="ui-switch" "data-bind:net_internal";
					div {
						label for="net-internal" class="ui-check-title" { "No outbound access" }
						small { "Containers on it can reach each other and nothing outside the server." }
					}
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="new-network-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create network" }
		},
	)
}

fn import_dialog(base: &str, suggestions: &[String]) -> Markup {
	let import = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/import"))
	);
	dialog(
		IMPORT_DIALOG,
		"Import a network",
		Some("Use a Podman network that already exists on this server. podkit never removes it."),
		&html! {
			form id="import-network-form" class="flex flex-col gap-4" "data-on:submit"=(import) {
				div class="ui-field-group" {
					label for="import-name" class=(LABEL) { "Podman network name" }
					input id="import-name" class=(INPUT) type="text" required autocomplete="off" spellcheck="false"
						list=(SUGGESTIONS) autofocus "data-bind:import_name";
					datalist id=(SUGGESTIONS) {
						@for name in suggestions { option value=(name) {} }
					}
					@if suggestions.is_empty() {
						small class="text-xs text-muted-foreground" { "No unmanaged networks were found. Type a name to check one." }
					}
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="import-network-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Import network" }
		},
	)
}

fn migrate_dialog(base: &str) -> Markup {
	let post = format!("@post({})", js_string(&format!("{base}/migrate")));
	dialog(
		MIGRATE_DIALOG,
		"Migrate this server's networks?",
		None,
		&html! {
			div class="flex flex-col gap-3" {
				p { "Moves every running app, database and model from the shared " code { "podkit" } " network onto its environment's own network, without restarting it. Compose stacks move on their next deploy." }
				p class="text-sm text-muted-foreground" { "Do this once after upgrading. Anything left behind moves on its next deploy anyway." }
				(form_error())
			}
		},
		&html! {
			form method="dialog" { button class=(BUTTON_OUTLINE) autofocus { "Cancel" } }
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Migrate networks" }
		},
	)
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Result<Markup, Response> {
	let found = servers::get(&s.db, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let server = &found.server;
	let views = service::list(&s, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_update = ctx.perms.can(Action::Update, Resource::Server);
	let suggestions: Vec<String> = if can_update {
		tokio::time::timeout(
			std::time::Duration::from_secs(3),
			service::scan(&s, ctx.user.id, ctx.team.id, id),
		)
		.await
		.ok()
		.and_then(Result::ok)
		.unwrap_or_default()
		.into_iter()
		.map(|n| n.name)
		.collect()
	} else {
		Vec::new()
	};
	let base = networks_url(ctx.team.id, id);
	let policy = crate::access_rules::policy::build(&s.db, ctx.team.id, id, views.clone())
		.await
		.map_err(|e| error::page(&ctx, &crate::error::ServerError::from(e)))?;

	let actions = html! {
		@if can_update {
			(dialog_open_button(MIGRATE_DIALOG, &button_class(Variant::Outline, Size::Md), &html! { (icon("refresh-cw")) "Migrate" }))
			(dialog_open_button(IMPORT_DIALOG, &button_class(Variant::Outline, Size::Md), &html! { (icon("download")) "Import" }))
			(dialog_open_button(NEW_DIALOG, &button_class(Variant::Default, Size::Md), &html! { (icon("plus")) "New network" }))
		}
	};
	let mut crumbs = server_pages::crumbs(&ctx);
	crumbs.push((
		server.name.as_str(),
		Some(server_pages::detail_url(ctx.team.id, id)),
	));
	crumbs.push(("Networks", None));
	let signals = json!({
		"net_name": "", "net_internal": false, "import_name": "", "_busy": false, "_error": "",
	});
	let body = html! {
		div class="ui-page-body" data-signals=(signals.to_string()) {
			(server_header(ctx.team.id, server, ServerTab::Networks, &actions))
			section class="ui-stack" aria-labelledby="networks-title" {
				div {
					h2 id="networks-title" class="ui-sec-h" { "Networks" }
					p class="ui-lede" {
						"Every environment gets a private network, so production and staging cannot reach each other. Create a network to connect resources on purpose."
					}
				}
				(policy::warnings(ctx.team.id, &policy.warnings))
				(list(&ctx, &base, &views, can_update))
			}
			(policy::section(ctx.team.id, &policy))
			@if can_update {
				(new_dialog(&base))
				(import_dialog(&base, &suggestions))
				(migrate_dialog(&base))
			}
		}
	};
	Ok(app_page(
		&ctx,
		&format!("{} · Networks", server.name),
		&crumbs,
		&body,
	))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NewSignals {
	net_name: String,
	net_internal: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ImportSignals {
	import_name: String,
}

/// The list after a change, and the dialogs closed with their fields cleared.
async fn refreshed(s: &AppState, ctx: &TeamCtx, server: ServerId) -> Result<sse::Events, Response> {
	let base = networks_url(ctx.team.id, server);
	let views = service::list(s, ctx.user.id, ctx.team.id, server)
		.await
		.map_err(|e| error::action(&e))?;
	let can_update = ctx.perms.can(Action::Update, Resource::Server);
	Ok(sse::patch(&list(ctx, &base, &views, can_update))
		.and(sse::signals(&json!({
			"net_name": "", "net_internal": false, "import_name": "", "_error": "",
		})))
		.and(sse::script(
			"document.querySelectorAll('dialog[open]').forEach((d) => d.close())",
		)))
}

fn finish(done: Result<sse::Events, Response>) -> Response {
	match done {
		Ok(events) => events.into_response(),
		Err(response) => response,
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<NewSignals>,
) -> Response {
	match service::create(
		&s,
		ctx.user.id,
		ctx.team.id,
		server,
		&sig.net_name,
		sig.net_internal,
	)
	.await
	{
		Ok(_) => finish(refreshed(&s, &ctx, server).await),
		Err(e) => error::action(&e),
	}
}

async fn import(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<ImportSignals>,
) -> Response {
	match service::import(&s, ctx.user.id, ctx.team.id, server, &sig.import_name, None).await {
		Ok(_) => finish(refreshed(&s, &ctx, server).await),
		Err(e) => error::action(&e),
	}
}

async fn delete(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server, id)): Path<(TeamId, ServerId, NetworkId)>,
) -> Response {
	match service::delete(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => finish(refreshed(&s, &ctx, server).await),
		Err(e) => error::action(&e),
	}
}

async fn migrate(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
) -> Response {
	match crate::networks::migrate::run(&s, ctx.user.id, ctx.team.id, server).await {
		Ok(report) => {
			let message = if report.redeploy.is_empty() {
				format!("Moved {} containers.", report.moved.len())
			} else {
				format!(
					"Moved {} containers; {} move on their next deploy.",
					report.moved.len(),
					report.redeploy.len()
				)
			};
			finish(
				refreshed(&s, &ctx, server)
					.await
					.map(|events| events.and(sse::toast(&message))),
			)
		}
		Err(e) => error::action(&e),
	}
}
