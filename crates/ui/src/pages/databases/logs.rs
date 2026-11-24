//! The Logs tab: the database engine's own output, streamed. A failure to start (no
//! container, the agent down) is written into the log itself rather than a 500.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::general::log_failure;
use super::{Tab, database_url, load, page};
use crate::components::{card, card_content, card_section, empty_state, log_viewer};
use crate::extract::TeamCtx;
use crate::live::container_log;
use podkit_core::databases::service;
use podkit_core::ids::{DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/logs",
			get(logs_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/logs/stream",
			get(logs_stream),
		)
}

async fn logs_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let (project, d) = load(&s, &ctx, project, database).await?;
	let base = database_url(ctx.team.id, project.id, d.database.id);
	let content = card(&html! {
		(card_section("Container log", Some("The database engine's own output, newest last."), None))
		(card_content(&html! {
			@if d.database.container_id.is_some() {
				(log_viewer("container-log", "Container log", &format!("{base}/logs/stream")))
			} @else {
				(empty_state(
					"scroll-text",
					"There is no container to read a log from.",
					Some("Start the database first."),
					None,
				))
			}
		}))
	});
	Ok(page(&s, &ctx, &project, &d, Tab::Logs, &content).await)
}

async fn logs_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::container_logs(&s, ctx.user.id, ctx.team.id, project, database, 300, true).await
	{
		Ok(events) => container_log(events, "container-log", s.shutdown.clone()),
		Err(e) => log_failure("container-log", &e),
	}
}
