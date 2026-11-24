//! The Volumes tab of a stack: the named volumes of its compose file, each with its
//! backups. The files themselves are never listed; what a volume holds is the services'
//! business. The cards and their actions are shared with the application's Volumes tab
//! (`pages::volume_backups`).

use axum::extract::{Path, State};
use axum::response::Response;
use maud::{Markup, html};

use super::frame::{self, Tab};
use crate::components::{card, card_section, empty_state};
use crate::error;
use crate::extract::TeamCtx;
use crate::pages::volume_backups::{self as backups, Kind};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::state::AppState;
use podkit_core::volume_backups::subject::Owner;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	if !ctx.perms.can(Action::Read, Resource::Backup) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let l = frame::load(&s, &ctx, project, id).await?;
	let (views, destinations) = backups::views_for(&s, &ctx, l.project.id, Owner::Stack(id))
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = format!("{}/volumes", l.base(ctx.team.id));
	let content = html! {
		@if views.is_empty() {
			(card(&html! {
				(card_section("Volumes", Some("The named volumes of the compose file, and their backups."), None))
				(empty_state(
					"hard-drive",
					"No volumes in this stack.",
					Some("A named volume in the compose file keeps its files across deploys, and shows up here to be backed up."),
					None,
				))
			}))
		} @else {
			(backups::panel(&ctx, Kind::Stack, &base, &views, &destinations))
		}
	};
	Ok(frame::page(&ctx, &l, Tab::Volumes, &content))
}
