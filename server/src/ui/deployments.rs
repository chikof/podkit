//! Deployment pages: the Deploy, Restart and Stop buttons, the deployment
//! history with rollback and cancel, one deployment's live build log, and
//! the running container's logs.
//!
//! Every action comes back as an SSE stream. A refusal ("not running", "no
//! previous build") lands in the `_action_error` signal, which the buttons in
//! the app header display, so no page needs its own error line.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use super::apps::{Tab, app_url, load, shell};
use super::components::{BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, card, card_content, card_header};
use super::extract::TeamCtx;
use super::live::{container_log, job_log, log_view};
use super::status::{Status, poll, status_badge};
use super::{error, sse};
use crate::applications::service::ApplicationView;
use crate::auth::permission::{Action, Resource};
use crate::deployments::service::{self, DeploymentView, Trigger};
use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use crate::state::AppState;

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

/// The tail lengths the Logs tab offers.
const TAILS: [u32; 3] = [200, 1000, 5000];

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at("/deploy"), post(deploy))
		.route(&at("/restart"), post(restart))
		.route(&at("/stop"), post(stop))
		.route(&at("/deployments"), get(history))
		.route(&at("/deployments/table"), get(table_fragment))
		.route(&at("/deployments/{deployment_id}"), get(detail))
		.route(
			&at("/deployments/{deployment_id}/status"),
			get(status_fragment),
		)
		.route(&at("/deployments/{deployment_id}/log"), get(build_log))
		.route(&at("/deployments/{deployment_id}/rollback"), post(rollback))
		.route(&at("/deployments/{deployment_id}/cancel"), post(cancel))
		.route(&at("/logs"), get(logs_page))
		.route(&at("/logs/stream"), get(logs_stream))
}

fn deployment_url(base: &str, id: DeploymentId) -> String {
	format!("{base}/deployments/{id}")
}

/// A signal update that shows `msg` in the header's action error line.
fn action_error(msg: &str) -> sse::Events {
	sse::signals(&json!({ "_action_error": msg }))
}

fn fail(e: &ServerError) -> Response {
	action_error(&error::message(e)).into_response()
}

// --- header actions ---------------------------------------------------------

/// Deploy, Restart and Stop for the app header, each shown only to someone
/// who may use it and enabled only while it can work. Refusals the server
/// still gives (a stale page, say) show in the error line.
pub fn actions(ctx: &TeamCtx, app: &ApplicationView) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_stop = ctx.perms.can(Action::Update, Resource::Deployment);
	let busy = app.latest.is_some_and(Status::in_flight);
	let running = app.latest == Some(crate::deployments::DeploymentStatus::Running);
	html! {
		div id="app-actions" class="flex flex-col gap-2" "data-signals"="{_action_error: '', _acting: false}" {
			@if can_create || can_stop {
				div class="flex flex-wrap items-center gap-2" {
					@if can_create {
						button type="button" class=(BUTTON) "data-on:click"={ "@post('" (base) "/deploy')" } "data-indicator:_acting" "data-attr:disabled"="$_acting" { "Deploy" }
						@if busy || app.latest.is_none() {
							button type="button" class=(BUTTON_OUTLINE) disabled { "Restart" }
						} @else {
							button type="button" class=(BUTTON_OUTLINE) "data-on:click"={ "@post('" (base) "/restart')" } "data-attr:disabled"="$_acting" { "Restart" }
						}
					}
					@if can_stop {
						@if running {
							button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (base) "/stop')" } "data-attr:disabled"="$_acting" { "Stop" }
						} @else {
							button type="button" class=(BUTTON_DANGER) disabled { "Stop" }
						}
					}
				}
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		}
	}
}

