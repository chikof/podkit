//! The Quadlet tab: the units that would run this application under systemd, read-only. The
//! body is `pages::quadlet::tab`, shared by every kind of resource.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{AppPath, Tab, forbid_unless_allowed, load, page};
use crate::state::AppState;
use crate::ui::extract::TeamCtx;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/quadlet",
		get(quadlet_page),
	)
}

async fn quadlet_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Quadlet)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let view = crate::quadlet::application(&s, ctx.user.id, ctx.team.id, project.id, app.id).await;
	let content = crate::ui::pages::quadlet::tab(&ctx, project.id, view)?;
	Ok(page(&s, &ctx, &project, &app, Tab::Quadlet, &content).await)
}
