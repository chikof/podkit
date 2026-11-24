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
use crate::components::{
	BUTTON_OUTLINE, INPUT, Size, Variant, button_class, card, card_content, card_section, icon,
};
use crate::extract::TeamCtx;
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::databases::EngineKind;
use podkit_core::databases::service::{self, ConnectionInfo};
use podkit_core::ids::{DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/connection"), get(connection_page))
		.route(&at("/connection/hide"), get(hide))
		.route(&at("/reveal"), post(reveal))
}

/// The password row before **Reveal**: bullets and the eye. `#db-secret` is what the reveal
/// and the hide patch.
fn masked(base: &str) -> Markup {
	let reveal = format!("@post({})", js_string(&format!("{base}/reveal")));
	html! {
		div id="db-secret" class="ui-field-group" {
			label for="db-password" class=(crate::components::LABEL) { "Password" }
			div class="ui-copy" {
				input id="db-password" class=(INPUT) type="text" readonly value="••••••••••••••••" tabindex="-1";
				button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(reveal)
					aria-label="Reveal password and URLs"
					"data-indicator:_revealing" "data-attr:disabled"="$_revealing" { (icon("eye")) }
			}
			p class="ui-error" role="alert" "data-show"="$_reveal_error" "data-text"="$_reveal_error" style="display: none" {}
		}
	}
}

/// The connection strings before **Reveal**: they carry the password, so none is written
/// into the page.
fn urls_masked() -> Markup {
	html! {
		div id="db-urls" class="ui-field-group" {
			input class=(INPUT) type="text" readonly value="••••••••••••••••••••••••" tabindex="-1" aria-label="Connection string (hidden)";
			p class="ui-hint" { "The connection strings carry the password, so they appear only after you reveal it." }
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
		div class="ui-two" {
			(card(&html! {
				(card_section("Credentials", None, Some(&html! { span class="ui-card-note" { "internal network" } })))
				(card_content(&html! {
					@if let Some(info) = &info {
						(internal_rows(info, keyed))
					}
					(masked(&base))
				}))
			}))
			div class="ui-stack" {
				(card(&html! {
					(card_section("Connection string", None, None))
					(card_content(&urls_masked()))
				}))
				@if let Some(info) = &info {
					(card(&html! {
						(card_section("External", None, None))
						(card_content(&html! {
							@if let Some((host, port)) = &info.public {
								(copy_row("Public address", &format!("{host}:{port}")))
							} @else {
								p class="ui-hint" {
									"Not published. Set a public port under General to reach it from outside the server."
								}
							}
						}))
					}))
				}
			}
		}
	};
	Ok(page(&s, &ctx, &project, &d, Tab::Connection, &content).await)
}

fn internal_rows(info: &ConnectionInfo, keyed: bool) -> Markup {
	html! {
		(copy_row("Host", &info.internal_host))
		div class="ui-pair" {
			(copy_row("Port", &info.port.to_string()))
			@if keyed {
				(copy_row("Database", &info.database))
			}
		}
		@if keyed {
			(copy_row("User", &info.username))
		}
	}
}

/// The masked rows again, after **Hide**.
async fn hide(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::find(&s.db, ctx.user.id, ctx.team.id, project, database).await {
		Ok(found) => sse::patch(&masked(&database_url(ctx.team.id, project, found.id)))
			.and(sse::patch(&urls_masked()))
			.and(sse::signals(&json!({ "_reveal_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

/// The password and URLs, patched into `#db-secret` and `#db-urls`. They exist in a page only
/// after this click, and the response may not be stored.
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
		div id="db-secret" class="ui-field-group" {
			(copy_row("Password", &connection.password))
			div {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(hide) { (icon("eye-off")) "Hide" }
			}
		}
	};
	let urls = html! {
		div id="db-urls" class="ui-field-group" {
			(copy_row("Internal URL", &connection.internal_url))
			@if let Some(public) = &connection.public_url {
				(copy_row("Public URL", public))
			}
		}
	};
	let mut response = sse::patch(&secret)
		.and(sse::patch(&urls))
		.and(sse::signals(&json!({ "_reveal_error": "" })))
		.into_response();
	response
		.headers_mut()
		.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	response
}
