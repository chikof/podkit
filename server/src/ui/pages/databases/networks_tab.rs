//! The Networks tab: the networks this database is on, and attaching others. The panel and
//! its two actions are shared with the other resource pages (`pages::networks::panel`).

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, forbid_unless_allowed, load, page};
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::networks::ResourceKind;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::networks::panel;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/databases/{database_id}/networks",
		get(networks_page),
	)
}

async fn networks_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Networks)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let content = panel::content(
		&s,
		&ctx,
		project.id,
		ResourceKind::Database,
		d.database.id.0,
	)
	.await?;
	Ok(page(&s, &ctx, &project, &d, Tab::Networks, &content).await)
}
