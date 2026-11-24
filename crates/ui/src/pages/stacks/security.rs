//! The Security tab: what scanning found in the images of the stack's services, one scan per
//! service. The page body is `pages::security::content`, shared by every kind of resource.

use axum::extract::{Path, Query, State};
use axum::response::Response;
use maud::Markup;

use super::frame::{self, Tab};
use crate::extract::TeamCtx;
use crate::pages::security::{self, TabQuery};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::scanning::Subject;
use podkit_core::state::AppState;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(query): Query<TabQuery>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let page_url = format!("{}/security", l.base(ctx.team.id));
	let content = security::content(
		&s,
		&ctx,
		l.project.id,
		Subject::Stack(id),
		&page_url,
		&query,
	)
	.await?;
	Ok(frame::page(&ctx, &l, Tab::Security, &content))
}