async fn deploy(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match service::deploy(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn restart(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match service::restart(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn stop(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	let result = async {
		service::stop(&s, ctx.user.id, ctx.team.id, project, app).await?;
		crate::applications::service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(view) => sse::patch(&actions(&ctx, &view))
			.and(sse::toast("Stopped"))
			.into_response(),
		Err(e) => fail(&e),
	}
}

// --- history ---------------------------------------------------------------

fn two(n: u8) -> String {
	format!("{n:02}")
}

/// `2026-11-17 20:03 UTC`: no locale, no JS, same everywhere.
fn when(t: OffsetDateTime) -> String {
	let t = t.to_offset(time::UtcOffset::UTC);
	format!(
		"{}-{}-{} {}:{} UTC",
		t.date().year(),
		two(t.month() as u8),
		two(t.day()),
		two(t.hour()),
		two(t.minute()),
	)
}

fn duration(d: time::Duration) -> String {
	let secs = d.whole_seconds().max(0);
	match (secs / 3600, secs / 60 % 60, secs % 60) {
		(0, 0, s) => format!("{s}s"),
		(0, m, s) => format!("{m}m {s}s"),
		(h, m, _) => format!("{h}h {m}m"),
	}
}

fn short_sha(sha: &str) -> &str {
	sha.get(..7).unwrap_or(sha)
}

fn trigger_label(t: &Trigger) -> &'static str {
	match t {
		Trigger::User => "User",
		Trigger::Webhook => "Webhook",
	}
}

const TH: &str = "py-2 pr-4 text-xs font-medium text-muted-foreground";
const TD: &str = "py-3 pr-4 align-middle whitespace-nowrap";

/// The history table. `#deployments` is what polling and actions re-render;
/// the polling element is part of it, so it disappears once nothing is in
/// flight.
fn table(ctx: &TeamCtx, app: &ApplicationView, rows: &[DeploymentView]) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	let active = rows.iter().any(DeploymentView::in_flight);
	html! {
		div id="deployments" {
			(poll(&format!("{base}/deployments/table"), active))
			@if rows.is_empty() {
				p class="text-sm text-muted-foreground" { "Nothing has been deployed yet." }
			} @else {
				div class="overflow-x-auto" {
					table class="w-full text-left text-sm" {
						thead {
							tr {
								th class=(TH) { "Status" }
								th class=(TH) { "Trigger" }
								th class=(TH) { "Ref" }
								th class=(TH) { "Created" }
								th class=(TH) { "Duration" }
								th class=(TH) {}
							}
						}
						tbody {
							@for d in rows {
								tr class="border-t border-border" {
									td class=(TD) {
										a class="hover:underline" href=(deployment_url(&base, d.id)) { (status_badge(d.status)) }
									}
									td class=(TD) { (trigger_label(&d.trigger)) }
									td class=(TD) {
										@if let Some(sha) = &d.commit_sha { code class="font-mono text-xs" { (short_sha(sha)) } } @else { span class="text-muted-foreground" { "-" } }
									}
									td class=(TD) { (when(d.created_at)) }
									td class=(TD) {
										@if let Some(t) = d.duration() { (duration(t)) } @else { span class="text-muted-foreground" { "-" } }
									}
									td class="py-3 text-right align-middle whitespace-nowrap" {
										div class="flex justify-end gap-2" {
											@if can_update && d.can_cancel() {
												button type="button" class=(BUTTON_OUTLINE) "data-on:click"={ "@post('" (deployment_url(&base, d.id)) "/cancel')" } { "Cancel" }
											}
											@if can_create && d.can_rollback() {
												(rollback_buttons(&base, d.id))
											}
										}
									}
								}
							}
						}
					}
				}
			}
		}
	}
}

/// Rollback asks once more before it deploys anything: the first click only
/// swaps the button for Confirm and Keep.
fn rollback_buttons(base: &str, id: DeploymentId) -> Markup {
	html! {
		button type="button" class=(BUTTON_OUTLINE) "data-show"={ "$_rollback !== '" (id.0) "'" } "data-on:click"={ "$_rollback = '" (id.0) "'" } { "Rollback" }
		div class="flex items-center gap-2" "data-show"={ "$_rollback === '" (id.0) "'" } style="display: none" {
			button type="button" class=(BUTTON) "data-on:click"={ "$_rollback = ''; @post('" (deployment_url(base, id)) "/rollback')" } { "Confirm rollback" }
			button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$_rollback = ''" { "Keep" }
		}
	}
}

async fn history_rows(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<Vec<DeploymentView>, ServerError> {
	service::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
}

async fn history(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let rows = history_rows(&s, &ctx, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let content = card(&html! {
		(card_header("Deployments", Some("Every attempt to build and run this application, newest first.")))
		(card_content(&html! {
			div data-signals="{_rollback: ''}" { (table(&ctx, &app, &rows)) }
		}))
	});
	Ok(shell(&s, &ctx, &project, &app, Tab::Deployments, &content))
}

/// What polling fetches: the table again, with its polling element only
/// while something is still in flight.
async fn table_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	let result = async {
		let view = crate::applications::service::get(&s.db, ctx.user.id, ctx.team.id, project, app)
			.await?;
		let rows = history_rows(&s, &ctx, project, app).await?;
		Ok::<_, ServerError>(table(&ctx, &view, &rows))
	}
	.await;
	match result {
		Ok(markup) => sse::patch(&markup).into_response(),
		Err(e) => fail(&e),
	}
}

async fn rollback(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	let result = async {
		// The same rule the buttons follow, so a forged post for a running,
		// in-flight or imageless deployment is refused with a reason.
		let target = service::get(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		if !target.has_image {
			return Err(ServerError::Validation(
				"That deployment never produced an image to roll back to.".to_owned(),
			));
		}
		if !target.can_rollback() {
			return Err(ServerError::Validation(
				"That deployment is live or still in progress, so there is nothing to roll back to."
					.to_owned(),
			));
		}
		service::rollback(&s, ctx.user.id, ctx.team.id, project, app, id).await
	}
	.await;
	match result {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn cancel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	match service::cancel(&s, ctx.user.id, ctx.team.id, project, app, id).await {
		// The worker moves the row to cancelled a moment later; polling shows it.
		Ok(_) => sse::toast("Cancelling")
			.and(action_error(""))
			.into_response(),
		Err(e) => fail(&e),
	}
}

// --- one deployment --------------------------------------------------------

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

/// Status, times and the Cancel button. `#deployment-status` is what polling
/// re-renders while the deployment is in flight.
fn status_card(ctx: &TeamCtx, app: &ApplicationView, d: &DeploymentView) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let url = deployment_url(&base, d.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	html! {
		div id="deployment-status" {
			(poll(&format!("{url}/status"), d.in_flight()))
			(card(&html! {
				(card_header(&format!("Deployment #{}", d.id), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
							(fact("Status", &status_badge(d.status)))
							(fact("Trigger", &html! { (trigger_label(&d.trigger)) }))
							(fact("Commit", &html! {
								@if let Some(sha) = &d.commit_sha { code class="font-mono text-xs" { (short_sha(sha)) } } @else { "Not cloned yet" }
							}))
							(fact("Created", &html! { (when(d.created_at)) }))
							(fact("Started", &html! {
								@if let Some(t) = d.started_at { (when(t)) } @else { "-" }
							}))
							(fact("Duration", &html! {
								@if let Some(t) = d.duration() { (duration(t)) } @else { "-" }
							}))
						}
						@if let Some(message) = &d.error_message {
							p class="rounded-lg border border-destructive/30 bg-destructive/15 p-3 text-sm break-words text-destructive" role="alert" { (message) }
						}
						@if can_update && d.can_cancel() {
							div {
								button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (url) "/cancel')" } { "Cancel deployment" }
							}
						}
					}
				}))
			}))
		}
	}
}

async fn detail(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let d = service::get(&s.db, ctx.user.id, ctx.team.id, project.id, app.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let url = deployment_url(&app_url(ctx.team.id, project.id, app.id), d.id);
	let content = html! {
		(status_card(&ctx, &app, &d))
		(card(&html! {
			(card_header("Build log", Some("Clone, build and start output.")))
			(card_content(&log_view("build-log", &format!("{url}/log"))))
		}))
	};
	Ok(shell(&s, &ctx, &project, &app, Tab::Deployments, &content))
}

async fn status_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	let result = async {
		let view = crate::applications::service::get(&s.db, ctx.user.id, ctx.team.id, project, app)
			.await?;
		let d = service::get(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		Ok::<_, ServerError>((view, d))
	}
	.await;
	match result {
		// The header's buttons depend on the latest status too.
		Ok((view, d)) => sse::patch(&status_card(&ctx, &view, &d))
			.and(sse::patch(&actions(&ctx, &view)))
			.into_response(),
		Err(e) => fail(&e),
	}
}

/// Why a stream could not start. Not found and not allowed are plain status
/// codes, like any other page; anything else (nothing running, the agent
/// down) is written into the log itself.
fn stream_failure(e: &ServerError, target: &str) -> Response {
	let status = e.status();
	if status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN {
		return status.into_response();
	}
	let line = html! { div class="break-all whitespace-pre-wrap text-destructive" { (error::message(e)) } };
	sse::patch_into(&format!("#{target}"), ElementPatchMode::Append, &line)
		.and(sse::signals(&json!({ "_live": false })))
		.into_response()
}

async fn build_log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	match service::build_log_job(&s.db, ctx.user.id, ctx.team.id, project, app, id).await {
		Ok(job) => job_log(&s, job, "build-log"),
		Err(e) => stream_failure(&e, "build-log"),
	}
}

// --- runtime logs ----------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct LogParams {
	tail: Option<u32>,
	follow: Option<String>,
}

