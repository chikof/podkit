//! The application's Terminal tab: a shell in its running container.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{AppPath, Tab, forbid_unless_allowed, load, page};
use crate::deployments::service;
use crate::state::AppState;
use crate::terminal::service::Subject;
use crate::ui::extract::TeamCtx;
use crate::ui::{error, pages::terminal};

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/terminal",
		get(terminal_page),
	)
}

async fn terminal_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Terminal)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let running = service::is_running(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let content = terminal::panel(
		&s,
		&ctx,
		project.id,
		&Subject::App(app.id),
		"application",
		&app.name,
		running,
	)
	.await;
	Ok(page(&s, &ctx, &project, &app, Tab::Terminal, &content).await)
}
