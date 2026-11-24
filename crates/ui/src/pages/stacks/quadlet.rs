//! The Quadlet tab: the units that would run every service of this stack under systemd,
//! read-only, ordered by `depends_on`. The body is `pages::quadlet::tab`, shared by every kind
//! of resource.

use axum::extract::{Path, State};
use axum::response::Response;
use maud::Markup;

use super::frame::{self, Tab};
use crate::error;
use crate::extract::TeamCtx;
use crate::pages::quadlet;
use podkit_core::auth::permission::Resource;
use podkit_core::error::ServerError;
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::state::AppState;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	if !quadlet::tab_allowed(&ctx, Resource::Stack) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let l = frame::load(&s, &ctx, project, id).await?;
	let view = podkit_core::quadlet::stack(&s, ctx.user.id, ctx.team.id, l.project.id, id).await;
	let content = quadlet::tab(&ctx, l.project.id, view)?;
	Ok(frame::page(&ctx, &l, Tab::Quadlet, &content))
}