impl LogParams {
	fn tail(&self) -> u32 {
		self.tail.filter(|t| TAILS.contains(t)).unwrap_or(TAILS[0])
	}

	/// Following is the default; only `follow=0` turns it off.
	fn follow(&self) -> bool {
		self.follow.as_deref() != Some("0")
	}
}

async fn logs_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<LogParams>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let running = service::is_running(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let (tail, follow) = (params.tail(), params.follow());
	let content = card(&html! {
		(card_header("Logs", Some("Output of the running container.")))
		(card_content(&html! {
			@if running {
				div class="flex flex-col gap-4" {
					form class="flex flex-wrap items-end gap-3" method="get" action={ (base) "/logs" } {
						label class="flex flex-col gap-1 text-sm font-medium" {
							"Lines"
							select class="h-9 rounded-lg border border-border bg-transparent px-3 text-sm font-normal" name="tail" "data-on:change"="el.form.requestSubmit()" {
								@for t in TAILS {
									option value=(t) selected[t == tail] { "Last " (t) }
								}
							}
						}
						label class="flex flex-col gap-1 text-sm font-medium" {
							"Follow"
							select class="h-9 rounded-lg border border-border bg-transparent px-3 text-sm font-normal" name="follow" "data-on:change"="el.form.requestSubmit()" {
								option value="1" selected[follow] { "Stream new lines" }
								option value="0" selected[!follow] { "Snapshot only" }
							}
						}
						button type="submit" class=(BUTTON_OUTLINE) { "Apply" }
					}
					(log_view("runtime-log", &format!("{base}/logs/stream?tail={tail}&follow={}", u8::from(follow))))
				}
			} @else {
				div class="flex flex-col gap-2 py-6 text-center" {
					p class="font-medium" { "This application isn't running." }
					p class="text-sm text-muted-foreground" { "Deploy it, or restart it from its last build, and its output shows up here." }
				}
			}
		}))
	});
	Ok(shell(&s, &ctx, &project, &app, Tab::Logs, &content))
}

