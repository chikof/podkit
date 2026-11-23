//! The Cron tab: the commands scheduled to run inside this application's container. The
//! list and its form live in `pages::cron`; this is the tab around them.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, load, page};
use crate::cron::service;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::cron::tab_card;

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
