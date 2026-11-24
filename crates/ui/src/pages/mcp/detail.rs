//! A server's page.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::mcp_url;
use crate::components::{
	LogoSize, Size, Tone, Variant, badge, button_class, card, card_content, card_section,
	empty_state, link_button, log_viewer, logo, table,
};
use crate::extract::TeamCtx;
use crate::layout::app_page;
use crate::pages::apps::app_url;
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::mcp_servers::service::{self, Detail, Tools};
use podkit_core::mcp_servers::{self, KeyUsage, LinkedApp, Transport};
use podkit_core::projects;
use podkit_core::state::AppState;

type Ids = Path<(TeamId, ProjectId, ApplicationId)>;

const M: &str = "/teams/{team_id}/projects/{project_id}/mcp/{app_id}";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(M, get(page))
		.route(&format!("{M}/tools"), post(tools))
		.route(&format!("{M}/unlink"), post(unlink))
}

fn facts(rows: &[(&str, Markup)]) -> Markup {
	html! {
		dl class="ui-facts" {
			@for (label, value) in rows {
				div class="ui-fact" {
					dt { (label) }
					dd { (value) }
				}
			}
		}
	}
}

fn header(ctx: &TeamCtx, project_name: &str, detail: &Detail) -> Markup {
	let app = &detail.app;
	let manage = app_url(ctx.team.id, app.project_id, app.id);
	html! {
		header class="ui-page-head" {
			div class="ui-head-id" {
				(logo("mcp", LogoSize::Lg))
				div class="ui-page-title" {
					p class="ui-eyebrow" { "MCP server · " (project_name) }
					h1 { (app.name) }
					ul class="ui-chips" aria-label="MCP server details" {
						li {
							@if detail.running { (badge(Tone::Success, "Running")) }
							@else { (badge(Tone::Neutral, "Not running")) }
						}
						li { (badge(Tone::Neutral, detail.mcp.transport.as_str())) }
						li { (badge(Tone::Neutral, &app.server_name)) }
					}
				}
			}
			div class="ui-page-actions" {
				(link_button(Variant::Outline, Size::Md, &manage, &html! { "Deploys and settings" }))
			}
		}
	}
}

fn connection_card(detail: &Detail) -> Markup {
	let mcp = &detail.mcp;
	card(&html! {
		(card_section("Connection", Some("Linked applications reach the server through the gateway with a key of their own. It is never routed to the internet."), None))
		(card_content(&facts(&[
			("Gateway URL", html! { code { (mcp_servers::gateway_url(&detail.app.slug)) } }),
			("Transport", html! {
				@match mcp.transport {
					Transport::Http => { "Streamable HTTP, served by the image" },
					Transport::Stdio => { "Standard input and output, behind podkit's bridge" },
				}
			}),
			("Endpoint", html! { code { (mcp.path) } " on port " code { (detail.app.container_port) } }),
			("Command", html! {
				@if mcp.transport == Transport::Http { "Not used" }
				@else if !mcp.has_command() { "The image's own entrypoint and command" }
				// It may carry a token, so the page never prints it; the API reveals it.
				@else { code { (podkit_core::audit::MASK) } " set, hidden. Pass secrets as environment variables." }
			}),
		])))
	})
}

fn tool_list(tools: &Tools) -> Markup {
	html! {
		div id="mcp-tools" class="flex flex-col gap-3" {
			@if let Some(server) = &tools.server {
				p class="text-sm text-muted-foreground" { "Answered by " code { (server) } "." }
			}
			@if tools.tools.is_empty() {
				p class="text-sm text-muted-foreground" { "The server lists no tools." }
			} @else {
				ul class="flex flex-col" aria-label="Tools" {
					@for tool in &tools.tools {
						li class="flex flex-col gap-1 border-t border-border py-3 first:border-t-0 first:pt-0" {
							span class="font-mono text-sm font-semibold" { (tool.name) }
							@if let Some(description) = &tool.description {
								span class="text-sm text-muted-foreground" { (description) }
							}
							@if !tool.params.is_empty() {
								span class="flex flex-wrap gap-1" aria-label="Arguments" {
									@for param in &tool.params { (badge(Tone::Neutral, param)) }
								}
							}
						}
					}
				}
			}
		}
	}
}

fn tools_card(ctx: &TeamCtx, detail: &Detail, project: ProjectId) -> Markup {
	let can_list = ctx.perms.can(Action::Update, Resource::Application);
	let load = format!(
		"@post({})",
		js_string(&format!(
			"{}/tools",
			mcp_url(ctx.team.id, project, detail.app.id)
		))
	);
	card(&html! {
		(card_section("Tools", Some("Asked from the running server, through the gateway, when you press the button."), None))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				@if !detail.running {
					p class="text-sm text-muted-foreground" { "The server is not running. Deploy it to see its tools." }
				} @else if can_list {
					div {
						button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(load)
							"data-indicator:_loading" "data-attr:disabled"="$_loading" { "List tools" }
					}
				}
				div id="mcp-tools" {
					p class="text-sm text-muted-foreground" { "Not loaded yet." }
				}
			}
		}))
	})
}