async fn logs_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<LogParams>,
) -> Response {
	match service::runtime_log(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		app,
		params.tail(),
		params.follow(),
	)
	.await
	{
		Ok(events) => container_log(events, "runtime-log", s.shutdown.clone()),
		Err(e) => stream_failure(&e, "runtime-log"),
	}
}

#[cfg(test)]
mod tests {
	use axum::Router;
	use axum::http::StatusCode;
	use serde_json::json;

	use super::*;
	use crate::ids::JobId;
	use crate::tests::{
		PgPool, application_id, call, count, db_test, deploy_job_id, deployment_image,
		deployment_status, insert_deployment, local_server_id, newest_deployment, project_id,
		set_deployment_error, set_deployment_status, state,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	struct Fixture {
		state: AppState,
		app: Router,
		cookie: String,
		team: i64,
		project: i64,
		application: i64,
		db: PgPool,
	}

	impl Fixture {
		async fn new(db: &PgPool) -> Self {
			let state = state(db.clone());
			let app = crate::routes::routes(state.clone()).await.unwrap();
			let (cookie, team) = ui_session(&app, "alice").await;
			post_signals(
				&app,
				&format!("/teams/{team}/projects"),
				Some(&cookie),
				&json!({ "name": "Shop", "slug": "shop" }),
			)
			.await;
			let project = project_id(db, "shop").await;
			let server = local_server_id(db, team).await;
			let (s, _, _) = post_signals(
				&app,
				&format!("/teams/{team}/projects/{project}/apps"),
				Some(&cookie),
				&json!({
					"name": "Web", "slug": "web", "server_id": server.to_string(),
					"repo_url": "https://example.com/web.git", "git_ref": "main",
					"deploy_key": "", "git_token": "", "build_strategy": "dockerfile",
					"dockerfile_path": "Dockerfile", "container_port": "3000",
					"memory_limit_mb": "", "cpu_limit": "",
				}),
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			let application = application_id(db, "web").await;
			Self {
				state,
				app,
				cookie,
				team,
				project,
				application,
				db: db.clone(),
			}
		}

		fn base(&self) -> String {
			format!(
				"/teams/{}/projects/{}/apps/{}",
				self.team, self.project, self.application
			)
		}

		async fn get(&self, uri: &str) -> (StatusCode, String) {
			let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
			(s, body)
		}

		async fn post(&self, uri: &str) -> String {
			post_signals(&self.app, uri, Some(&self.cookie), &json!({}))
				.await
				.2
		}

		/// Inserts a deployment of the app straight into `status`.
		async fn seed(&self, id: i64, status: &str, image: Option<&str>, container: Option<&str>) {
			insert_deployment(&self.db, self.application, id, status, image, container).await;
		}

		async fn newest_deployment(&self) -> i64 {
			newest_deployment(&self.db).await
		}
	}

	/// Adds `name` to `team` with the built-in read-only Member role (id 2).
	async fn join_as_member(f: &Fixture, name: &str) -> String {
		let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
		let (_, login) = call(&f.app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
		let token = login["token"].as_str().unwrap().to_owned();
		crate::tests::user_with_team(&f.app, name).await;
		let (s, _) = call(
			&f.app,
			"POST",
			&format!("/api/v1/teams/{}/members", f.team),
			Some(&token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
		ui_login(&f.app, name).await
	}

	#[db_test]
	async fn deploy_queues_a_deployment_and_opens_its_page_with_a_streaming_log(db: PgPool) {
		let f = Fixture::new(&db).await;
		let body = f.post(&format!("{}/deploy", f.base())).await;
		let id = f.newest_deployment().await;
		assert_contains(
			&body,
			&format!("window.location.assign('{}/deployments/{id}')", f.base()),
		);
		assert_eq!(deployment_status(&db, id).await, "queued");

		let page_uri = format!("{}/deployments/{id}", f.base());
		let (s, page) = f.get(&page_uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Queued");
		assert_contains(&page, &format!("Deployment #{id}"));
		assert_contains(&page, "Cancel deployment");
		// Polls while queued, and the log view points at this deployment's log.
		assert_contains(&page, &format!("{page_uri}/status"));
		assert_contains(&page, &format!("{page_uri}/log"));
		assert_contains(&page, "id=\"build-log\"");

		// A fake job log: the agent isn't needed.
		let job = deploy_job_id(&db, id).await;
		let mut log = f.state.logs.open(JobId(job)).await.unwrap();
		log.push(proto::Stream::Stdout, "Step 1/3 : FROM alpine")
			.await;
		log.push(proto::Stream::Stderr, "<script>alert(1)</script>")
			.await;
		log.finish().await;
		let (s, log_body) = f.get(&format!("{page_uri}/log")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&log_body, "selector #build-log");
		assert_contains(&log_body, "Step 1/3 : FROM alpine");
		assert_contains(&log_body, "&lt;script&gt;alert(1)&lt;/script&gt;");
		assert_not_contains(&log_body, "<script>alert(1)");
		assert_contains(&log_body, "_live");

		// Cancelling the queued deployment works end to end, and polling shows it.
		let body = f.post(&format!("{page_uri}/cancel")).await;
		assert_contains(&body, "Cancelling");
		let (_, page) = f.get(&page_uri).await;
		assert_contains(&page, "Cancelled");
		assert_not_contains(&page, "Cancel deployment");
		assert_not_contains(&page, &format!("{page_uri}/status"));
	}

	#[db_test]
	async fn the_history_lists_deployments_and_polls_only_while_one_is_in_flight(db: PgPool) {
		let f = Fixture::new(&db).await;
		let uri = format!("{}/deployments", f.base());
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Nothing has been deployed yet.");

		f.seed(100, "stopped", Some("img:1"), None).await;
		let (_, page) = f.get(&uri).await;
		assert_contains(&page, "Stopped");
		assert_contains(&page, "abcdef1");
		assert_not_contains(&page, "abcdef12");
		assert_contains(&page, "Webhook");
		assert_contains(&page, "1m 15s");
		assert_not_contains(&page, "data-on-interval");

		f.seed(101, "building", None, None).await;
		let (_, page) = f.get(&uri).await;
		assert_contains(&page, "Building");
		assert_contains(&page, "data-on-interval__duration.3s");
		let fragment = f.get(&format!("{uri}/table")).await.1;
		assert_contains(&fragment, "datastar-patch-elements");
		assert_contains(&fragment, "data-on-interval__duration.3s");

		set_deployment_status(&db, 101, "failed").await;
		let fragment = f.get(&format!("{uri}/table")).await.1;
		assert_not_contains(&fragment, "data-on-interval");

		// A user-triggered one says so.
		f.post(&format!("{}/deploy", f.base())).await;
		let (_, page) = f.get(&uri).await;
		assert_contains(&page, "User");
	}

	#[db_test]
	async fn rollback_and_cancel_show_only_in_valid_states(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "stopped", Some("img:1"), None).await;
		f.seed(101, "running", Some("img:2"), Some("c1")).await;
		f.seed(102, "failed", None, None).await;
		f.seed(103, "queued", None, None).await;
		f.seed(104, "building", None, None).await;
		let (_, page) = f.get(&format!("{}/deployments", f.base())).await;
		let rollback = |id: i64| format!("{}/deployments/{id}/rollback", f.base());
		let cancel = |id: i64| format!("{}/deployments/{id}/cancel", f.base());
		assert_contains(&page, &rollback(100));
		assert_contains(&page, "Confirm rollback");
		for id in [101, 102, 103, 104] {
			assert_not_contains(&page, &rollback(id));
		}
		assert_contains(&page, &cancel(103));
		assert_contains(&page, &cancel(104));
		for id in [100, 101, 102] {
			assert_not_contains(&page, &cancel(id));
		}

		// The page of a running deployment offers no cancel either.
		let (_, page) = f.get(&format!("{}/deployments/101", f.base())).await;
		assert_not_contains(&page, "Cancel deployment");
		let (_, page) = f.get(&format!("{}/deployments/104", f.base())).await;
		assert_contains(&page, "Cancel deployment");
	}

	#[db_test]
	async fn rollback_redeploys_the_image_and_refuses_what_cannot_be_rolled_back(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "stopped", Some("img:1"), None).await;
		f.seed(101, "running", Some("img:2"), Some("c1")).await;
		f.seed(102, "failed", None, None).await;
		let rollback = |id: i64| format!("{}/deployments/{id}/rollback", f.base());

		let before = count(&db, "deployments").await;
		for (id, message) in [
			(101, "live or still in progress"),
			(102, "never produced an image"),
		] {
			let body = f.post(&rollback(id)).await;
			assert_contains(&body, message);
			assert_contains(&body, "_action_error");
		}
		let body = f
			.post(&format!("{}/deployments/999/rollback", f.base()))
			.await;
		assert_contains(&body, "That no longer exists.");
		assert_eq!(count(&db, "deployments").await, before);

		let body = f.post(&rollback(100)).await;
		let new = f.newest_deployment().await;
		assert_ne!(new, 102);
		assert_contains(
			&body,
			&format!("window.location.assign('{}/deployments/{new}')", f.base()),
		);
		assert_eq!(deployment_image(&db, new).await.as_deref(), Some("img:1"));
		assert_eq!(count(&db, "deployments").await, before + 1);
	}

	#[db_test]
	async fn cancelling_a_finished_deployment_is_refused_inline(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "stopped", Some("img:1"), None).await;
		let body = f
			.post(&format!("{}/deployments/100/cancel", f.base()))
			.await;
		assert_contains(&body, "not in progress");
		assert_not_contains(&body, "Cancelling");
	}

	#[db_test]
	async fn the_header_buttons_follow_the_latest_state(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (_, page) = f.get(&f.base()).await;
		assert_contains(&page, "Deploy");
		assert_contains(&page, &format!("{}/deploy", f.base()));
		// Nothing built or running: Restart and Stop exist but are disabled.
		assert_not_contains(&page, &format!("{}/restart", f.base()));
		assert_not_contains(&page, &format!("{}/stop", f.base()));
		assert_contains(&page, "disabled");

		f.seed(100, "running", Some("img:1"), Some("c1")).await;
		let (_, page) = f.get(&f.base()).await;
		assert_contains(&page, &format!("{}/restart", f.base()));
		assert_contains(&page, &format!("{}/stop", f.base()));

		set_deployment_status(&db, 100, "stopped").await;
		let (_, page) = f.get(&f.base()).await;
		assert_contains(&page, &format!("{}/restart", f.base()));
		assert_not_contains(&page, &format!("{}/stop", f.base()));
	}

	#[db_test]
	async fn stop_and_restart_show_the_api_refusals_inline(db: PgPool) {
		let f = Fixture::new(&db).await;
		let body = f.post(&format!("{}/stop", f.base())).await;
		assert_contains(&body, "not currently running");
		let body = f.post(&format!("{}/restart", f.base())).await;
		assert_contains(&body, "no previous successful build");
		assert_eq!(count(&db, "deployments").await, 0);

		f.seed(100, "stopped", Some("img:1"), None).await;
		let body = f.post(&format!("{}/restart", f.base())).await;
		let new = f.newest_deployment().await;
		assert_contains(
			&body,
			&format!("window.location.assign('{}/deployments/{new}')", f.base()),
		);
		assert_eq!(count(&db, "deployments").await, 2);
	}

	#[db_test]
	async fn the_logs_tab_says_so_when_nothing_runs(db: PgPool) {
		let f = Fixture::new(&db).await;
		let uri = format!("{}/logs", f.base());
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "isn't running");
		assert_not_contains(&page, "runtime-log");

		f.seed(100, "stopped", Some("img:1"), None).await;
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "isn't running");

		// The stream endpoint refuses in a signal, not a 500.
		let (s, body) = f.get(&format!("{uri}/stream")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "not currently running");
		assert_contains(&body, "#runtime-log");
		assert_contains(&body, "_live");
	}

	#[db_test]
	async fn the_logs_tab_streams_a_chosen_tail_while_running(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "running", Some("img:1"), Some("c1")).await;
		let uri = format!("{}/logs", f.base());
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "isn't running");
		assert_contains(&page, "id=\"runtime-log\"");
		assert_contains(&page, &format!("{uri}/stream?tail=200&amp;follow=1"));
		for tail in ["200", "1000", "5000"] {
			assert_contains(&page, &format!("value=\"{tail}\""));
		}

