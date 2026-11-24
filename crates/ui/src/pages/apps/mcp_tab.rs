//! The MCP tab: the MCP servers this application reaches through the gateway, and the dialog
//! that links another.
//!
//! A link has a key of its own; the application gets `MCP_<NAME>_URL`, `MCP_<NAME>_KEY` and
//! `PODKIT_MCP_SERVERS` on its next deploy. Nothing here ever shows a key.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use super::{AppPath, Tab, app_url, load, page};
use crate::components::{
	LABEL, SELECT, Size, Tone, Variant, badge, button_class, card, card_content, card_section,
	dialog, dialog_cancel, dialog_open_button, empty_state, link_button,
};
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::pages::mcp::{mcp_url, new_url};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, ProjectId};
use podkit_core::mcp_servers;
use podkit_core::mcp_servers::service::{self, AppLinks};
use podkit_core::state::AppState;

const DIALOG: &str = "mcp-link-dialog";

pub fn router() -> Router<AppState> {
	const A: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}/mcp";
	Router::new()
		.route(A, get(mcp_page))
		.route(&format!("{A}/link"), post(link))
		.route(&format!("{A}/unlink"), post(unlink))
}

fn links_region(ctx: &TeamCtx, project: ProjectId, app: ApplicationId, links: &AppLinks) -> Markup {
	let base = format!("{}/mcp", app_url(ctx.team.id, project, app));
	let can_link = ctx.perms.can(Action::Update, Resource::Application);
	let unlink = |server: ApplicationId| {
		format!(
			"@post({})",
			js_string(&format!("{base}/unlink?server={server}"))
		)
	};
	let post_link = format!("@post({})", js_string(&format!("{base}/link")));
	let create = ctx
		.perms
		.can(Action::Create, Resource::Application)
		.then(|| new_url(ctx.team.id, project));
	let open = (can_link && !links.linkable.is_empty()).then(|| {
		dialog_open_button(
			DIALOG,
			&button_class(Variant::Default, Size::Md),
			&html! { "Link an MCP server" },
		)
	});
	html! {
		div id="mcp-links" {
			(card(&html! {
				(card_section(
					"MCP servers",
					Some("A linked server is reached through the gateway with a key of its own for this application. Changes apply on the next deploy."),
					open.as_ref(),
				))
				(card_content(&html! {
					@if links.linked.is_empty() {
						(empty_state(
							"plug",
							"No MCP server is linked to this application.",
							Some("Link one that runs on the same server, or make one first. A template is the quickest way."),
							create.as_ref().map(|href| link_button(Variant::Outline, Size::Md, href, &html! { "New MCP server" })).as_ref(),
						))
					} @else {
						ul class="flex flex-col" aria-label="Linked MCP servers" {
							@for link in &links.linked {
								li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 first:border-t-0 first:pt-0" {
									div class="flex min-w-0 flex-col gap-0.5" {
										a class="truncate text-sm font-medium hover:underline" href=(mcp_url(ctx.team.id, link.project_id, link.mcp_application_id)) { (link.name) }
										span class="truncate font-mono text-xs text-muted-foreground" {
											(mcp_servers::env_name(&link.slug)) "_URL, " (mcp_servers::env_name(&link.slug)) "_KEY"
										}
									}
									div class="flex flex-wrap items-center gap-3" {
										(badge(Tone::Neutral, link.transport.as_str()))
										@if can_link {
											button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(unlink(link.mcp_application_id)) { "Unlink" }
										}
									}
								}
							}
						}
					}
				}))
			}))
			@if can_link && !links.linkable.is_empty() {
				(dialog(
					DIALOG,
					"Link an MCP server",
					Some("The application gets the server's gateway URL and a key made for this link."),
					&html! {
						div class="flex flex-col gap-3" {
							label class=(LABEL) {
								"MCP server"
								select class=(SELECT) required "data-bind"="mcpServer" autofocus {
									option value="" { "Choose…" }
									@for (id, name) in &links.linkable { option value=(id.0) { (name) } }
								}
							}
							p class="text-sm text-muted-foreground" {
								"Only servers on this application's own server are listed: the gateway reaches no others."
							}
							(form_error())
						}
					},
					&html! {
						(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
						button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post_link)
							"data-indicator:_busy" "data-attr:disabled"="$_busy || !$mcpServer" disabled { "Link" }
					},
				))
			}
		}
	}
}

async fn load_links(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<AppLinks, ServerError> {
	service::for_app(s, ctx.user.id, ctx.team.id, project, app).await
}

async fn mcp_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
) -> Result<Markup, Response> {
	super::forbid_unless_allowed(&ctx, Tab::Mcp)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let links = load_links(&s, &ctx, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let content = html! {
		div class="contents" data-signals="{mcpServer: '', _error: '', _busy: false}" {
			(links_region(&ctx, project.id, app.id, &links))
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Mcp, &content).await)
}

#[derive(Deserialize)]
struct LinkSignals {
	#[serde(rename = "mcpServer")]
	server: String,
}

fn parse_server(s: &str) -> Result<ApplicationId, ServerError> {
	s.trim()
		.parse::<i64>()
		.map(ApplicationId)
		.map_err(|_| ServerError::Validation("Choose an MCP server.".to_owned()))
}

/// Re-renders the region after a change, and says what happened.
async fn region_response(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
	toast: &str,
) -> Response {
	match load_links(s, ctx, project, app).await {
		Ok(links) => sse::patch(&links_region(ctx, project, app, &links))
			.and(sse::signals(
				&serde_json::json!({ "mcpServer": "", "_error": "" }),
			))
			.and(sse::toast(toast))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn link(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
	ReadSignals(sig): ReadSignals<LinkSignals>,
) -> Response {
	let result = async {
		let server = parse_server(&sig.server)?;
		service::link(&s, ctx.user.id, ctx.team.id, project, app, server).await
	}
	.await;
	match result {
		Ok(_) => {
			region_response(
				&s,
				&ctx,
				project,
				app,
				"Linked. Redeploy the application to give it the new variables.",
			)
			.await
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct ServerQuery {
	server: String,
}

async fn unlink(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
	Query(q): Query<ServerQuery>,
) -> Response {
	let result = async {
		let server = parse_server(&q.server)?;
		service::unlink(&s, ctx.user.id, ctx.team.id, project, app, server).await
	}
	.await;
	match result {
		Ok(()) => {
			region_response(
				&s,
				&ctx,
				project,
				app,
				"Unlinked. The running application keeps its variables until its next deploy.",
			)
			.await
		}
		Err(e) => error::action(&e),
	}
}
