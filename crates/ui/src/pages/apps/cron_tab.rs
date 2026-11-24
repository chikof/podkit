//! The Cron tab: the commands scheduled to run inside this application's container. The
//! list and its form live in `pages::cron`; this is the tab around them.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, load, page};
use crate::error;
use crate::extract::TeamCtx;
use crate::pages::cron::tab_card;
use podkit_core::cron::service;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/cron",
		get(cron_page),
	)
}

async fn cron_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	super::forbid_unless_allowed(&ctx, Tab::Cron)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let tasks = service::list(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project.id,
		None,
		Some(app.id),
	)
	.await
	.map_err(|e| error::page(&ctx, &e))?;
	let content = tab_card(&ctx, project.id, &app, &tasks);
	Ok(page(&s, &ctx, &project, &app, Tab::Cron, &content).await)
}