fn links_card(ctx: &TeamCtx, detail: &Detail, project: ProjectId, linked: &[LinkedApp]) -> Markup {
	let can_unlink = ctx.perms.can(Action::Update, Resource::Application);
	let unlink = |app: ApplicationId| {
		format!(
			"@post({})",
			js_string(&format!(
				"{}/unlink?app={app}",
				mcp_url(ctx.team.id, project, detail.app.id)
			))
		)
	};
	html! {
		div id="mcp-links" {
			(card(&html! {
				(card_section("Linked applications", Some("Link an application from the MCP tab of its own page. It gets the URL and its key on its next deploy."), None))
				(card_content(&html! {
					@if linked.is_empty() {
						p class="text-sm text-muted-foreground" { "No application uses this server yet." }
					} @else {
						ul class="flex flex-col" aria-label="Linked applications" {
							@for app in linked {
								li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0 first:pt-0" {
									a class="font-medium hover:underline" href=(app_url(ctx.team.id, app.project_id, app.application_id)) { (app.name) }
									@if can_unlink {
										button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(unlink(app.application_id)) { "Unlink" }
									}
								}
							}
						}
					}
				}))
			}))
		}
	}
}

fn usage_card(rows: &[KeyUsage]) -> Markup {
	card(&html! {
		(card_section("Usage", Some("Requests the gateway forwarded in the last 7 days, per key."), None))
		(card_content(&if rows.is_empty() {
			empty_state(
				"activity",
				"Nothing has used this server in the last 7 days.",
				Some("Requests through the gateway show up here, per key."),
				None,
			)
		} else {
			let table_rows: Vec<Vec<Markup>> = rows
				.iter()
				.map(|r| {
					vec![
						html! { (r.key_label) },
						html! { (r.requests_24h) },
						html! { (r.requests_7d) },
					]
				})
				.collect();
			table("mcp-usage", "Usage by key", &["Key", "Requests 24 h", "Requests 7 d"], &table_rows)
		}))
	})
}

fn log_card(ctx: &TeamCtx, detail: &Detail, project: ProjectId) -> Markup {
	let stream = format!(
		"{}/logs/stream?tail=200&follow=1",
		app_url(ctx.team.id, project, detail.app.id)
	);
	card(&html! {
		(card_section("Log", Some("The server's own output."), None))
		(card_content(&if detail.running {
			log_viewer("runtime-log", "Server log", &stream)
		} else {
			empty_state(
				"scroll-text",
				"The server is not running.",
				Some("Deploy it and its output shows up here."),
				None,
			)
		}))
	})
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Ids,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let detail = service::get(&s, ctx.user.id, ctx.team.id, project.id, id)
		.await
		.map_err(fail)?;
	let linked = service::for_server(&s, ctx.user.id, ctx.team.id, project.id, id)
		.await
		.map_err(fail)?;
	let usage = service::usage(&s, ctx.user.id, ctx.team.id, project.id, id)
		.await
		.map_err(fail)?;
	let body = html! {
		div class="ui-page-body" data-signals="{_loading: false, _error: ''}" {
			(header(&ctx, &project.name, &detail))
			(connection_card(&detail))
			(tools_card(&ctx, &detail, project.id))
			(links_card(&ctx, &detail, project.id, &linked))
			(usage_card(&usage))
			(log_card(&ctx, &detail, project.id))
		}
	};
	let team = ctx.team.id;
	Ok(app_page(
		&ctx,
		&detail.app.name,
		&[
			("Projects", Some(format!("/teams/{team}/projects"))),
			(
				project.name.as_str(),
				Some(format!("/teams/{team}/projects/{}", project.id)),
			),
			(detail.app.name.as_str(), None),
		],
		&body,
	))
}

/// Lists the tools and patches them in. A server that does not answer is a message, not a
/// page.
async fn tools(State(s): State<AppState>, ctx: TeamCtx, Path((_, project, id)): Ids) -> Response {
	match service::tools(&s, ctx.user.id, ctx.team.id, project, id).await {
		Ok(tools) => sse::patch(&tool_list(&tools)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct AppQuery {
	app: String,
}

async fn unlink(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Ids,
	Query(q): Query<AppQuery>,
) -> Response {
	let result = async {
		let app = q
			.app
			.parse::<i64>()
			.map(ApplicationId)
			.map_err(|_| ServerError::Validation("Choose an application.".to_owned()))?;
		service::unlink(&s, ctx.user.id, ctx.team.id, project, app, id).await?;
		let detail = service::get(&s, ctx.user.id, ctx.team.id, project, id).await?;
		let linked = service::for_server(&s, ctx.user.id, ctx.team.id, project, id).await?;
		Ok::<_, ServerError>((detail, linked))
	}
	.await;
	match result {
		Ok((detail, linked)) => sse::patch(&links_card(&ctx, &detail, project, &linked))
			.and(sse::toast(
				"Unlinked. The running application keeps its variables until its next deploy.",
			))
			.into_response(),
		Err(e) => error::action(&e),
	}
}
