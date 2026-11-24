//! The Networks tab: the networks this application is on, and attaching others. The panel
//! and its two actions are shared with the other resource pages (`pages::networks::panel`).

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::Markup;

use super::{Tab, load, page};
use crate::extract::TeamCtx;
use crate::pages::networks::panel;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::networks::ResourceKind;
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/networks",
		get(networks_page),
	)
}

async fn networks_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	super::forbid_unless_allowed(&ctx, Tab::Networks)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let content = panel::content(&s, &ctx, project.id, ResourceKind::Application, app.id.0).await?;
	Ok(page(&s, &ctx, &project, &app, Tab::Networks, &content).await)
}
