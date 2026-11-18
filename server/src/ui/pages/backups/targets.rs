//! Backup pages: the team's backup targets, and the Backups tab of a
//! database (history, back up now, restore, delete).
//!
//! A target's secret key is write-only: the form posts it once, and no page,
//! signal or response ever carries it back. A restore overwrites the live
//! data, so the service refuses it unless the database's name is typed, and
//! the page only offers the button.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::backups::service::{self as backups, Entry, Overview};
use crate::backups::targets::service::{self as targets, CreateInput};
use crate::backups::{BackupStatus, BackupTarget};
use crate::error::ServerError;
use crate::ids::{BackupId, BackupTargetId, DatabaseId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, card, card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::pages::projects::databases::{Tab, database_url, load, shell, when};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/backup-targets",
			get(targets_page).post(create_target),
		)
		.route(
			"/teams/{team_id}/backup-targets/{target_id}/delete",
			post(remove_target),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/backups",
			get(backups_page).post(back_up_now),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/backups/list",
			get(backups_poll),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/backups/{backup_id}/restore",
			post(restore),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/backups/{backup_id}/delete",
			post(remove_backup),
		)
}

/// `1.5 MB`: binary multiples, one decimal above a kilobyte.
#[allow(clippy::cast_precision_loss)]
fn human_size(bytes: i64) -> String {
	const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
	if bytes < 1024 {
		return format!("{} B", bytes.max(0));
	}
	let mut value = bytes as f64 / 1024.0;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	format!("{value:.1} {}", UNITS[unit])
}

// --- targets ------------------------------------------------------------------

fn targets_url(team: TeamId) -> String {
	format!("/teams/{team}/backup-targets")
}

fn delete_target_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
	}
}

fn target_row(ctx: &TeamCtx, target: &BackupTarget, can_delete: bool) -> Markup {
	let delete = format!(
		"confirm('Delete this backup target? Databases using it stop backing up, and backups already made to it can no longer be restored.') && @post({})",
		js_string(&format!(
			"{}/{}/delete",
			targets_url(ctx.team.id),
			target.id.0
		))
	);
	html! {
		li class="flex flex-wrap items-start justify-between gap-3 border-t border-border py-4 text-sm first:border-t-0" {
			div class="flex min-w-0 flex-col gap-1" {
				span class="font-medium break-words" { (target.name) }
				span class="text-muted-foreground break-all" { (target.endpoint) }
				span class="text-muted-foreground break-all" {
					"Bucket " strong class="font-medium text-foreground" { (target.bucket) }
					" · region " (target.region)
					@if !target.prefix.is_empty() { " · prefix " (target.prefix) }
					@if target.path_style { " · path-style" }
				}
				span class="text-muted-foreground break-all" { "Access key " (target.access_key_id) }
			}
			@if can_delete {
				button type="button" class=(BUTTON_DANGER) "data-on:click"=(delete) { "Delete" }
			}
		}
	}
}

