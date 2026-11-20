//! The Networks tab: the networks this stack is on besides its own, and attaching others.
//! A stack keeps its own private network; it joins its environment's network or a user
//! network only when attached to it. The panel and its two actions are shared with the
//! other resource pages (`pages::networks::panel`).

use axum::extract::{Path, State};
use axum::response::Response;
use maud::Markup;

use super::frame::{self, Tab};
use crate::ids::{ProjectId, StackId, TeamId};
use crate::networks::ResourceKind;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::networks::panel;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let content = panel::content(&s, &ctx, l.project.id, ResourceKind::Stack, id.0).await?;
	Ok(frame::page(&ctx, &l, Tab::Networks, &content))
}
