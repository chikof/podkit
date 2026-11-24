//! The Quadlet tab: the units that would run this database under systemd, read-only. The
//! engine's credentials are in the env file the unit names, never on the page. The body is
//! `pages::quadlet::tab`, shared by every kind of resource.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, forbid_unless_allowed, load, page};
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::extract::TeamCtx;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/databases/{database_id}/quadlet",
		get(quadlet_page),
	)
}

async fn quadlet_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Quadlet)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let view =
		crate::quadlet::database(&s, ctx.user.id, ctx.team.id, project.id, d.database.id).await;
	let content = crate::ui::pages::quadlet::tab(&ctx, project.id, view)?;
	Ok(page(&s, &ctx, &project, &d, Tab::Quadlet, &content).await)
}
