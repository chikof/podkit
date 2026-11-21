//! The Security tab: what scanning found in the application's image. The page body is
//! `pages::security::content`, shared by every kind of resource.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::{Tab, access, app_url, forbid_unless_allowed, load, page};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::scanning::Subject;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::security::{self, TabQuery};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	Router::new().route(&format!("{APP}/security"), get(security_page))
}

async fn security_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<TabQuery>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Security)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let page_url = format!("{}/security", app_url(ctx.team.id, project.id, app.id));
	let content = security::content(
		&s,
		&ctx,
		project.id,
		Subject::Application(app.id),
		&page_url,
		&query,
	)
	.await?;
	// What Traefik does to requests, for someone who may see the domains.
	let rules = if ctx.perms.can(Action::Read, Resource::Domain) {
		crate::access_rules::service::get(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
			.await
			.ok()
	} else {
		None
	};
	let base = app_url(ctx.team.id, project.id, app.id);
	let content = html! {
		@if let Some(rules) = &rules { (access::security_card(&base, rules)) }
		(content)
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Security, &content).await)
}