async fn targets_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let rows = targets::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Backup);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&targets_url(ctx.team.id))
	);
	let signals = json!({
		"name": "", "endpoint": "", "region": "us-east-1", "bucket": "", "prefix": "",
		"path_style": false, "access_key_id": "", "secret_access_key": "",
		"_error": "", "_delete_error": "",
	})
	.to_string();

	let body = html! {
		div class="flex flex-col gap-6" "data-signals"=(signals) {
			(card(&html! {
				(card_header("Backup targets", Some("S3-compatible buckets your team's databases are backed up to.")))
				(card_content(&html! {
					div class="flex flex-col gap-3" {
						@if rows.is_empty() {
							p class="text-sm text-muted-foreground" { "No backup targets yet." }
						} @else {
							ul {
								@for target in &rows { (target_row(&ctx, target, can_delete)) }
							}
						}
						(delete_target_error())
					}
				}))
			}))
			@if can_create {
				(card(&html! {
					(card_header("Add a backup target", Some("The secret key is stored encrypted and never shown again.")))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(post) {
							label class=(LABEL) {
								"Name"
								input class=(INPUT) type="text" required maxlength="60" placeholder="offsite" "data-bind:name";
							}
							label class=(LABEL) {
								"Endpoint"
								input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" placeholder="https://s3.eu-west-1.amazonaws.com" "data-bind:endpoint";
								span class="text-sm font-normal text-muted-foreground" { "Use https. Plain http is only accepted for localhost." }
							}
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Bucket"
									input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:bucket";
								}
								label class=(LABEL) {
									"Region"
									input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:region";
								}
							}
							label class=(LABEL) {
								"Prefix (optional)"
								input class=(INPUT) type="text" autocomplete="off" spellcheck="false" placeholder="podkit" "data-bind:prefix";
							}
							label class="flex cursor-pointer items-center gap-2 text-sm" {
								input type="checkbox" "data-bind:path_style";
								"Path-style addressing (MinIO and most self-hosted S3)"
							}
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Access key ID"
									input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:access_key_id";
								}
								label class=(LABEL) {
									"Secret access key"
									input class=(INPUT) type="password" required autocomplete="new-password" spellcheck="false" "data-bind:secret_access_key";
								}
							}
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add backup target" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Backup targets",
		&[("Backup targets", None)],
		&body,
	))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TargetSignals {
	name: String,
	endpoint: String,
	region: String,
	bucket: String,
	prefix: String,
	path_style: bool,
	access_key_id: String,
	secret_access_key: String,
}

async fn create_target(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<TargetSignals>,
) -> Response {
	let input = CreateInput {
		name: sig.name.trim().to_owned(),
		endpoint: sig.endpoint,
		region: sig.region.trim().to_owned(),
		bucket: sig.bucket.trim().to_owned(),
		path_style: sig.path_style,
		prefix: sig.prefix.trim().to_owned(),
		access_key_id: sig.access_key_id.trim().to_owned(),
		secret_access_key: sig.secret_access_key,
	};
	match targets::create(&s, ctx.user.id, ctx.team.id, input).await {
		Ok(_) => sse::redirect(&targets_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove_target(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, target)): Path<(TeamId, BackupTargetId)>,
) -> Response {
	match targets::remove(&s.db, ctx.user.id, ctx.team.id, target).await {
		Ok(_) => sse::redirect(&targets_url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}

// --- a database's backups -----------------------------------------------------

fn backups_url(ctx: &TeamCtx, project: ProjectId, database: DatabaseId) -> String {
	format!("{}/backups", database_url(ctx.team.id, project, database))
}

fn entry_row(ctx: &TeamCtx, base: &str, entry: &Entry) -> Markup {
	let backup = &entry.backup;
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let can_restore = ctx.perms.can(Action::Update, Resource::Database)
		&& backup.status == BackupStatus::Succeeded
		&& backup.target_id.is_some();
	let can_remove = can_delete && !backup.status.in_flight();
	let by = match (&entry.triggered_by, backup.triggered_by) {
		(Some(name), _) => name.clone(),
		(None, Some(_)) => "A removed user".to_owned(),
		(None, None) => "Schedule".to_owned(),
	};
	let select = format!(
		"$restore_id = '{}'; $confirm = ''; $_restore_label = {}",
		backup.id.0,
		js_string(&when(backup.created_at))
	);
	let delete = format!(
		"confirm('Delete this backup? Its file is removed from the bucket.') && @post({})",
		js_string(&format!("{base}/{}/delete", backup.id.0))
	);
	html! {
		li class="flex flex-col gap-2 border-t border-border py-4 text-sm first:border-t-0" {
			div class="flex flex-wrap items-center justify-between gap-3" {
				div class="flex flex-wrap items-center gap-3" {
					(status_badge(backup.status))
					@if let Some(size) = backup.size_bytes {
						span class="font-medium" { (human_size(size)) }
					}
				}
				@if can_restore || can_remove {
					div class="flex flex-wrap gap-2" {
						@if can_restore {
							button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(select) { "Restore" }
						}
						@if can_remove {
							button type="button" class=(BUTTON_DANGER) "data-on:click"=(delete) { "Delete" }
						}
					}
				}
			}
			div class="flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground" {
				span { "Started " (when(backup.created_at)) }
				@if let Some(finished) = backup.finished_at {
					span { "Finished " (when(finished)) }
				}
				span { "By " (by) }
			}
			@if let Some(message) = &backup.error {
				p class="text-destructive break-words" { (message) }
			}
			@if backup.status == BackupStatus::Succeeded && backup.target_id.is_none() {
				p class="text-muted-foreground" { "Its backup target was deleted, so this backup can't be restored." }
			}
		}
	}
}

/// The history, re-fetched every few seconds while a backup is queued or
/// running.
fn history(ctx: &TeamCtx, base: &str, o: &Overview) -> Markup {
	let in_flight = o.entries.iter().any(|e| e.backup.status.in_flight());
	html! {
		div id="backup-list" class="flex flex-col" {
			@if o.entries.is_empty() {
				p class="text-sm text-muted-foreground" { "No backups yet." }
			} @else {
				ul {
					@for entry in &o.entries { (entry_row(ctx, base, entry)) }
				}
			}
			(poll(&format!("{base}/list"), in_flight))
		}
	}
}

fn restore_panel(base: &str, name: &str) -> Markup {
	let action = format!(
		"@post({} + $restore_id + '/restore')",
		js_string(&format!("{base}/"))
	);
	html! {
		div class="flex flex-col gap-3 rounded-lg border border-destructive/30 bg-destructive/10 p-4" "data-show"="$restore_id !== ''" style="display: none" {
			p class="text-sm font-medium text-destructive" {
				"Restoring the backup from " span "data-text"="$_restore_label" {}
				" overwrites the current data of this database."
			}
			p class="text-sm" {
				"Everything written since that backup is lost. Type "
				strong class="break-all" { (name) }
				" to confirm."
			}
			input class=(INPUT) type="text" autocomplete="off" spellcheck="false" aria-label="Database name" "data-bind:confirm";
			div class="flex flex-wrap gap-3" {
				button type="button" class=(BUTTON_DANGER) "data-on:click"=(action) "data-indicator:_restoring" "data-attr:disabled"="$_restoring || $confirm === ''" { "Restore and overwrite" }
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$restore_id = ''; $confirm = ''" { "Cancel" }
			}
		}
	}
}

async fn overview_of(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Overview, ServerError> {
	backups::overview(&s.db, ctx.user.id, ctx.team.id, project, database).await
}

async fn backups_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let (project, d) = load(&s, &ctx, project, database).await?;
	let o = overview_of(&s, &ctx, project.id, d.database.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = backups_url(&ctx, project.id, d.database.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Backup);
	let can_restore = ctx.perms.can(Action::Update, Resource::Database);
	let start = format!("@post({})", js_string(&base));
	let settings = format!(
		"{}/settings",
		database_url(ctx.team.id, project.id, d.database.id)
	);

	let content = html! {
		div class="flex flex-col gap-6" "data-signals"="{_error: '', restore_id: '', confirm: '', _restore_label: ''}" {
			(card(&html! {
				(card_header("Backups", Some("Dumps of this database in the backup target. Set the target and schedule under Settings.")))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						@if can_create {
							@match &o.can_start {
								Ok(()) => div {
									button type="button" class=(BUTTON) "data-on:click"=(start) "data-indicator:_starting" "data-attr:disabled"="$_starting" { "Back up now" }
								},
								Err(why) => p class="text-sm text-muted-foreground" {
									(why) ". "
									@if ctx.perms.can(Action::Update, Resource::Database) {
										a class="underline" href=(settings) { "Open settings" } "."
									}
								},
							}
						}
						(form_error())
						(history(&ctx, &base, &o))
						@if can_restore { (restore_panel(&base, &d.database.name)) }
					}
				}))
			}))
		}
	};
	Ok(shell(&ctx, &project, &d, Tab::Backups, &content))
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Markup, ServerError> {
	let o = overview_of(s, ctx, project, database).await?;
	Ok(history(ctx, &backups_url(ctx, project, database), &o))
}

async fn backups_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match refreshed(&s, &ctx, project, database).await {
		Ok(list) => sse::patch(&list).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn back_up_now(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	let result = async {
		backups::create(&s, ctx.user.id, ctx.team.id, project, database).await?;
		refreshed(&s, &ctx, project, database).await
	}
	.await;
	match result {
		Ok(list) => sse::patch(&list)
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestoreSignals {
	confirm: String,
}

async fn restore(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database, backup)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
	ReadSignals(sig): ReadSignals<RestoreSignals>,
) -> Response {
	let result = backups::restore_confirmed(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		database,
		backup,
		&sig.confirm,
	)
	.await;
	match result {
		Ok(_) => sse::signals(&json!({ "_error": "", "restore_id": "", "confirm": "" }))
			.and(sse::toast(
				"Restore queued. The database is overwritten when it runs.",
			))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove_backup(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database, backup)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
) -> Response {
	let result = async {
		backups::remove(&s, ctx.user.id, ctx.team.id, project, database, backup).await?;
		refreshed(&s, &ctx, project, database).await
	}
	.await;
	match result {
		Ok(list) => sse::patch(&list)
			.and(sse::signals(&json!({ "_error": "" })))
			.and(sse::toast("Backup deleted"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use axum::Router;
	use axum::http::StatusCode;
	use serde_json::{Value, json};

	use super::human_size;
	use crate::tests::dbs::{
		add_backup, add_backup_target, backup_count, job_kinds, local_server, set_backup_target,
		set_state, stored_secret, target_count, user_id,
	};
	use crate::tests::{PgPool, app, call, db_test, user_with_team};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login,
	};

	const SECRET: &str = "s3cr3t-Access-Key-4711";

	struct Fixture {
		app: Router,
		token: String,
		cookie: String,
		team: i64,
		project: i64,
		database: i64,
		target: i64,
	}

	impl Fixture {
		/// Alice, her team, a project, a running database "orders" with a
		/// backup target set.
		async fn new(db: &PgPool) -> Self {
			let app = app(db.clone()).await;
			let (token, team) = user_with_team(&app, "alice").await;
			let cookie = ui_login(&app, "alice").await;
			let (s, project) = call(
				&app,
				"POST",
				&format!("/api/v1/teams/{team}/projects"),
				Some(&token),
				Some(json!({ "name": "Shop", "slug": "shop" })),
			)
			.await;
			assert!(s.is_success(), "create project: {s}");
			let project = project["id"].as_i64().unwrap();
			let server = local_server(db, team).await;
			let (s, database) = call(
				&app,
				"POST",
				&format!("/api/v1/teams/{team}/projects/{project}/databases"),
				Some(&token),
				Some(json!({ "name": "orders", "engine": "postgres", "server_id": server })),
			)
			.await;
			assert!(s.is_success(), "create database: {s} {database}");
			let database = database["id"].as_i64().unwrap();
			set_state(db, database, "running", Some("c1")).await;
			let target = add_backup_target(db, 900, team, "offsite").await;
			set_backup_target(db, database, target).await;
			Self {
				app,
				token,
				cookie,
				team,
				project,
				database,
				target,
			}
		}

		fn targets(&self) -> String {
			format!("/teams/{}/backup-targets", self.team)
		}

		fn base(&self) -> String {
			format!(
				"/teams/{}/projects/{}/databases/{}/backups",
				self.team, self.project, self.database
			)
		}

		fn api(&self) -> String {
			format!(
				"/api/v1/teams/{}/projects/{}/databases/{}/backups",
				self.team, self.project, self.database
			)
		}

		async fn get(&self, uri: &str) -> (StatusCode, String) {
			let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
			(s, body)
		}

		async fn post(&self, uri: &str, signals: &Value) -> String {
			post_signals(&self.app, uri, Some(&self.cookie), signals)
				.await
				.2
		}

		/// A member with the built-in read-only role, signed in.
		async fn reader(&self, name: &str) -> String {
			user_with_team(&self.app, name).await;
			let (s, _) = call(
				&self.app,
				"POST",
				&format!("/api/v1/teams/{}/members", self.team),
				Some(&self.token),
				Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
			)
			.await;
			assert!(s.is_success(), "add member: {s}");
			ui_login(&self.app, name).await
		}
	}

	#[allow(clippy::needless_pass_by_value)]
	fn form(extra: Value) -> Value {
		let mut form = json!({
			"name": "offsite", "endpoint": "https://s3.example.com", "region": "eu-west-1",
			"bucket": "podkit", "prefix": "", "path_style": false,
			"access_key_id": "AKIA1234", "secret_access_key": SECRET,
		});
		for (k, v) in extra.as_object().unwrap() {
			form[k] = v.clone();
		}
		form
	}

	#[test]
	fn sizes_read_as_people_write_them() {
		for (bytes, text) in [
			(0, "0 B"),
			(1023, "1023 B"),
			(1024, "1.0 KB"),
			(1536 * 1024, "1.5 MB"),
			(5 * 1024 * 1024 * 1024, "5.0 GB"),
			(3 * 1024_i64.pow(4), "3.0 TB"),
			(2048 * 1024_i64.pow(4), "2048.0 TB"),
		] {
			assert_eq!(human_size(bytes), text, "{bytes}");
		}
	}

	#[db_test]
	async fn targets_are_listed_added_and_never_show_the_secret(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (s, page) = f.get(&f.targets()).await;
		assert_eq!(s, StatusCode::OK);
		for text in [
			"offsite",
			"https://s3.example.com",
			"Bucket",
			"us-east-1",
			"Add a backup target",
		] {
			assert_contains(&page, text);
		}

		let body = f
			.post(
				&f.targets(),
				&form(json!({ "name": "second", "prefix": "podkit/prod", "path_style": true })),
			)
			.await;
		assert_contains(&body, &format!("window.location.assign('{}')", f.targets()));
		assert_not_contains(&body, SECRET);

		let (_, page) = f.get(&f.targets()).await;
		for text in [
			"second",
			"AKIA1234",
			"eu-west-1",
			"podkit",
			"podkit/prod",
			"path-style",
		] {
			assert_contains(&page, text);
		}
		assert_not_contains(&page, SECRET);
		// The page has the form's secret field empty and nothing after it.
		assert_not_contains(&page, "secret_access_key\":\"s");
		// Stored encrypted, not as typed.
		assert!(!String::from_utf8_lossy(&stored_secret(&db).await).contains(SECRET));

		let (s, list) = call(
			&f.app,
			"GET",
			&format!("/api/v1/teams/{}/backup-targets", f.team),
			Some(&f.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(list.as_array().unwrap().len(), 2);
		assert_not_contains(&list.to_string(), SECRET);
	}

	#[db_test]
	async fn bad_target_input_is_an_inline_message_and_adds_nothing(db: PgPool) {
		let f = Fixture::new(&db).await;
		for (extra, message) in [
			(
				json!({ "name": "x", "endpoint": "http://minio.example.com" }),
				"endpoint must use https",
			),
			(
				json!({ "name": "x", "endpoint": "nope" }),
				"invalid endpoint",
			),
			(
				json!({ "name": "x", "secret_access_key": "" }),
				"invalid secret_access_key",
			),
			(json!({ "name": "x", "prefix": "../up" }), "invalid prefix"),
			(json!({ "name": "offsite" }), "already exists"),
		] {
			let body = f.post(&f.targets(), &form(extra)).await;
			assert_contains(&body, message);
			assert_not_contains(&body, "window.location");
			assert_not_contains(&body, SECRET);
		}
		assert_eq!(target_count(&db).await, 1);
	}

	#[db_test]
	async fn deleting_a_target_works_and_another_teams_target_is_untouchable(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
		let theirs = add_backup_target(&db, 901, mallory_team, "theirs").await;

		let body = f
			.post(&format!("{}/{theirs}/delete", f.targets()), &json!({}))
			.await;
		assert_contains(&body, "_delete_error");
		assert_contains(&body, "That no longer exists.");
		assert_eq!(target_count(&db).await, 2);

		let body = f
			.post(&format!("{}/{}/delete", f.targets(), f.target), &json!({}))
			.await;
		assert_contains(&body, &format!("window.location.assign('{}')", f.targets()));
		assert_eq!(target_count(&db).await, 1);
	}

	#[db_test]
	async fn targets_of_another_team_are_a_404_page(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
		add_backup_target(&db, 901, mallory_team, "theirs").await;

		let (s, page) = f
			.get(&format!("/teams/{mallory_team}/backup-targets"))
			.await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert_not_contains(&page, "theirs");
		let body = f
			.post(
				&format!("/teams/{mallory_team}/backup-targets"),
				&form(json!({ "name": "evil" })),
			)
			.await;
		assert_not_contains(&body, "window.location");
		assert_eq!(target_count(&db).await, 2);
	}

	#[db_test]
	async fn a_target_name_with_a_script_is_escaped(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup_target(&db, 902, f.team, "<script>alert(1)</script>").await;
		let (_, page) = f.get(&f.targets()).await;
		assert_not_contains(&page, "<script>alert(1)");
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}

	#[db_test]
	async fn a_read_only_member_sees_targets_but_cannot_change_them(db: PgPool) {
		let f = Fixture::new(&db).await;
		let cookie = f.reader("rita").await;

		let (s, _, page) = get_html(&f.app, &f.targets(), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "offsite");
		assert_not_contains(&page, "Add backup target");
		assert_not_contains(&page, "Delete");

		let body = post_signals(
			&f.app,
			&f.targets(),
			Some(&cookie),
			&form(json!({ "name": "forged" })),
		)
		.await
		.2;
		assert_contains(&body, "You don't have permission to do that.");
		let body = post_signals(
			&f.app,
			&format!("{}/{}/delete", f.targets(), f.target),
			Some(&cookie),
			&json!({}),
		)
		.await
		.2;
		assert_contains(&body, "You don't have permission to do that.");
		assert_eq!(target_count(&db).await, 1);
	}

	#[db_test]
	async fn the_tab_lists_backups_with_size_who_and_the_error(db: PgPool) {
		let f = Fixture::new(&db).await;
		let alice = user_id(&db, "alice").await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"succeeded",
			Some(1536 * 1024),
			None,
			Some(alice),
		)
		.await;
		add_backup(
			&db,
			11,
			f.database,
			Some(f.target),
			"failed",
			None,
			Some("bucket <b>gone</b>"),
			None,
		)
		.await;

		let (s, page) = f.get(&f.base()).await;
		assert_eq!(s, StatusCode::OK);
		for text in [
			"Backups",
			"Succeeded",
			"1.5 MB",
			"Failed",
			"By alice",
			"By Schedule",
			"Back up now",
			"Finished",
		] {
			assert_contains(&page, text);
		}
		assert_contains(&page, "bucket &lt;b&gt;gone&lt;/b&gt;");
		// All finished: no polling.
		assert_not_contains(&page, "data-on-interval");

		add_backup(
			&db,
			12,
			f.database,
			Some(f.target),
			"running",
			None,
			None,
			Some(alice),
		)
		.await;
		let (_, page) = f.get(&f.base()).await;
		assert_contains(&page, "data-on-interval__duration.3s");
		assert_contains(&page, &format!("{}/list", f.base()));
		let (_, _, patch) = get_html(&f.app, &format!("{}/list", f.base()), Some(&f.cookie)).await;
		assert_contains(&patch, "Running");
		assert_contains(&patch, "data-on-interval");
	}

	#[db_test]
	async fn back_up_now_queues_a_backup_and_the_list_starts_polling(db: PgPool) {
		let f = Fixture::new(&db).await;
		let body = f.post(&f.base(), &json!({})).await;
		assert_contains(&body, "Queued");
		assert_contains(&body, "data-on-interval");
		assert_eq!(backup_count(&db, f.database).await, 1);

		let (s, list) = call(&f.app, "GET", &f.api(), Some(&f.token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(list[0]["status"], "queued");
		assert!(list[0]["triggered_by"].is_i64());
	}

	#[db_test]
	async fn without_a_target_there_is_no_button_and_a_forged_post_is_refused(db: PgPool) {
		let f = Fixture::new(&db).await;
		crate::tests::dbs::clear_backup_target(&db, f.database).await;
		let (_, page) = f.get(&f.base()).await;
		assert_not_contains(&page, "Back up now");
		assert_contains(&page, "configure a backup target first");

		let body = f.post(&f.base(), &json!({})).await;
		assert_contains(&body, "configure a backup target first");
		assert_eq!(backup_count(&db, f.database).await, 0);
	}

	#[db_test]
	async fn a_restore_needs_the_typed_name_even_when_the_post_is_forged(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"succeeded",
			Some(10),
			None,
			None,
		)
		.await;
		let (_, page) = f.get(&f.base()).await;
		assert_contains(&page, "overwrites the current data");
		assert_contains(&page, "<strong class=\"break-all\">orders</strong>");
		assert_contains(&page, "Restore and overwrite");

		let uri = format!("{}/10/restore", f.base());
		for signals in [
			json!({}),
			json!({ "confirm": "" }),
			json!({ "confirm": "order" }),
			json!({ "confirm": "orders " }),
			json!({ "confirm": "ORDERS" }),
		] {
			let body = f.post(&uri, &signals).await;
			assert_contains(&body, "The name doesn't match.");
			assert_not_contains(&body, "Restore queued");
		}
		assert!(job_kinds(&db, 10).await.is_empty());

		let body = f.post(&uri, &json!({ "confirm": "orders" })).await;
		assert_contains(&body, "Restore queued");
		assert_eq!(job_kinds(&db, 10).await, ["restore"]);
	}

	#[db_test]
	async fn a_restore_of_an_unsuitable_backup_is_refused(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"failed",
			None,
			Some("x"),
			None,
		)
		.await;
		add_backup(
			&db,
			11,
			f.database,
			Some(f.target),
			"succeeded",
			Some(1),
			None,
			None,
		)
		.await;
		let confirm = json!({ "confirm": "orders" });

		let body = f.post(&format!("{}/10/restore", f.base()), &confirm).await;
		assert_contains(&body, "only a succeeded backup can be restored");
		let body = f.post(&format!("{}/99/restore", f.base()), &confirm).await;
		assert_contains(&body, "That no longer exists.");

		set_state(&db, f.database, "stopped", None).await;
		let body = f.post(&format!("{}/11/restore", f.base()), &confirm).await;
		assert_contains(&body, "the database must be running");
		assert!(job_kinds(&db, 10).await.is_empty() && job_kinds(&db, 11).await.is_empty());
	}

	#[db_test]
	async fn deleting_a_backup_removes_it_unless_it_is_in_progress(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"failed",
			None,
			Some("x"),
			None,
		)
		.await;
		add_backup(
			&db,
			11,
			f.database,
			Some(f.target),
			"running",
			None,
			None,
			None,
		)
		.await;

		let body = f.post(&format!("{}/11/delete", f.base()), &json!({})).await;
		assert_contains(&body, "backup is in progress");
		assert_eq!(backup_count(&db, f.database).await, 2);

		let body = f.post(&format!("{}/10/delete", f.base()), &json!({})).await;
		assert_contains(&body, "Backup deleted");
		assert_eq!(backup_count(&db, f.database).await, 1);
		assert_not_contains(&body, "Failed");
	}

	#[db_test]
	async fn another_teams_database_and_backups_are_not_found(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"succeeded",
			Some(1),
			None,
			None,
		)
		.await;
		let (mtoken, mteam) = user_with_team(&f.app, "mallory").await;
		let mcookie = ui_login(&f.app, "mallory").await;
		let (_, mproject) = call(
			&f.app,
			"POST",
			&format!("/api/v1/teams/{mteam}/projects"),
			Some(&mtoken),
			Some(json!({ "name": "Theirs", "slug": "theirs" })),
		)
		.await;
		let mproject = mproject["id"].as_i64().unwrap();

		// Her database through his team and project: the 404 page.
		let theirs = format!(
			"/teams/{mteam}/projects/{mproject}/databases/{}/backups",
			f.database
		);
		let (s, _, page) = get_html(&f.app, &theirs, Some(&mcookie)).await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert_not_contains(&page, "orders");
		// His own team id with her database's path: also not found.
		let (s, page) = f
			.get(&format!(
				"/teams/{}/projects/{mproject}/databases/{}/backups",
				f.team, f.database
			))
			.await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert_not_contains(&page, "orders");
		// And her team is closed to him.
		let (s, _, _) = get_html(&f.app, &f.base(), Some(&mcookie)).await;
		assert_eq!(s, StatusCode::NOT_FOUND);

		for action in ["", "/10/restore", "/10/delete"] {
			let body = post_signals(
				&f.app,
				&format!("{theirs}{action}"),
				Some(&mcookie),
				&json!({ "confirm": "orders" }),
			)
			.await
			.2;
			assert_not_contains(&body, "Restore queued");
			assert_not_contains(&body, "Queued");
		}
		assert_eq!(backup_count(&db, f.database).await, 1);
		assert!(job_kinds(&db, 10).await.is_empty());
	}

	#[db_test]
	async fn a_read_only_member_sees_history_but_no_actions(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"succeeded",
			Some(1),
			None,
			None,
		)
		.await;
		let cookie = f.reader("rita").await;

		let (s, _, page) = get_html(&f.app, &f.base(), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Succeeded");
		for hidden in [
			"Back up now",
			"Restore and overwrite",
			">Delete<",
			">Restore<",
		] {
			assert_not_contains(&page, hidden);
		}

		let confirm = json!({ "confirm": "orders" });
		for (uri, signals) in [
			(f.base(), json!({})),
			(format!("{}/10/restore", f.base()), confirm),
			(format!("{}/10/delete", f.base()), json!({})),
		] {
			let body = post_signals(&f.app, &uri, Some(&cookie), &signals).await.2;
			assert_contains(&body, "You don't have permission to do that.");
		}
		assert_eq!(backup_count(&db, f.database).await, 1);
		assert!(job_kinds(&db, 10).await.is_empty());
	}

	#[db_test]
	async fn the_settings_target_select_links_to_adding_one(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (_, page) = f
			.get(&format!(
				"/teams/{}/projects/{}/databases/{}/settings",
				f.team, f.project, f.database
			))
			.await;
		assert_contains(&page, "Add a backup target");
		assert_contains(&page, &format!("href=\"{}\"", f.targets()));
	}

	#[db_test]
	async fn the_json_api_keeps_its_behaviour(db: PgPool) {
		let f = Fixture::new(&db).await;
		// Restore over JSON has never asked for a typed name.
		add_backup(
			&db,
			10,
			f.database,
			Some(f.target),
			"succeeded",
			Some(1),
			None,
			None,
		)
		.await;
		let (s, _) = call(
			&f.app,
			"POST",
			&format!("{}/10/restore", f.api()),
			Some(&f.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED);
		assert_eq!(job_kinds(&db, 10).await, ["restore"]);

		let (s, created) = call(&f.app, "POST", &f.api(), Some(&f.token), None).await;
		assert_eq!(s, StatusCode::ACCEPTED);
		assert_eq!(created["status"], "queued");
		let (s, list) = call(&f.app, "GET", &f.api(), Some(&f.token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(list.as_array().unwrap().len(), 2);
		let (s, _) = call(
			&f.app,
			"DELETE",
			&format!("{}/10", f.api()),
			Some(&f.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		let (s, _) = call(
			&f.app,
			"DELETE",
			&format!("{}/10", f.api()),
			Some(&f.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND);
	}
}
