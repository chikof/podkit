//! The database's Terminal tab: a shell in its running container.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, forbid_unless_allowed, load, page};
use crate::extract::TeamCtx;
use crate::pages::terminal;
use podkit_core::ids::{DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;
use podkit_core::terminal::service::Subject;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/databases/{database_id}/terminal",
		get(terminal_page),
	)
}

async fn terminal_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Terminal)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let content = terminal::panel(
		&s,
		&ctx,
		project.id,
		&Subject::Database(d.database.id),
		"database",
		&d.database.name,
		d.database.container_id.is_some(),
	)
	.await;
	Ok(page(&s, &ctx, &project, &d, Tab::Terminal, &content).await)
}
