//! The Quadlet tab: the units that would run every service of this stack under systemd,
//! read-only, ordered by `depends_on`. The body is `pages::quadlet::tab`, shared by every kind
//! of resource.

use axum::extract::{Path, State};
use axum::response::Response;
use maud::Markup;

use super::frame::{self, Tab};
use crate::auth::permission::Resource;
use crate::error::ServerError;
use crate::ids::{ProjectId, StackId, TeamId};
use crate::state::AppState;
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::quadlet;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	if !quadlet::tab_allowed(&ctx, Resource::Stack) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let l = frame::load(&s, &ctx, project, id).await?;
	let view = crate::quadlet::stack(&s, ctx.user.id, ctx.team.id, l.project.id, id).await;
	let content = quadlet::tab(&ctx, l.project.id, view)?;
	Ok(frame::page(&ctx, &l, Tab::Quadlet, &content))
}
