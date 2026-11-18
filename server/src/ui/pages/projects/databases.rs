//! Database pages: create, overview (status, lifecycle, connection, latest
//! job log), container logs, settings (limits, backups) and delete.
//!
//! The password is write-only in spirit: the pages show host, port, user and
//! database, and the password and URLs arrive only in the response to the
//! **Reveal** action, never in a page or a signal.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::auth::permission::{Action, Resource};
use crate::databases::service::{self, ConnectionInfo, CreateInput, DatabaseDetail, SettingsInput};
use crate::databases::{Database, DatabaseStatus, EngineKind};
use crate::error::ServerError;
use crate::ids::{BackupTargetId, DatabaseId, ProjectId, ServerId, TeamId};
use crate::jobs::JobStatus;
use crate::projects;
use crate::projects::Project;
use crate::servers::ServerStatus;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, TAB, TAB_ACTIVE, card, card_content,
	card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::live::{container_log, job_log, log_view};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/databases",
			post(create),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/new",
			get(new_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/rows",
			get(rows_poll),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}",
			get(overview),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/status",
			get(status_poll),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/job-log",
			get(job_log_stream),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/start",
			post(start),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/stop",
			post(stop),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/restart",
			post(restart),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/reveal",
			post(reveal),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/logs",
			get(logs_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/logs/stream",
			get(logs_stream),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/settings",
			get(settings_page).post(save),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/schedule",
			post(schedule_preview),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/delete",
			post(remove),
		)
}

// --- urls and small pieces ----------------------------------------------------

fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

fn databases_url(team: TeamId, project: ProjectId) -> String {
	format!("{}/databases", project_url(team, project))
}

pub fn database_url(team: TeamId, project: ProjectId, database: DatabaseId) -> String {
	format!("{}/{database}", databases_url(team, project))
}

fn crumbs<'a>(ctx: &TeamCtx, project: &'a Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

fn engine_label(engine: EngineKind) -> &'static str {
	match engine {
		EngineKind::Postgres => "PostgreSQL",
		EngineKind::Mysql => "MySQL",
		EngineKind::Mongodb => "MongoDB",
		EngineKind::Redis => "Redis",
	}
}

fn engine_value(engine: EngineKind) -> &'static str {
	match engine {
		EngineKind::Postgres => "postgres",
		EngineKind::Mysql => "mysql",
		EngineKind::Mongodb => "mongodb",
		EngineKind::Redis => "redis",
	}
}

fn parse_engine(raw: &str) -> Result<EngineKind, ServerError> {
	[
		EngineKind::Postgres,
		EngineKind::Mysql,
		EngineKind::Mongodb,
		EngineKind::Redis,
	]
	.into_iter()
	.find(|e| engine_value(*e) == raw)
	.ok_or_else(|| ServerError::Validation("Pick an engine.".to_owned()))
}

/// `2026-11-18 03:00 UTC`.
pub(in crate::ui) fn when(t: OffsetDateTime) -> String {
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

/// A signal value as text, whether Datastar sent it as a string, a number or
/// nothing.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::String(s) => s,
		Value::Number(n) => n.to_string(),
		_ => String::new(),
	})
}

fn number<T: std::str::FromStr>(raw: &str, what: &str) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a number.")))
}

fn id<T>(raw: &str, what: &str, make: fn(i64) -> T) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse::<i64>()
		.map(|n| Some(make(n)))
		.map_err(|_| ServerError::Validation(format!("Pick a valid {what}.")))
}

/// A read-only field with a button that copies its value. `execCommand`
/// rather than the Clipboard API, which browsers withhold over plain HTTP.
fn copy_row(label: &str, value: &str) -> Markup {
	html! {
		div class="flex flex-col gap-2 text-sm font-medium" {
			span { (label) }
			div class="flex gap-2" {
				input class=(INPUT) type="text" readonly value=(value) autocomplete="off" spellcheck="false" "data-on:focus"="el.select()";
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"="el.previousElementSibling.select(); document.execCommand('copy')" { "Copy" }
			}
		}
	}
}

// --- the project overview's database card -------------------------------------

/// The databases of a project as the project overview shows them. Re-fetched
/// every few seconds while any of them is still changing.
pub fn project_rows(ctx: &TeamCtx, project: ProjectId, rows: &[service::Row]) -> Markup {
	let in_flight = rows.iter().any(|r| r.status.in_flight());
	let url = format!("{}/rows", databases_url(ctx.team.id, project));
	html! {
		div id="db-rows" {
			@if rows.is_empty() {
				p class="text-sm text-muted-foreground" { "No databases yet." }
			} @else {
				ul {
					@for row in rows {
						li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0" {
							div class="flex min-w-0 flex-col gap-1" {
								a class="font-medium hover:underline" href=(database_url(ctx.team.id, project, row.id)) { (row.name) }
								span class="text-muted-foreground" { (engine_label(row.engine)) " " (row.version) }
							}
							(status_badge(row.status))
						}
					}
				}
			}
			(poll(&url, in_flight))
		}
	}
}

/// The project overview's Databases card, with its "New database" button for
/// whoever may create one.
#[allow(
	dead_code,
	reason = "the environment grid replaced it; the database cards drop it"
)]
pub fn project_card(ctx: &TeamCtx, project: ProjectId, rows: &[service::Row]) -> Markup {
	let can_create = ctx.perms.can(Action::Create, Resource::Database);
	card(&html! {
		(card_header("Databases", None))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				(project_rows(ctx, project, rows))
				@if can_create {
					div {
						a class=(BUTTON) href={ (databases_url(ctx.team.id, project)) "/new" } { "New database" }
					}
				}
			}
		}))
	})
}

