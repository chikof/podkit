//! The Security tab: what scanning found in the database's image. The page body is
//! `pages::security::content`, shared by every kind of resource.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::{Tab, database_url, forbid_unless_allowed, load, page};
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::scanning::Subject;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::networks::policy;
use crate::ui::pages::security::{self, TabQuery};

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	Router::new().route(&format!("{DB}/security"), get(security_page))
}

async fn security_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(query): Query<TabQuery>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Security)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let page_url = format!(
		"{}/security",
		database_url(ctx.team.id, project.id, d.database.id)
	);
	let content = security::content(
		&s,
		&ctx,
		project.id,
		Subject::Database(d.database.id),
		&page_url,
		&query,
	)
	.await?;
	let content = html! {
		(policy::database_warning(ctx.team.id, project.id.0, d.database.id.0, &d.database.name, d.database.public_port))
		(content)
	};
	Ok(page(&s, &ctx, &project, &d, Tab::Security, &content).await)
}