		let (_, page) = f.get(&format!("{uri}?tail=5000&follow=0")).await;
		assert_contains(&page, &format!("{uri}/stream?tail=5000&amp;follow=0"));
		assert_contains(&page, "value=\"5000\" selected");
		// Anything else falls back to the default.
		let (_, page) = f.get(&format!("{uri}?tail=999999&follow=x")).await;
		assert_contains(&page, &format!("{uri}/stream?tail=200&amp;follow=1"));

		// With no agent behind it the stream fails inline, not with a 500.
		let (s, body) = f.get(&format!("{uri}/stream")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "#runtime-log");
	}

	#[db_test]
	async fn a_read_only_member_sees_no_buttons_and_forged_posts_are_refused(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "stopped", Some("img:1"), None).await;
		f.seed(101, "running", Some("img:2"), Some("c1")).await;
		f.seed(102, "queued", None, None).await;
		let carol = join_as_member(&f, "carol").await;

		for uri in [
			f.base(),
			format!("{}/deployments", f.base()),
			format!("{}/deployments/100", f.base()),
			format!("{}/deployments/102", f.base()),
			format!("{}/logs", f.base()),
		] {
			let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			for control in [
				">Rollback<",
				">Cancel<",
				">Restart<",
				">Stop<",
				">Deploy<",
				"/deploy'",
				"/stop'",
				"/restart'",
			] {
				assert_not_contains(&page, control);
			}
		}
		let (_, _, page) = get_html(&f.app, &f.base(), Some(&carol)).await;
		assert_contains(&page, "Deployments");

		let before = count(&db, "deployments").await;
		for uri in [
			format!("{}/deploy", f.base()),
			format!("{}/restart", f.base()),
			format!("{}/stop", f.base()),
			format!("{}/deployments/100/rollback", f.base()),
			format!("{}/deployments/102/cancel", f.base()),
		] {
			let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &json!({})).await;
			assert_contains(&body, "You don't have permission to do that.");
		}
		assert_eq!(count(&db, "deployments").await, before);
		assert_eq!(deployment_status(&db, 102).await, "queued");
		assert_eq!(deployment_status(&db, 101).await, "running");
	}

	#[db_test]
	async fn text_from_a_failed_build_is_escaped(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "failed", None, None).await;
		set_deployment_error(&db, 100, "<script>alert(1)</script>").await;
		let (s, page) = f.get(&format!("{}/deployments/100", f.base())).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "<script>alert(1)");
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}

	#[db_test]
	async fn another_teams_deployments_are_the_404_page_and_cannot_be_changed(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.seed(100, "stopped", Some("img:1"), None).await;
		f.seed(101, "queued", None, None).await;
		let (bob, bob_team) = ui_session(&f.app, "bob").await;
		let theirs = format!(
			"/teams/{bob_team}/projects/{}/apps/{}",
			f.project, f.application
		);

		for uri in [
			format!("{theirs}/deployments"),
			format!("{theirs}/deployments/100"),
			format!("{theirs}/logs"),
			format!("{theirs}/deployments/100/log"),
		] {
			let (s, _, body) = get_html(&f.app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_not_contains(&body, "Stopped");
		}
		// Alice's own team path, Bob's cookie: not a member, so the same page.
		for uri in [
			format!("{}/deployments", f.base()),
			format!("{}/deployments/100", f.base()),
			format!("{}/logs", f.base()),
		] {
			let (s, _, _) = get_html(&f.app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		}

		let before = count(&db, "deployments").await;
		for base in [&theirs, &f.base()] {
			for path in [
				"/deploy",
				"/restart",
				"/stop",
				"/deployments/100/rollback",
				"/deployments/101/cancel",
			] {
				let (_, _, body) =
					post_signals(&f.app, &format!("{base}{path}"), Some(&bob), &json!({})).await;
				assert_contains(&body, "That no longer exists.");
			}
		}
		assert_eq!(count(&db, "deployments").await, before);
		assert_eq!(deployment_status(&db, 101).await, "queued");
	}

	#[db_test]
	async fn the_json_deployment_endpoints_still_work(db: PgPool) {
		let f = Fixture::new(&db).await;
		let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
		let (_, login) = call(&f.app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
		let token = login["token"].as_str().unwrap().to_owned();
		let api = format!(
			"/api/v1/teams/{}/projects/{}/applications/{}",
			f.team, f.project, f.application
		);
		let (s, d) = call(
			&f.app,
			"POST",
			&format!("{api}/deployments"),
			Some(&token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED, "{d}");
		let id = d["id"].as_i64().unwrap();
		assert_eq!(d["status"], "queued");
		let (s, d) = call(
			&f.app,
			"POST",
			&format!("{api}/deployments/{id}/cancel"),
			Some(&token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED, "{d}");
		assert_eq!(d["status"], "cancelled");
		let (s, e) = call(&f.app, "POST", &format!("{api}/stop"), Some(&token), None).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
		let (s, e) = call(
			&f.app,
			"POST",
			&format!("{api}/restart"),
			Some(&token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
		let (s, _) = call(&f.app, "GET", &format!("{api}/logs"), Some(&token), None).await;
		assert_eq!(s, StatusCode::BAD_REQUEST);
	}

	#[test]
	fn durations_and_times_read_plainly() {
		assert_eq!(duration(time::Duration::seconds(5)), "5s");
		assert_eq!(duration(time::Duration::seconds(75)), "1m 15s");
		assert_eq!(duration(time::Duration::seconds(3700)), "1h 1m");
		assert_eq!(duration(time::Duration::seconds(-3)), "0s");
		let t = time::macros::datetime!(2026-11-17 20:03:59 UTC);
		assert_eq!(when(t), "2026-11-17 20:03 UTC");
		assert_eq!(short_sha("abcdef1234"), "abcdef1");
		assert_eq!(short_sha("abc"), "abc");
	}
}
