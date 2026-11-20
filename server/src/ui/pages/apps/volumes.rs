//! The Volumes tab: persistent storage mounted into the container.
//!
//! Deleting a volume deletes its files, so each row has a confirmation dialog that says so
//! and starts on Cancel. A failure (the agent unreachable, the application still running) is
//! shown inside the dialog, which stays open, and the row stays.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{AppPath, Tab, app_url, load, page, saved_slot};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, ProjectId, TeamId, VolumeId};
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, Size, Variant, button_class, card, card_content, card_section, dialog,
	dialog_open_button, empty_state, icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::pages::volume_backups::{self as vb, Kind};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};
use crate::volume_backups::subject::Owner;
use crate::volumes::Volume;
use crate::volumes::service as volumes;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/volumes",
			get(volumes_page).post(volume_add),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/volumes/{volume_id}/delete",
			post(volume_remove),
		)
}

const ROW: &str = "ui-card-row";

fn delete_dialog(base: &str, volume: &Volume) -> Markup {
	let id = format!("volume-delete-{}", volume.id);
	let post = format!(
		"@post({})",
		js_string(&format!("{base}/volumes/{}/delete", volume.id))
	);
	dialog(
		&id,
		"Delete this volume?",
		None,
		&html! {
			div class="flex flex-col gap-3" {
				p {
					strong { "This permanently deletes all data in " (volume.mount_path) "." }
					" The files are removed from the server and can't be recovered. The application must be stopped first."
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" } }
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(post)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete volume and its data" }
		},
	)
}

/// Each row opens its own confirmation: the dialog names the path it deletes.
fn volume_list(base: &str, volumes: &[Volume], can_delete: bool) -> Markup {
	html! {
		div id="volume-list" {
			@if volumes.is_empty() {
				(empty_state(
					"hard-drive",
					"No volumes yet.",
					Some("Without one, files written by the application are lost on every deploy."),
					None,
				))
			} @else {
				ul class="ui-card-rows" aria-label="Volumes" {
					@for volume in volumes {
						li class=(ROW) {
							div class="flex min-w-0 flex-col" {
								code class="font-mono text-sm font-medium break-all" { (volume.mount_path) }
								span class="text-xs break-all text-muted-foreground" { (volume.name) }
							}
							@if can_delete {
								(dialog_open_button(
									&format!("volume-delete-{}", volume.id),
									&button_class(Variant::Ghost, Size::Icon),
									&html! { (icon("trash-2")) span class="sr-only" { "Delete " (volume.mount_path) } },
								))
							}
						}
					}
				}
				@if can_delete {
					@for volume in volumes { (delete_dialog(base, volume)) }
				}
			}
		}
	}
}

async fn volumes_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let list = volumes::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Volume);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);
	let backups = if list.is_empty() || !ctx.perms.can(Action::Read, Resource::Backup) {
		html! {}
	} else {
		let (views, destinations) = vb::views_for(&s, &ctx, project.id, Owner::App(app.id))
			.await
			.map_err(|e| error::page(&ctx, &e))?;
		vb::panel(
			&ctx,
			Kind::App,
			&format!("{base}/volumes"),
			&views,
			&destinations,
		)
	};

	let add = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/volumes"))
	);
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({ "volume_path": "", "_error": "" }).to_string()) {
			(saved_slot(&ctx, &base, false))
			(card(&html! {
				(card_section("Volumes", Some("Persistent storage mounted into the container. A volume keeps its files across deploys and is mounted from the next deploy on."), None))
				div class="ui-card-list" {
					div class="ui-card-slot" { (form_error()) }
					(volume_list(&base, &list, can_delete))
				}
			}))
			(backups)
			@if can_create {
				(card(&html! {
					(card_section("Add a volume", None, None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(add) {
							label class=(LABEL) {
								"Mount path"
								input class={ (INPUT) " font-mono" } type="text" required placeholder="/data" autocomplete="off" spellcheck="false" "data-bind:volume_path";
								span class="text-xs font-normal text-muted-foreground" { "An absolute path inside the container, such as /var/lib/app." }
							}
							div {
								button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add volume" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Volumes, &content).await)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VolumeSignals {
	volume_path: String,
}

async fn volume_add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<VolumeSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		volumes::create(
			&s.db,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.volume_path.trim(),
		)
		.await?;
		volumes::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);
			sse::patch(&volume_list(&base, &list, can_delete))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(&json!({ "volume_path": "", "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn volume_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, VolumeId)>,
) -> Response {
	let result = async {
		volumes::remove(&s, ctx.user.id, ctx.team.id, project, app, id).await?;
		volumes::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);
			sse::patch(&volume_list(&base, &list, can_delete))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		// The dialog stays open and shows the reason; the row stays.
		Err(e) => sse::signals(&json!({ "_error": error::message(&e) })).into_response(),
	}
}