async fn rows_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Response {
	match service::list(&s.db, ctx.user.id, ctx.team.id, project, None).await {
		Ok(rows) => {
			let rows: Vec<service::Row> = rows.iter().map(service::Row::from).collect();
			sse::patch(&project_rows(&ctx, project, &rows)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- create -------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateSignals {
	name: String,
	engine: String,
	version: String,
	server_id: String,
	#[serde(deserialize_with = "lenient")]
	public_port: String,
	#[serde(deserialize_with = "lenient")]
	memory_limit_mb: String,
	#[serde(deserialize_with = "lenient")]
	cpu_limit: String,
}

impl CreateSignals {
	fn input(&self) -> Result<CreateInput, ServerError> {
		let version = self.version.trim();
		Ok(CreateInput {
			name: self.name.trim().to_owned(),
			engine: parse_engine(&self.engine)?,
			version: (!version.is_empty()).then(|| version.to_owned()),
			server_id: id(&self.server_id, "server", ServerId)?
				.ok_or_else(|| ServerError::Validation("Pick a server.".to_owned()))?,
			public_port: number(&self.public_port, "Public port")?,
			memory_limit_mb: number(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: number(&self.cpu_limit, "CPU limit")?,
			environment_id: None,
		})
	}
}

/// The version hint of one engine, shown while that engine is picked.
fn version_hint(option: &service::EngineOption) -> Markup {
	let condition = format!("$engine === {}", js_string(engine_value(option.kind)));
	html! {
		p class="text-sm font-normal text-muted-foreground" "data-show"=(condition) style="display: none" {
			"Default " strong { (option.default_version) }
			". Available: " (option.versions.join(", ")) "."
		}
	}
}

async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers = service::servers_for_create(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;
	let servers: Vec<_> = servers
		.into_iter()
		.filter(|server| server.status == ServerStatus::Active)
		.collect();
	let options = service::engine_options();

	let first = servers
		.first()
		.map(|server| server.id.to_string())
		.unwrap_or_default();
	let signals = json!({
		"name": "", "engine": "postgres", "version": "", "server_id": first,
		"public_port": "", "memory_limit_mb": "", "cpu_limit": "", "_error": "",
	})
	.to_string();
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&databases_url(ctx.team.id, project.id))
	);

	let body = html! {
		div class="flex flex-col gap-6" "data-signals"=(signals) {
			(card(&html! {
				(card_header("New database", Some("A container on one of your servers, with generated credentials.")))
				(card_content(&html! {
					@if servers.is_empty() {
						p class="text-sm text-muted-foreground" { "No active server yet. Add one under Servers first." }
					} @else {
						form class="flex flex-col gap-4" "data-on:submit"=(post) {
							label class=(LABEL) {
								"Name"
								input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
							}
							label class=(LABEL) {
								"Engine"
								select class=(INPUT) "data-bind:engine" {
									@for option in &options {
										option value=(engine_value(option.kind)) { (option.label) }
									}
								}
							}
							label class=(LABEL) {
								"Version (optional)"
								input class=(INPUT) type="text" autocomplete="off" placeholder="Engine default" "data-bind:version";
								@for option in &options { (version_hint(option)) }
							}
							label class=(LABEL) {
								"Server"
								select class=(INPUT) "data-bind:server_id" {
									@for server in &servers {
										option value=(server.id.0) { (server.name) }
									}
								}
							}
							label class=(LABEL) {
								"Public port (optional)"
								input class=(INPUT) type="text" inputmode="numeric" placeholder="Not published" "data-bind:public_port";
								span class="text-sm font-normal text-muted-foreground" { "Publish the database on the server's host at this port, 1024 to 65535. Leave empty to keep it reachable only by your applications." }
							}
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Memory limit in MB (optional)"
									input class=(INPUT) type="text" inputmode="numeric" placeholder="No limit" "data-bind:memory_limit_mb";
								}
								label class=(LABEL) {
									"CPU limit in cores (optional)"
									input class=(INPUT) type="text" inputmode="decimal" placeholder="No limit" "data-bind:cpu_limit";
								}
							}
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create database" }
							}
						}
					}
				}))
			}))
		}
	};
	Ok(app_page(
		&ctx,
		"New database",
		&crumbs(&ctx, &project, "New database"),
		&body,
	))
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		service::create(&s, ctx.user.id, ctx.team.id, project, input).await
	}
	.await;
	match result {
		Ok(database) => {
			sse::redirect(&database_url(ctx.team.id, project, database.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- overview -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Tab {
	Overview,
	Logs,
	Backups,
	Settings,
}

/// True while the status, or a queued or running job, can still change it.
fn busy(d: &DatabaseDetail) -> bool {
	d.database.status.in_flight()
		|| matches!(d.job, Some((_, JobStatus::Queued | JobStatus::Running)))
}

fn tabs(ctx: &TeamCtx, database: &Database, active: Tab) -> Markup {
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let class = |tab| if tab == active { TAB_ACTIVE } else { TAB };
	let can_configure = ctx.perms.can(Action::Update, Resource::Database)
		|| ctx.perms.can(Action::Delete, Resource::Database);
	html! {
		nav class="flex gap-1 border-b border-border" aria-label="Database" {
			a class=(class(Tab::Overview)) href=(base) { "Overview" }
			a class=(class(Tab::Logs)) href={ (base) "/logs" } { "Logs" }
			@if ctx.perms.can(Action::Read, Resource::Backup) {
				a class=(class(Tab::Backups)) href={ (base) "/backups" } { "Backups" }
			}
			@if can_configure {
				a class=(class(Tab::Settings)) href={ (base) "/settings" } { "Settings" }
			}
		}
	}
}

pub(in crate::ui) fn shell(
	ctx: &TeamCtx,
	project: &Project,
	d: &DatabaseDetail,
	active: Tab,
	content: &Markup,
) -> Markup {
	let database = &d.database;
	let body = html! {
		div class="flex flex-col gap-1" {
			h1 class="text-xl font-semibold break-words" { (database.name) }
			p class="text-sm text-muted-foreground" {
				(engine_label(database.engine)) " " (database.version) " on " (d.server_name)
			}
		}
		(tabs(ctx, database, active))
		(content)
	};
	let title = match active {
		Tab::Overview => database.name.clone(),
		Tab::Logs => format!("Logs · {}", database.name),
		Tab::Backups => format!("Backups · {}", database.name),
		Tab::Settings => format!("Settings · {}", database.name),
	};
	app_page(ctx, &title, &crumbs(ctx, project, &database.name), &body)
}

pub(in crate::ui) async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<(Project, DatabaseDetail), Response> {
	let fail = |e: ServerError| error::page(ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let detail = service::detail(&s.db, ctx.user.id, ctx.team.id, project.id, database)
		.await
		.map_err(fail)?;
	Ok((project, detail))
}

/// Status, what the last job said, and the lifecycle buttons. Polls itself
/// while the database or its job is still moving.
fn status_region(ctx: &TeamCtx, d: &DatabaseDetail) -> Markup {
	let database = &d.database;
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let busy = busy(d);
	let can_update = ctx.perms.can(Action::Update, Resource::Database);
	let status = database.status;
	let can_start = !busy && matches!(status, DatabaseStatus::Stopped | DatabaseStatus::Failed);
	let can_stop = !busy && status == DatabaseStatus::Running;
	let can_restart = !busy
		&& matches!(
			status,
			DatabaseStatus::Running | DatabaseStatus::Stopped | DatabaseStatus::Failed
		);
	let action = |name: &str| format!("@post({})", js_string(&format!("{base}/{name}")));
	html! {
		div id="db-status" class="flex flex-col gap-4" {
			div class="flex flex-wrap items-center gap-3" {
				(status_badge(status))
				@if busy && !status.in_flight() {
					span class="text-sm text-muted-foreground" { "A job is running" }
				}
				@if let Some(message) = &database.status_message {
					span class="text-sm text-muted-foreground break-words" { (message) }
				}
			}
			@if can_update {
				div class="flex flex-wrap gap-3" {
					button type="button" class=(BUTTON) disabled[!can_start] "data-on:click"=(action("start")) { "Start" }
					button type="button" class=(BUTTON_OUTLINE) disabled[!can_stop] "data-on:click"=(action("stop")) { "Stop" }
					button type="button" class=(BUTTON_OUTLINE) disabled[!can_restart] "data-on:click"=(action("restart")) { "Restart" }
				}
				(form_error())
			}
			(poll(&format!("{base}/status"), busy))
		}
	}
}

fn connection_card(ctx: &TeamCtx, d: &DatabaseDetail, info: &ConnectionInfo) -> Markup {
	let base = database_url(ctx.team.id, d.database.project_id, d.database.id);
	let reveal = format!("@post({})", js_string(&format!("{base}/reveal")));
	let keyed = d.database.engine != EngineKind::Redis;
	let host_port = format!("{}:{}", info.internal_host, info.port);
	card(&html! {
		(card_header("Connection", Some("How your applications reach this database inside podkit.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				(copy_row("Host", &info.internal_host))
				(copy_row("Port", &info.port.to_string()))
				@if keyed {
					(copy_row("User", &info.username))
					(copy_row("Database", &info.database))
				}
				@if let Some((host, port)) = &info.public {
					(copy_row("Public address", &format!("{host}:{port}")))
				}
				p class="text-sm text-muted-foreground" { "Internal address " (host_port) "." }
				div id="db-secret" class="flex flex-col gap-4" {}
				p class="text-sm text-destructive" role="alert" "data-show"="$_reveal_error" "data-text"="$_reveal_error" style="display: none" {}
				div {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(reveal) "data-indicator:_revealing" "data-attr:disabled"="$_revealing" { "Reveal password and URLs" }
				}
			}
		}))
	})
}

async fn overview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let (project, d) = load(&s, &ctx, project, database).await?;
	let info = service::connection_info(&s, ctx.user.id, ctx.team.id, project.id, d.database.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = database_url(ctx.team.id, project.id, d.database.id);

	let content = html! {
		div class="flex flex-col gap-6" "data-signals"="{_error: '', _reveal_error: ''}" {
			(card(&html! {
				(card_header("Status", None))
				(card_content(&status_region(&ctx, &d)))
			}))
			@if let Some(info) = &info { (connection_card(&ctx, &d, info)) }
			(card(&html! {
				(card_header("Latest job", Some("What podkit did most recently to this database.")))
				(card_content(&html! {
					@if d.job.is_some() {
						(log_view("job-log", &format!("{base}/job-log")))
					} @else {
						p class="text-sm text-muted-foreground" { "No jobs have run yet." }
					}
				}))
			}))
		}
	};
	Ok(shell(&ctx, &project, &d, Tab::Overview, &content))
}

async fn status_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::detail(&s.db, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => sse::patch(&status_region(&ctx, &d)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn job_log_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::latest_job(&s.db, ctx.user.id, ctx.team.id, project, database).await {
		Ok(job) => job_log(&s, job, "job-log"),
		Err(e) => log_failure("job-log", &e),
	}
}

/// A log stream that could not start: say so inside the log and stop the
/// "Live" indicator.
fn log_failure(target: &str, e: &ServerError) -> Response {
	let message = error::message(e);
	let line = html! {
		div class="break-all whitespace-pre-wrap text-muted-foreground" { (message) }
	};
	sse::patch_into(&format!("#{target}"), ElementPatchMode::Append, &line)
		.and(sse::signals(&json!({ "_live": false })))
		.into_response()
}

// --- lifecycle ----------------------------------------------------------------

/// Starts a lifecycle job and reloads the page, which then follows its log.
fn reloaded(ctx: &TeamCtx, database: &Database) -> Response {
	sse::redirect(&database_url(ctx.team.id, database.project_id, database.id)).into_response()
}

async fn start(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::start(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => error::action(&e),
	}
}

async fn stop(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::stop(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => error::action(&e),
	}
}

async fn restart(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::restart(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => error::action(&e),
	}
}

// --- reveal -------------------------------------------------------------------

/// The password and URLs, patched into `#db-secret`. They exist in a page
/// only after this click, and the response may not be stored.
async fn reveal(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	let connection = match service::reveal(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(connection) => connection,
		Err(e) => {
			return sse::signals(&json!({ "_reveal_error": error::message(&e) })).into_response();
		}
	};
	let secret = html! {
		div id="db-secret" class="flex flex-col gap-4" {
			(copy_row("Password", &connection.password))
			(copy_row("Internal URL", &connection.internal_url))
			@if let Some(public) = &connection.public_url {
				(copy_row("Public URL", public))
			}
			div {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"="el.closest('#db-secret').replaceChildren()" { "Hide" }
			}
		}
	};
	let mut response = sse::patch(&secret)
		.and(sse::signals(&json!({ "_reveal_error": "" })))
		.into_response();
	response
		.headers_mut()
		.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	response
}

// --- container logs -----------------------------------------------------------

async fn logs_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let (project, d) = load(&s, &ctx, project, database).await?;
	let base = database_url(ctx.team.id, project.id, d.database.id);
	let content = card(&html! {
		(card_header("Container log", Some("The database engine's own output, newest last.")))
		(card_content(&html! {
			@if d.database.container_id.is_some() {
				(log_view("container-log", &format!("{base}/logs/stream")))
			} @else {
				p class="text-sm text-muted-foreground" { "There is no container to read a log from. Start the database first." }
			}
		}))
	});
	Ok(shell(&ctx, &project, &d, Tab::Logs, &content))
}

async fn logs_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::container_logs(&s, ctx.user.id, ctx.team.id, project, database, 300, true).await
	{
		Ok(events) => container_log(events, "container-log", s.shutdown.clone()),
		Err(e) => log_failure("container-log", &e),
	}
}

// --- settings and delete ------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct SettingsSignals {
	#[serde(deserialize_with = "lenient")]
	public_port: String,
	#[serde(deserialize_with = "lenient")]
	memory_limit_mb: String,
	#[serde(deserialize_with = "lenient")]
	cpu_limit: String,
	#[serde(deserialize_with = "lenient")]
	backup_target_id: String,
	backup_schedule: String,
	#[serde(deserialize_with = "lenient")]
	backup_keep: String,
}

impl SettingsSignals {
	fn schedule(&self) -> Option<&str> {
		let schedule = self.backup_schedule.trim();
		(!schedule.is_empty()).then_some(schedule)
	}

	fn input(&self) -> Result<SettingsInput, ServerError> {
		Ok(SettingsInput {
			public_port: number(&self.public_port, "Public port")?,
			memory_limit_mb: number(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: number(&self.cpu_limit, "CPU limit")?,
			backup_target_id: id(&self.backup_target_id, "backup target", BackupTargetId)?,
			backup_schedule: self.schedule().map(str::to_owned),
			backup_keep: number(&self.backup_keep, "Backups to keep")?,
		})
	}
}

fn prefilled(database: &Database) -> String {
	let text = |n: Option<String>| n.unwrap_or_default();
	json!({
		"public_port": text(database.public_port.map(|n| n.to_string())),
		"memory_limit_mb": text(database.memory_limit_mb.map(|n| n.to_string())),
		"cpu_limit": text(database.cpu_limit.map(|n| n.to_string())),
		"backup_target_id": text(database.backup_target_id.map(|n| n.0.to_string())),
		"backup_schedule": database.backup_schedule.clone().unwrap_or_default(),
		"backup_keep": database.backup_keep.to_string(),
		"confirm": "", "purge": false,
		"_error": "", "_delete_error": "",
	})
	.to_string()
}

/// The line under the schedule field: nothing set, the next run, or why the
/// expression is not valid.
fn schedule_hint(schedule: Option<&Result<OffsetDateTime, String>>) -> Markup {
	html! {
		p id="schedule-hint" class="text-sm font-normal" role="status" {
			@match schedule {
				None => span class="text-muted-foreground" { "No schedule: backups run only when you start them." },
				Some(Ok(next)) => span class="text-muted-foreground" { "Next run " (when(*next)) "." },
				Some(Err(why)) => span class="text-destructive" { "Not a valid schedule: " (why) },
			}
		}
	}
}

fn delete_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
	}
}

// A maud template reads best as one piece: the form and the danger zone.
#[allow(clippy::too_many_lines)]
async fn settings_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let can_update = ctx.perms.can(Action::Update, Resource::Database);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Database);
	if !can_update && !can_delete {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let (project, d) = load(&s, &ctx, project, database).await?;
	let database = &d.database;
	let base = database_url(ctx.team.id, project.id, database.id);
	let targets = if can_update {
		service::backup_targets(&s.db, ctx.user.id, ctx.team.id)
			.await
			.map_err(|e| error::page(&ctx, &e))?
	} else {
		Vec::new()
	};
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/settings"))
	);
	let preview = format!("@post({})", js_string(&format!("{base}/schedule")));
	let delete = format!("@post({})", js_string(&format!("{base}/delete")));
	let current = database
		.backup_schedule
		.as_deref()
		.map(service::check_schedule);

	let content = html! {
		div class="flex flex-col gap-6" "data-signals"=(prefilled(database)) {
			@if can_update {
				(card(&html! {
					(card_header("Settings", Some("Changing the public port or a limit recreates the container; its data is kept.")))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(save) {
							label class=(LABEL) {
								"Public port"
								input class=(INPUT) type="text" inputmode="numeric" placeholder="Not published" "data-bind:public_port";
							}
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Memory limit in MB"
									input class=(INPUT) type="text" inputmode="numeric" placeholder="No limit" "data-bind:memory_limit_mb";
								}
								label class=(LABEL) {
									"CPU limit in cores"
									input class=(INPUT) type="text" inputmode="decimal" placeholder="No limit" "data-bind:cpu_limit";
								}
							}
							label class=(LABEL) {
								"Backup target"
								select class=(INPUT) "data-bind:backup_target_id" {
									option value="" { "None" }
									@for (target, name) in &targets {
										option value=(target.0) { (name) }
									}
								}
								@if ctx.perms.can(Action::Create, Resource::Backup) {
									a class="text-sm font-normal underline" href={ "/teams/" (ctx.team.id) "/backup-targets" } { "Add a backup target" }
								}
							}
							label class=(LABEL) {
								"Backup schedule (cron, UTC)"
								input class=(INPUT) type="text" autocomplete="off" spellcheck="false" placeholder="0 3 * * *" "data-bind:backup_schedule" "data-on:input__debounce.400ms"=(preview);
								(schedule_hint(current.as_ref()))
							}
							label class=(LABEL) {
								"Backups to keep"
								input class=(INPUT) type="text" inputmode="numeric" placeholder="7" "data-bind:backup_keep";
							}
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save changes" }
							}
						}
					}))
				}))
			}
			@if can_delete {
				(card(&html! {
					(card_header("Danger zone", None))
					(card_content(&html! {
						div class="flex flex-col gap-4" {
							p class="text-sm" {
								"Deleting stops and removes the database's container. Type "
								strong class="break-all" { (database.name) }
								" to confirm."
							}
							label class="flex cursor-pointer items-center gap-2 text-sm" {
								input type="checkbox" "data-bind:purge";
								"Also delete the data"
							}
							p class="text-sm text-destructive" "data-show"="$purge" style="display: none" {
								"The data volume is deleted too. This can't be undone."
							}
							p class="text-sm text-muted-foreground" "data-show"="!$purge" {
								"Without this, the data volume stays on the server."
							}
							input class=(INPUT) type="text" autocomplete="off" aria-label="Database name" "data-bind:confirm";
							(delete_error())
							div {
								button type="button" class=(BUTTON_DANGER) "data-on:click"=(delete) "data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete database" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(shell(&ctx, &project, &d, Tab::Settings, &content))
}

async fn schedule_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let Some(schedule) = sig.schedule() else {
		return sse::patch(&schedule_hint(None)).into_response();
	};
	match service::preview_schedule(&s.db, ctx.user.id, ctx.team.id, project, database, schedule)
		.await
	{
		Ok(result) => sse::patch(&schedule_hint(Some(&result))).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		// A bad cron is the field's problem; say it under the field.
		let next = sig.schedule().map(service::check_schedule);
		if let Some(Err(_)) = &next {
			return Ok(Err(schedule_hint(next.as_ref())));
		}
		let saved =
			service::update_settings(&s, ctx.user.id, ctx.team.id, project, database, input)
				.await?;
		Ok::<_, ServerError>(Ok((saved, next)))
	}
	.await;
	match result {
		Ok(Err(hint)) => sse::patch(&hint)
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Ok(Ok(((updated, true), _))) => reloaded(&ctx, &updated),
		Ok(Ok(((_, false), next))) => sse::patch(&schedule_hint(next.as_ref()))
			.and(sse::signals(&json!({ "_error": "" })))
			.and(sse::toast("Settings saved"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DeleteSignals {
	confirm: String,
	purge: bool,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let result = async {
		// The name is checked after a read, so a mistyped one never reaches
		// the delete; `remove` still authorizes the delete itself.
		let found = service::find(&s.db, ctx.user.id, ctx.team.id, project, database).await?;
		if sig.confirm != found.name {
			return Ok(false);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, project, database, sig.purge).await?;
		Ok::<_, ServerError>(true)
	}
	.await;
	match result {
		Ok(true) => sse::redirect(&project_url(ctx.team.id, project)).into_response(),
		Ok(false) => sse::signals(&json!({
			"_delete_error": "The name doesn't match. Type it exactly to confirm.",
		}))
		.into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use axum::Router;
	use axum::http::{StatusCode, header};
	use serde_json::{Value, json};

	use crate::tests::dbs::{
		add_backup_target, add_job, backup_settings, database_id, delete_jobs, job_kinds,
		local_server, set_jobs, set_state, status_of,
	};
	use crate::tests::{PgPool, app, call, count, db_test, user_with_team};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
	};

	struct Fixture {
		app: Router,
		token: String,
		cookie: String,
		team: i64,
		project: i64,
		server: i64,
	}

	impl Fixture {
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
			let server = local_server(db, team).await;
			Self {
				app,
				token,
				cookie,
				team,
				project: project["id"].as_i64().unwrap(),
				server,
			}
		}

		fn base(&self) -> String {
			format!("/teams/{}/projects/{}/databases", self.team, self.project)
		}

		fn url(&self, id: i64) -> String {
			format!("{}/{id}", self.base())
		}

		#[allow(clippy::needless_pass_by_value)]
		fn form(&self, name: &str, extra: Value) -> Value {
			let mut form = json!({
				"name": name, "engine": "postgres", "version": "",
				"server_id": self.server.to_string(),
				"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
			});
			for (k, v) in extra.as_object().unwrap() {
				form[k] = v.clone();
			}
			form
		}

		/// Creates a database through the UI and returns its id.
		async fn create(&self, db: &PgPool, name: &str, extra: Value) -> i64 {
			let (s, _, body) = post_signals(
				&self.app,
				&self.base(),
				Some(&self.cookie),
				&self.form(name, extra),
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			let id = database_id(db, name).await;
			assert_contains(
				&body,
				&format!("window.location.assign('{}')", self.url(id)),
			);
			id
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

		async fn password(&self, id: i64) -> String {
			let (s, db) = call(
				&self.app,
				"GET",
				&format!(
					"/api/v1/teams/{}/projects/{}/databases/{id}",
					self.team, self.project
				),
				Some(&self.token),
				None,
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			db["connection"]["password"].as_str().unwrap().to_owned()
		}

		/// A member with the built-in read-only role, signed in.
		async fn member(&self, name: &str) -> String {
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
	fn saved(extra: Value) -> Value {
		let mut form = json!({
			"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
			"backup_target_id": "", "backup_schedule": "", "backup_keep": "7",
		});
		for (k, v) in extra.as_object().unwrap() {
			form[k] = v.clone();
		}
		form
	}

	#[db_test]
	async fn the_create_form_offers_engines_defaults_and_active_servers(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (s, body) = f.get(&format!("{}/new", f.base())).await;
		assert_eq!(s, StatusCode::OK);
		for engine in ["PostgreSQL", "MySQL", "MongoDB", "Redis"] {
			assert_contains(&body, engine);
		}
		assert_contains(&body, "<strong>18</strong>");
		assert_contains(&body, "<strong>8</strong>");
		assert_contains(&body, "Engine default");
		assert_contains(&body, &format!("value=\"{}\"", f.server));
		assert_contains(&body, "Create database");
	}

	#[db_test]
	async fn creating_queues_the_provision_job_and_opens_the_database(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f
			.create(
				&db,
				"orders",
				json!({ "memory_limit_mb": "512", "cpu_limit": "1.5" }),
			)
			.await;
		assert_eq!(job_kinds(&db, id).await, ["database_provision"]);
		assert_eq!(status_of(&db, id).await, "pending");

		let (s, page) = f.get(&f.url(id)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "orders");
		assert_contains(&page, "PostgreSQL 18");
		assert_contains(&page, "Pending");
		// The provision job is queued, so the page polls and shows its log.
		assert_contains(&page, "data-on-interval__duration.3s");
		assert_contains(&page, &format!("{}/job-log", f.url(id)));
	}

	#[db_test]
	async fn bad_input_is_an_inline_message_and_creates_nothing(db: PgPool) {
		let f = Fixture::new(&db).await;
		for (extra, message) in [
			(json!({ "name": "" }), "name must be 1 to 60 characters"),
			(json!({ "version": "7" }), "unsupported version 7"),
			(
				json!({ "public_port": "80" }),
				"public_port must be between",
			),
			(
				json!({ "public_port": "abc" }),
				"Public port must be a number.",
			),
			(
				json!({ "memory_limit_mb": "lots" }),
				"Memory limit must be a number.",
			),
			(json!({ "server_id": "" }), "Pick a server."),
			(json!({ "engine": "oracle" }), "Pick an engine."),
		] {
			let body = f.post(&f.base(), &f.form("db", extra)).await;
			assert_contains(&body, message);
			assert_not_contains(&body, "window.location");
		}
		assert_eq!(count(&db, "databases").await, 0);
		assert_eq!(count(&db, "jobs").await, 0);
	}

	#[db_test]
	async fn the_password_is_only_in_the_reveal_response(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f
			.create(&db, "orders", json!({ "public_port": "15432" }))
			.await;
		let password = f.password(id).await;

		let (_, page) = f.get(&f.url(id)).await;
		assert_not_contains(&page, &password);
		assert_not_contains(&page, "postgres://");
		assert_contains(&page, &format!("podkit-db-{id}"));
		assert_contains(&page, "5432");
		assert_contains(&page, "podkit");
		assert_contains(&page, ":15432");
		assert_contains(&page, "Reveal password and URLs");
		assert_contains(&page, "<span>User</span>");

		let (s, headers, body) = post_signals(
			&f.app,
			&format!("{}/reveal", f.url(id)),
			Some(&f.cookie),
			&json!({}),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(headers[header::CACHE_CONTROL], "no-store");
		assert_contains(&body, &password);
		assert_contains(
			&body,
			&format!("postgres://podkit:{password}@podkit-db-{id}:5432/podkit"),
		);
		assert_contains(&body, ":15432/podkit");
		assert_contains(&body, "Copy");
		assert_contains(&body, "db-secret");

		// A page loaded after the reveal is as clean as before.
		let (_, again) = f.get(&f.url(id)).await;
		assert_not_contains(&again, &password);
	}

	#[db_test]
	async fn redis_has_no_user_or_database_to_show(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "cache", json!({ "engine": "redis" })).await;
		let (_, page) = f.get(&f.url(id)).await;
		assert_contains(&page, "Redis 8");
		assert_contains(&page, "6379");
		assert_not_contains(&page, "<span>User</span>");
		assert_not_contains(&page, "<span>Database</span>");
	}

	#[db_test]
	async fn a_read_only_member_sees_no_secrets_or_actions(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let password = f.password(id).await;
		set_state(&db, id, "running", Some("c1")).await;
		set_jobs(&db, id, "succeeded").await;
		let carol = f.member("carol").await;

		let (s, _, page) = get_html(&f.app, &f.url(id), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "orders");
		for hidden in ["Reveal password", "Connection", "Restart", "Settings"] {
			assert_not_contains(page_main(&page), hidden);
		}
		let (s, _, page) = get_html(&f.app, &format!("{}/logs", f.url(id)), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Container log");

		for uri in [
			format!("{}/new", f.base()),
			format!("{}/settings", f.url(id)),
		] {
			let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
			assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
			assert_contains(&page, "permission");
		}

		let jobs_before = job_kinds(&db, id).await;
		for (uri, signals) in [
			(f.base(), f.form("forged", json!({}))),
			(format!("{}/reveal", f.url(id)), json!({})),
			(format!("{}/start", f.url(id)), json!({})),
			(format!("{}/stop", f.url(id)), json!({})),
			(format!("{}/restart", f.url(id)), json!({})),
			(
				format!("{}/settings", f.url(id)),
				saved(json!({ "public_port": "15432" })),
			),
			(
				format!("{}/schedule", f.url(id)),
				saved(json!({ "backup_schedule": "0 3 * * *" })),
			),
			(
				format!("{}/delete", f.url(id)),
				json!({ "confirm": "orders", "purge": true }),
			),
		] {
			let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
			assert_contains(&body, "You don't have permission to do that.");
			assert_not_contains(&body, &password);
			assert_not_contains(&body, "window.location");
		}
		assert_eq!(count(&db, "databases").await, 1);
		assert_eq!(job_kinds(&db, id).await, jobs_before);
		assert_eq!(status_of(&db, id).await, "running");
		assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));
	}

	#[db_test]
	async fn lifecycle_buttons_queue_the_matching_job_and_reload(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;

		// While the provision job is queued nothing else can be started.
		let (_, page) = f.get(&f.url(id)).await;
		assert_contains(&page, "disabled");

		set_jobs(&db, id, "succeeded").await;
		set_state(&db, id, "stopped", None).await;
		let (_, page) = f.get(&f.url(id)).await;
		assert_not_contains(&page, "data-on-interval");
		assert_contains(&page, "Stopped");

		let body = f.post(&format!("{}/start", f.url(id)), &json!({})).await;
		assert_contains(&body, &format!("window.location.assign('{}')", f.url(id)));
		set_jobs(&db, id, "succeeded").await;
		f.post(&format!("{}/stop", f.url(id)), &json!({})).await;
		set_jobs(&db, id, "succeeded").await;
		f.post(&format!("{}/restart", f.url(id)), &json!({})).await;
		assert_eq!(
			job_kinds(&db, id).await,
			[
				"database_provision",
				"database_start",
				"database_stop",
				"database_provision"
			]
		);

		// The status region shows the badge and polls while a job is queued.
		let (_, _, body) =
			get_html(&f.app, &format!("{}/status", f.url(id)), Some(&f.cookie)).await;
		assert_contains(&body, "datastar-patch-elements");
		assert_contains(&body, "db-status");
		assert_contains(&body, "data-on-interval__duration.3s");
		set_jobs(&db, id, "succeeded").await;
		set_state(&db, id, "running", Some("c1")).await;
		let (_, _, body) =
			get_html(&f.app, &format!("{}/status", f.url(id)), Some(&f.cookie)).await;
		assert_contains(&body, "Running");
		assert_not_contains(&body, "data-on-interval");
	}

	#[db_test]
	async fn a_schedule_is_checked_inline_with_the_next_run(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let preview = format!("{}/schedule", f.url(id));

		let body = f
			.post(&preview, &saved(json!({ "backup_schedule": "0 3 * * *" })))
			.await;
		assert_contains(&body, "schedule-hint");
		assert_contains(&body, "Next run ");
		assert_contains(&body, ":00 UTC");
		assert_not_contains(&body, "Not a valid schedule");

		let body = f
			.post(&preview, &saved(json!({ "backup_schedule": "every day" })))
			.await;
		assert_contains(&body, "Not a valid schedule");
		assert_not_contains(&body, "Next run");

		let body = f
			.post(&preview, &saved(json!({ "backup_schedule": "  " })))
			.await;
		assert_contains(&body, "No schedule");
	}

	#[db_test]
	async fn saving_an_invalid_cron_changes_nothing_and_a_valid_one_is_stored(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let target = add_backup_target(&db, 8001, f.team, "offsite").await;
		let save = format!("{}/settings", f.url(id));

		let body = f
			.post(
				&save,
				&saved(
					json!({ "backup_schedule": "61 * * * *", "backup_target_id": target.to_string() }),
				),
			)
			.await;
		assert_contains(&body, "Not a valid schedule");
		assert_not_contains(&body, "Settings saved");
		assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));

		let body = f
			.post(
				&save,
				&saved(json!({
					"backup_schedule": "0 3 * * *", "backup_target_id": target.to_string(),
					"backup_keep": "5",
				})),
			)
			.await;
		assert_contains(&body, "Settings saved");
		assert_contains(&body, "Next run ");
		assert_eq!(
			backup_settings(&db, id).await,
			(Some(target), Some("0 3 * * *".to_owned()), 5, true)
		);
		// Backup settings alone do not recreate the container.
		assert_eq!(job_kinds(&db, id).await, ["database_provision"]);

		let body = f.post(&save, &saved(json!({ "backup_keep": "0" }))).await;
		assert_contains(&body, "backup_keep must be between 1 and 365");
		let body = f.post(&save, &saved(json!({ "backup_keep": "x" }))).await;
		assert_contains(&body, "Backups to keep must be a number.");

		let (_, page) = f.get(&save).await;
		assert_contains(&page, "offsite");
		assert_contains(&page, "0 3 * * *");
		assert_contains(&page, "Next run ");
	}

	#[db_test]
	async fn changing_the_port_or_a_limit_recreates_the_container(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let save = format!("{}/settings", f.url(id));

		let body = f
			.post(&save, &saved(json!({ "public_port": "15433" })))
			.await;
		assert_contains(&body, &format!("window.location.assign('{}')", f.url(id)));
		assert_eq!(
			job_kinds(&db, id).await,
			["database_provision", "database_provision"]
		);

		let body = f.post(&save, &saved(json!({ "public_port": "80" }))).await;
		assert_contains(&body, "public_port must be between");
		let body = f
			.post(
				&save,
				&saved(json!({ "public_port": "15433", "cpu_limit": "x" })),
			)
			.await;
		assert_contains(&body, "CPU limit must be a number.");
		assert_eq!(job_kinds(&db, id).await.len(), 2);
	}

	#[db_test]
	async fn another_teams_backup_target_cannot_be_chosen(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let (_, bob_team) = user_with_team(&f.app, "bob").await;
		let theirs = add_backup_target(&db, 8002, bob_team, "bobs").await;

		let body = f
			.post(
				&format!("{}/settings", f.url(id)),
				&saved(json!({ "backup_target_id": theirs.to_string() })),
			)
			.await;
		assert_contains(&body, "That no longer exists.");
		assert_eq!(backup_settings(&db, id).await.0, None);
		let (_, page) = f.get(&format!("{}/settings", f.url(id))).await;
		assert_not_contains(&page, "bobs");
	}

	#[db_test]
	async fn deleting_needs_the_name_and_purges_only_when_asked(db: PgPool) {
		let f = Fixture::new(&db).await;
		let keep = f.create(&db, "keep-data", json!({})).await;
		let purge = f.create(&db, "purge-data", json!({})).await;
		let wrong = f.create(&db, "wrong", json!({})).await;

		let body = f
			.post(
				&format!("{}/delete", f.url(wrong)),
				&json!({ "confirm": "WRONG", "purge": true }),
			)
			.await;
		assert_contains(&body, "The name doesn't match.");
		assert_eq!(status_of(&db, wrong).await, "pending");
		assert_eq!(job_kinds(&db, wrong).await, ["database_provision"]);

		// The checkbox left alone: the data volume stays.
		let body = f
			.post(
				&format!("{}/delete", f.url(keep)),
				&json!({ "confirm": "keep-data", "purge": false }),
			)
			.await;
		assert_contains(
			&body,
			&format!(
				"window.location.assign('/teams/{}/projects/{}')",
				f.team, f.project
			),
		);
		assert_eq!(status_of(&db, keep).await, "deleting");
		assert_eq!(
			job_kinds(&db, keep).await,
			["database_provision", "database_remove"]
		);

		// A request that doesn't mention the box at all doesn't purge either.
		let omitted = f.create(&db, "omitted", json!({})).await;
		f.post(
			&format!("{}/delete", f.url(omitted)),
			&json!({ "confirm": "omitted" }),
		)
		.await;
		assert_eq!(
			job_kinds(&db, omitted).await,
			["database_provision", "database_remove"]
		);

		f.post(
			&format!("{}/delete", f.url(purge)),
			&json!({ "confirm": "purge-data", "purge": true }),
		)
		.await;
		assert_eq!(
			job_kinds(&db, purge).await,
			["database_provision", "database_purge"]
		);
		assert_eq!(status_of(&db, purge).await, "deleting");

		let (_, page) = f.get(&format!("{}/settings", f.url(wrong))).await;
		assert_contains(&page, "Also delete the data");
		assert_contains(&page, "Delete database");
	}

	#[db_test]
	async fn another_teams_database_is_the_404_page_and_cannot_be_changed(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let password = f.password(id).await;
		set_state(&db, id, "running", Some("c1")).await;
		set_jobs(&db, id, "succeeded").await;

		let (bob_token, bob_team) = user_with_team(&f.app, "bob").await;
		let bob = ui_login(&f.app, "bob").await;
		let (_, bob_project) = call(
			&f.app,
			"POST",
			&format!("/api/v1/teams/{bob_team}/projects"),
			Some(&bob_token),
			Some(json!({ "name": "Mine", "slug": "mine" })),
		)
		.await;
		let bob_project = bob_project["id"].as_i64().unwrap();

		// Bob's own team and project, Alice's database id; and Alice's team.
		let bases = [
			format!("/teams/{bob_team}/projects/{bob_project}/databases/{id}"),
			format!("/teams/{bob_team}/projects/{}/databases/{id}", f.project),
			format!("/teams/{}/projects/{}/databases/{id}", f.team, f.project),
		];
		for base in &bases {
			for tail in ["", "/logs", "/settings"] {
				let (s, _, page) = get_html(&f.app, &format!("{base}{tail}"), Some(&bob)).await;
				assert_eq!(s, StatusCode::NOT_FOUND, "{base}{tail}");
				assert_contains(&page, "Not found");
				assert_not_contains(&page, "orders");
				assert_not_contains(&page, &password);
			}
			// Streams and polls answer a Datastar message, never data.
			for tail in ["/status", "/job-log", "/logs/stream"] {
				let (_, _, body) = get_html(&f.app, &format!("{base}{tail}"), Some(&bob)).await;
				assert_not_contains(&body, "orders");
				assert_not_contains(&body, &password);
				assert_not_contains(&body, "db-status");
			}
			for (tail, signals) in [
				("/reveal", json!({})),
				("/start", json!({})),
				("/stop", json!({})),
				("/restart", json!({})),
				("/settings", saved(json!({ "public_port": "15432" }))),
				(
					"/schedule",
					saved(json!({ "backup_schedule": "0 3 * * *" })),
				),
				("/delete", json!({ "confirm": "orders", "purge": true })),
			] {
				let (_, _, body) =
					post_signals(&f.app, &format!("{base}{tail}"), Some(&bob), &signals).await;
				assert_not_contains(&body, &password);
				assert_not_contains(&body, "window.location");
				assert!(
					body.contains("That no longer exists.")
						|| body.contains("permission")
						|| body.contains("Pick"),
					"{base}{tail}: {body}"
				);
			}
		}
		let (s, _, _) = get_html(
			&f.app,
			&format!("/teams/{bob_team}/projects/{bob_project}/databases/new"),
			Some(&bob),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "bob may create in his own team");
		let forged = f.form("planted", json!({}));
		let (_, _, body) = post_signals(
			&f.app,
			&format!("/teams/{bob_team}/projects/{}/databases", f.project),
			Some(&bob),
			&forged,
		)
		.await;
		assert_contains(&body, "That no longer exists.");

		assert_eq!(count(&db, "databases").await, 1);
		assert_eq!(status_of(&db, id).await, "running");
		assert_eq!(job_kinds(&db, id).await, ["database_provision"]);
		assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));
	}

	#[db_test]
	async fn names_are_escaped(db: PgPool) {
		let f = Fixture::new(&db).await;
		let name = "<script>alert(1)</script>";
		let id = f.create(&db, name, json!({})).await;
		let projects = format!("/teams/{}/projects/{}", f.team, f.project);
		for uri in [
			f.url(id),
			format!("{}/logs", f.url(id)),
			format!("{}/settings", f.url(id)),
			projects,
		] {
			let (s, page) = f.get(&uri).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
			assert_not_contains(&page, "<script>alert(1)");
		}
		let (_, _, body) = get_html(&f.app, &format!("{}/rows", f.base()), Some(&f.cookie)).await;
		assert_not_contains(&body, "<script>alert(1)");
	}

	#[db_test]
	async fn the_project_overview_lists_databases_and_polls_while_they_change(db: PgPool) {
		let f = Fixture::new(&db).await;
		let overview = format!("/teams/{}/projects/{}", f.team, f.project);
		let (_, page) = f.get(&overview).await;
		assert_contains(&page, "Nothing in production yet");
		assert_contains(&page, "MySQL");

		let id = f.create(&db, "orders", json!({ "engine": "mysql" })).await;
		let (_, page) = f.get(&overview).await;
		assert_contains(&page, &format!("href=\"{}\"", f.url(id)));
		assert_contains(&page, "MySQL 9");
		assert_contains(&page, "Pending");
		// The environment grid polls its own fragment while a database is changing.
		assert_contains(&page, &format!("{overview}/production/resources"));
		assert_contains(&page, "data-on-interval__duration.3s");

		set_state(&db, id, "running", Some("c1")).await;
		let (_, page) = f.get(&overview).await;
		assert_contains(&page, "Running");
		assert_not_contains(&page, "data-on-interval");
		let (_, _, rows) = get_html(&f.app, &format!("{}/rows", f.base()), Some(&f.cookie)).await;
		assert_contains(&rows, "db-rows");
		assert_contains(&rows, "Running");

		let carol = f.member("carol").await;
		let (_, _, page) = get_html(&f.app, &overview, Some(&carol)).await;
		assert_contains(&page, "orders");
		assert_not_contains(&page, "Create a resource");
	}

	#[db_test]
	async fn the_logs_tab_streams_only_when_there_is_a_container(db: PgPool) {
		let f = Fixture::new(&db).await;
		let id = f.create(&db, "orders", json!({})).await;
		let logs = format!("{}/logs", f.url(id));

		let (_, page) = f.get(&logs).await;
		assert_contains(&page, "There is no container to read a log from.");
		assert_not_contains(&page, "container-log");
		let (_, _, body) = get_html(&f.app, &format!("{logs}/stream"), Some(&f.cookie)).await;
		assert_contains(&body, "database is not running");
		assert_contains(&body, "_live");

		set_state(&db, id, "running", Some("c1")).await;
		let (_, page) = f.get(&logs).await;
		assert_contains(&page, "id=\"container-log\"");
		assert_contains(&page, &format!("{logs}/stream"));

		// Without a job the overview says so instead of opening a stream.
		delete_jobs(&db, id).await;
		let (_, page) = f.get(&f.url(id)).await;
		assert_contains(&page, "No jobs have run yet.");
		assert_not_contains(&page, "id=\"job-log\"");
		let (_, _, body) =
			get_html(&f.app, &format!("{}/job-log", f.url(id)), Some(&f.cookie)).await;
		assert_contains(&body, "_live");
		add_job(&db, id, "database_start", "running").await;
		let (_, page) = f.get(&f.url(id)).await;
		assert_contains(&page, "id=\"job-log\"");
	}

	#[db_test]
	async fn the_json_api_keeps_its_behaviour(db: PgPool) {
		let f = Fixture::new(&db).await;
		let api = format!("/api/v1/teams/{}/projects/{}/databases", f.team, f.project);
		let (s, created) = call(
			&f.app,
			"POST",
			&api,
			Some(&f.token),
			Some(json!({
				"name": "main", "engine": "postgres", "server_id": f.server,
			})),
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED);
		assert_eq!(created["status"], "pending");
		assert_eq!(created["version"], "18");
		assert!(created.get("connection").is_none());
		let id = created["id"].as_i64().unwrap();

		let (s, one) = call(&f.app, "GET", &format!("{api}/{id}"), Some(&f.token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert!(one["connection"]["password"].as_str().unwrap().len() >= 16);
		assert!(
			one["connection"]["internal_url"]
				.as_str()
				.unwrap()
				.starts_with("postgres://podkit:")
		);

		let (s, _) = call(
			&f.app,
			"PUT",
			&format!("{api}/{id}/settings"),
			Some(&f.token),
			Some(json!({ "backup_schedule": "nope" })),
		)
		.await;
		assert!(s.is_client_error(), "{s}");
		let (s, updated) = call(
			&f.app,
			"PUT",
			&format!("{api}/{id}/settings"),
			Some(&f.token),
			Some(json!({ "backup_schedule": "0 3 * * *" })),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert!(updated["backup_next_run_at"].as_i64().is_some());
		let (s, _) = call(
			&f.app,
			"PUT",
			&format!("{api}/{id}/settings"),
			Some(&f.token),
			Some(json!({ "public_port": 15432 })),
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED);

		let (s, list) = call(&f.app, "GET", &api, Some(&f.token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(list.as_array().unwrap().len(), 1);
		assert!(list[0].get("connection").is_none());

		let (s, _) = call(
			&f.app,
			"DELETE",
			&format!("{api}/{id}?purge_data=true"),
			Some(&f.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED);
		assert_eq!(
			job_kinds(&db, id).await,
			["database_provision", "database_provision", "database_purge"]
		);
		assert_eq!(status_of(&db, id).await, "deleting");
	}
}
