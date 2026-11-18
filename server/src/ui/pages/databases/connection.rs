//! The Connection tab: how to reach the database from inside podkit and from outside, with a
//! copy button on every value. The password and the URLs that carry it exist only in the
//! response to **Reveal**: no page, signal or stored response has them, and a page loaded
//! after a reveal is as clean as before.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde_json::json;

use super::{Tab, copy_row, database_url, forbid_unless_allowed, load, page};
use crate::databases::EngineKind;
use crate::databases::service::{self, ConnectionInfo};
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{BUTTON_OUTLINE, INPUT, card, card_content, card_section, icon};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/connection"), get(connection_page))
		.route(&at("/connection/hide"), get(hide))
		.route(&at("/reveal"), post(reveal))
}

/// The password row before **Reveal**: bullets and the button. `#db-secret` is what the
/// reveal and the hide patch.
fn masked(base: &str) -> Markup {
	let reveal = format!("@post({})", js_string(&format!("{base}/reveal")));
	html! {
		div id="db-secret" class="flex flex-col gap-3" {
			div class="flex flex-col gap-2 text-sm font-medium" {
				span { "Password" }
				div class="flex gap-2" {
					input class={ (INPUT) " font-mono" } type="text" readonly value="••••••••••••••••" aria-label="Password (hidden)" tabindex="-1";
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(reveal)
						"data-indicator:_revealing" "data-attr:disabled"="$_revealing" { (icon("eye")) "Reveal password and URLs" }
				}
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_reveal_error" "data-text"="$_reveal_error" style="display: none" {}
		}
	}
}

async fn connection_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Connection)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let info = service::connection_info(&s, ctx.user.id, ctx.team.id, project.id, d.database.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = database_url(ctx.team.id, project.id, d.database.id);
	let keyed = d.database.engine != EngineKind::Redis;

	let content = html! {
		div class="flex flex-col gap-6" {
			@if let Some(info) = &info {
				(card(&html! {
					(card_section("Internal", Some("How your applications reach this database inside podkit."), None))
					(card_content(&html! { (internal_rows(info, keyed)) }))
				}))
				(card(&html! {
					(card_section("External", Some("From outside the server, through the public port."), None))
					(card_content(&html! {
						@if let Some((host, port)) = &info.public {
							(copy_row("Public address", &format!("{host}:{port}")))
						} @else {
							p class="text-sm text-muted-foreground" {
								"Not published. Set a public port under General to reach it from outside the server."
							}
						}
					}))
				}))
			}
			(card(&html! {
				(card_section("Credentials", Some("The password is shown only after you ask for it, and is not kept in the page."), None))
				(card_content(&masked(&base)))
			}))
		}
	};
	Ok(page(&ctx, &project, &d, Tab::Connection, &content))
}

fn internal_rows(info: &ConnectionInfo, keyed: bool) -> Markup {
	html! {
		div class="flex flex-col gap-4" {
			(copy_row("Host", &info.internal_host))
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
				(copy_row("Port", &info.port.to_string()))
				@if keyed {
					(copy_row("User", &info.username))
					(copy_row("Database", &info.database))
				}
			}
		}
	}
}

/// The masked row again, after **Hide**.
async fn hide(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::find(&s.db, ctx.user.id, ctx.team.id, project, database).await {
		Ok(found) => sse::patch(&masked(&database_url(ctx.team.id, project, found.id)))
			.and(sse::signals(&json!({ "_reveal_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

/// The password and URLs, patched into `#db-secret`. They exist in a page only after this
/// click, and the response may not be stored.
async fn reveal(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	let connection = match service::reveal(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(connection) => connection,
		Err(e) => {
			return sse::signals(&json!({ "_reveal_error": error::message(&e) })).into_response();
		}
	};
	let hide = format!(
		"@get({})",
		js_string(&format!(
			"{}/connection/hide",
			database_url(ctx.team.id, project, database)
		))
	);
	let secret = html! {
		div id="db-secret" class="flex flex-col gap-4" {
			(copy_row("Password", &connection.password))
			(copy_row("Internal URL", &connection.internal_url))
			@if let Some(public) = &connection.public_url {
				(copy_row("Public URL", public))
			}
			div {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(hide) { (icon("eye-off")) "Hide" }
			}
		}
	};
	let mut response = sse::patch(&secret)
		.and(sse::signals(&json!({ "_reveal_error": "" })))
		.into_response();
	response
		.headers_mut()
		.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	response
}
