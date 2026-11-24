//! The create form and the fields it shares with the task page's settings form.
//!
//! A task's kind (and, for an exec task, its application) is chosen where the person came
//! from and cannot change afterwards, so the form never offers to turn one kind into the
//! other. The next five runs are shown under the schedule as the person types, read in the
//! task's own time zone.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::task_url;
use crate::components::{
	BUTTON, BUTTON_OUTLINE, CHECKBOX, INPUT, LABEL, SELECT, TEXTAREA, card, card_content,
	card_footer, card_section, icon,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page, form_error};
use crate::pages::apps::{app_url, project_url};
use crate::sse::js_string;
use crate::{pages::cron::cron_url, sse};
use podkit_core::applications::service::{self as applications, ApplicationView};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::cron::limits::{DEFAULT_BACKOFF_S, DEFAULT_TIMEOUT_S};
use podkit_core::cron::service::{self, CreateInput, UpdateInput};
use podkit_core::cron::{Concurrency, CronTask, Kind, schedule};
use podkit_core::environments;
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId};
use podkit_core::projects;
use podkit_core::servers::Server;
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	let base = "/teams/{team_id}/projects/{project_id}/cron";
	Router::new()
		.route(&format!("{base}/new"), get(new_page))
		.route(base, post(create))
		.route(&format!("{base}/preview"), post(preview))
}

/// Zones offered as suggestions; any IANA name is accepted.
const ZONES: [&str; 24] = [
	"UTC",
	"Europe/London",
	"Europe/Berlin",
	"Europe/Paris",
	"Europe/Madrid",
	"Europe/Rome",
	"Europe/Amsterdam",
	"Europe/Warsaw",
	"Europe/Istanbul",
	"Europe/Moscow",
	"America/New_York",
	"America/Chicago",
	"America/Denver",
	"America/Los_Angeles",
	"America/Sao_Paulo",
	"Africa/Cairo",
	"Africa/Johannesburg",
	"Asia/Dubai",
	"Asia/Kolkata",
	"Asia/Bangkok",
	"Asia/Shanghai",
	"Asia/Tokyo",
	"Australia/Sydney",
	"Pacific/Auckland",
];

/// A signal value as text, whether Datastar sent it as a string, a number or nothing.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::String(s) => s,
		Value::Number(n) => n.to_string(),
		_ => String::new(),
	})
}

/// Everything the form posts. Numbers and ids arrive as text and are read here, so a bad
/// one is a message under the form instead of a rejected request.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Signals {
	#[serde(deserialize_with = "lenient")]
	environment: String,
	#[serde(deserialize_with = "lenient")]
	kind: String,
	#[serde(deserialize_with = "lenient")]
	application_id: String,
	#[serde(deserialize_with = "lenient")]
	server_id: String,
	name: String,
	image: String,
	command: String,
	pub(super) schedule: String,
	pub(super) timezone: String,
	#[serde(deserialize_with = "lenient")]
	timeout_s: String,
	#[serde(deserialize_with = "lenient")]
	concurrency: String,
	#[serde(deserialize_with = "lenient")]
	max_retries: String,
	#[serde(deserialize_with = "lenient")]
	retry_backoff_s: String,
	enabled: bool,
}

fn number(raw: &str, what: &str) -> Result<i32, ServerError> {
	raw.trim()
		.parse()
		.map_err(|_| ServerError::Validation(format!("{what} must be a whole number.")))
}

fn id(raw: &str, what: &str) -> Result<Option<i64>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("Pick a valid {what}.")))
}

impl Signals {
	fn concurrency(&self) -> Result<Concurrency, ServerError> {
		Concurrency::parse(self.concurrency.trim())
			.ok_or_else(|| ServerError::Validation("Pick what to do when a run overlaps.".into()))
	}

	fn create_input(&self) -> Result<CreateInput, ServerError> {
		let kind = Kind::parse(self.kind.trim())
			.ok_or_else(|| ServerError::Validation("Pick what the task runs in.".into()))?;
		let environment = id(&self.environment, "environment")?
			.map(EnvironmentId)
			.ok_or_else(|| ServerError::Validation("Pick an environment.".into()))?;
		Ok(CreateInput {
			environment,
			name: self.name.clone(),
			kind,
			application: id(&self.application_id, "application")?.map(ApplicationId),
			server: id(&self.server_id, "server")?.map(ServerId),
			image: Some(self.image.clone()),
			command: self.command.clone(),
			schedule: self.schedule.clone(),
			timezone: self.timezone.clone(),
			timeout_s: number(&self.timeout_s, "The timeout")?,
			concurrency: self.concurrency()?,
			max_retries: number(&self.max_retries, "Retries")?,
			retry_backoff_s: number(&self.retry_backoff_s, "The retry delay")?,
			enabled: self.enabled,
		})
	}

	pub(super) fn update_input(&self, task: &CronTask) -> Result<UpdateInput, ServerError> {
		Ok(UpdateInput {
			name: Some(self.name.clone()),
			image: (task.kind == Kind::Container).then(|| self.image.clone()),
			server: if task.application_id.is_none() && task.kind == Kind::Container {
				id(&self.server_id, "server")?.map(ServerId)
			} else {
				None
			},
			command: Some(self.command.clone()),
			schedule: Some(self.schedule.clone()),
			timezone: Some(self.timezone.clone()),
			timeout_s: Some(number(&self.timeout_s, "The timeout")?),
			concurrency: Some(self.concurrency()?),
			max_retries: Some(number(&self.max_retries, "Retries")?),
			retry_backoff_s: Some(number(&self.retry_backoff_s, "The retry delay")?),
			enabled: Some(self.enabled),
		})
	}
}

/// The names of every signal the settings form binds, for the save bar.
pub(super) const FIELDS: [&str; 12] = [
	"enabled",
	"name",
	"application_id",
	"server_id",
	"image",
	"command",
	"schedule",
	"timezone",
	"timeout_s",
	"concurrency",
	"max_retries",
	"retry_backoff_s",
];

/// The next runs under the schedule field, or why the expression is not valid.
pub(super) fn hint(runs: &Result<Vec<OffsetDateTime>, String>, timezone: &str) -> Markup {
	html! {
		div id="cron-hint" class="text-sm font-normal" role="status" {
			@match runs {
				Ok(runs) => {
					p class="text-xs text-muted-foreground" { "Next runs, in " (timezone) }
					ol class="mt-1 flex flex-col gap-0.5 font-mono text-xs text-muted-foreground" {
						@for run in runs { li { (schedule::local_text(*run, timezone)) } }
					}
				},
				Err(why) => p class="text-destructive" { "Not a valid schedule: " (why) },
			}
		}
	}
}

pub(super) fn upcoming(schedule_text: &str, timezone: &str) -> Result<Vec<OffsetDateTime>, String> {
	schedule::upcoming(
		schedule_text.trim(),
		timezone.trim(),
		OffsetDateTime::now_utc(),
		schedule::PREVIEW_RUNS,
	)
}

/// A preset for the cron field: sets it and asks for the preview.
fn preset(label: &str, cron: &str, preview: &str) -> Markup {
	let click = format!("$schedule = {}; {preview}", js_string(cron));
	html! {
		button type="button" class="ui-chip" "data-on:click"=(click) { (label) }
	}
}

/// What the form needs to list: the environment's applications and the team's servers.
pub(super) struct Choices {
	pub apps: Vec<ApplicationView>,
	pub servers: Vec<Server>,
}

pub(super) async fn choices(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	environment: EnvironmentId,
) -> Result<Choices, ServerError> {
	let apps =
		applications::list(&s.db, ctx.user.id, ctx.team.id, project, Some(environment)).await?;
	// Someone who may not read servers can still pick an application.
	let servers = podkit_core::servers::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or_default();
	Ok(Choices { apps, servers })
}

/// What the fields are drawn from. On the create form the kind (and the application, when
/// the person came from its tab) is fixed by where they came from; on a task's page neither
/// can change, so `editing` locks both.
pub(super) struct Layout<'a> {
	pub ctx: &'a TeamCtx,
	pub project: ProjectId,
	pub kind: Kind,
	pub locked_app: bool,
	pub choices: &'a Choices,
	pub runs: &'a Result<Vec<OffsetDateTime>, String>,
	pub timezone: &'a str,
	pub editing: bool,
}

/// The fields of the create form and of a task's settings.
#[allow(
	clippy::too_many_lines,
	reason = "one form: its fields read best in the order they appear"
)]
pub(super) fn fields(layout: &Layout<'_>) -> Markup {
	let Layout {
		ctx,
		project,
		kind,
		locked_app,
		choices,
		runs,
		timezone,
		editing,
	} = *layout;
	let preview_call = format!(
		"@post({})",
		js_string(&format!("{}/preview", cron_url(ctx.team.id, project)))
	);
	let exec = kind == Kind::Exec;
	html! {
		div class="ui-pair" {
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" name="name" required maxlength="100" autocomplete="off"
					placeholder=(if exec { "Laravel scheduler" } else { "Nightly cleanup" }) "data-bind:name";
			}
			@if exec {
				label class=(LABEL) {
					"Application"
					@if locked_app || editing {
						select class=(SELECT) name="application_id" disabled "data-bind:application_id" {
							button { selectedcontent {} }
							@for app in &choices.apps { option value=(app.id.0) { (app.name) } }
						}
					} @else {
						select class=(SELECT) name="application_id" required "data-bind:application_id" {
							button { selectedcontent {} }
							option value="" { "Pick an application" }
							@for app in &choices.apps { option value=(app.id.0) { (app.name) } }
						}
					}
				}
			} @else {
				label class=(LABEL) {
					"Based on an application (optional)"
					@if editing {
						select class=(SELECT) name="application_id" disabled "data-bind:application_id" {
							button { selectedcontent {} }
							option value="" { "None" }
							@for app in &choices.apps { option value=(app.id.0) { (app.name) } }
						}
					} @else {
						select class=(SELECT) name="application_id" "data-bind:application_id" {
							button { selectedcontent {} }
							option value="" { "None" }
							@for app in &choices.apps { option value=(app.id.0) { (app.name) } }
						}
					}
					span class="ui-hint" { "The task inherits the app's environment variables, networks and server." }
				}
			}
		}
		@if !exec {
			div class="ui-pair" {
				label class=(LABEL) {
					"Image"
					input class=(INPUT) type="text" name="image" autocomplete="off" spellcheck="false" autocapitalize="none"
						placeholder="docker.io/library/busybox" "data-bind:image";
					span class="ui-hint" { "Leave empty to use the application's current image." }
				}
				label class=(LABEL) {
					"Server"
					@if editing && !choices.servers.is_empty() {
						select class=(SELECT) name="server_id" "data-bind:server_id" {
							button { selectedcontent {} }
							@for server in &choices.servers { option value=(server.id.0) { (server.name) } }
						}
					} @else if choices.servers.is_empty() {
						select class=(SELECT) name="server_id" disabled "data-bind:server_id" {
							button { selectedcontent {} }
							option value="" { "The application's server" }
						}
					} @else {
						select class=(SELECT) name="server_id" "data-bind:server_id" {
							button { selectedcontent {} }
							option value="" { "The application's server" }
							@for server in &choices.servers { option value=(server.id.0) { (server.name) } }
						}
					}
					span class="ui-hint" { "Only used when no application is picked." }
				}
			}
		}
		label class=(LABEL) {
			@if exec { "Command, run in the app's container" } @else { "Command" }
			textarea class=(TEXTAREA) name="command" required rows="2" maxlength="4000" spellcheck="false" autocapitalize="none"
				placeholder=(if exec { "php artisan schedule:run" } else { "date" }) "data-bind:command" {}
			span class="ui-hint" { "Split on spaces. Quotes group words; there is no shell, so pipes and && do nothing. Run sh -c '…' for those." }
		}
		div class="ui-pair" {
			label class=(LABEL) {
				"Schedule (cron, five fields)"
				input class={ (INPUT) " font-mono" } type="text" name="schedule" required autocomplete="off" spellcheck="false"
					autocapitalize="none" placeholder="*/5 * * * *" "data-bind:schedule"
					"data-on:input__debounce.400ms"=(preview_call);
			}
			label class=(LABEL) {
				"Time zone"
				input class=(INPUT) type="text" name="timezone" required list="cron-zones" autocomplete="off" spellcheck="false"
					autocapitalize="none" placeholder="UTC" "data-bind:timezone"
					"data-on:input__debounce.400ms"=(preview_call);
				datalist id="cron-zones" {
					@for zone in ZONES { option value=(zone) {} }
				}
			}
		}
		(hint(runs, timezone))
		div class="ui-chips" role="group" aria-label="Schedule presets" {
			(preset("Every minute", "* * * * *", &preview_call))
			(preset("Every 5 minutes", "*/5 * * * *", &preview_call))
			(preset("Hourly", "0 * * * *", &preview_call))
			(preset("Daily at 03:00", "0 3 * * *", &preview_call))
			(preset("Weekly, Sunday 03:00", "0 3 * * 0", &preview_call))
		}
		fieldset class="ui-cron-policy" {
			legend { "If the previous run is still going" }
			div class="ui-choice-grid" {
				@for c in Concurrency::ALL {
					label class="ui-choice" {
						input type="radio" name="concurrency" value=(c.as_str()) "data-bind:concurrency";
						span class="ui-choice-check" aria-hidden="true" { (icon("circle-check")) }
						span class="flex flex-col gap-0.5" {
							span class="text-sm font-medium" { (c.label()) }
							span class="text-xs text-muted-foreground" { (concurrency_hint(c)) }
						}
					}
				}
			}
		}
		div class="ui-pair" {
			label class=(LABEL) {
				"Timeout (seconds)"
				input class=(INPUT) type="text" name="timeout_s" required inputmode="numeric" autocomplete="off" "data-bind:timeout_s";
				span class="ui-hint" { "The run is killed after this long. At most 86400." }
			}
			label class=(LABEL) {
				"Retries"
				input class=(INPUT) type="text" name="max_retries" required inputmode="numeric" autocomplete="off" "data-bind:max_retries";
				span class="ui-hint" { "Extra attempts after a failure, 0 to 10." }
			}
		}
		label class=(LABEL) {
			"Delay before a retry (seconds)"
			input class=(INPUT) type="text" name="retry_backoff_s" required inputmode="numeric" autocomplete="off" "data-bind:retry_backoff_s";
			span class="ui-hint" { "It doubles with every attempt." }
		}
		label class="ui-check" {
			input type="checkbox" class=(CHECKBOX) name="enabled" "data-bind:enabled";
			"Enabled. A paused task keeps its history and does not run."
		}
	}
}

fn concurrency_hint(c: Concurrency) -> &'static str {
	match c {
		Concurrency::Skip => "The new run is recorded as skipped.",
		Concurrency::Queue => "The new run waits until the old one ends.",
		Concurrency::Allow => "Both run at the same time.",
	}
}

/// The signals a form starts with.
pub(super) fn seed(
	task: Option<&CronTask>,
	kind: Kind,
	environment: EnvironmentId,
	app: Option<ApplicationId>,
) -> Value {
	let text = |v: Option<i64>| v.map_or_else(String::new, |v| v.to_string());
	match task {
		Some(t) => json!({
			"environment": t.environment_id.0.to_string(), "kind": t.kind.as_str(),
			"application_id": text(t.application_id.map(|a| a.0)),
			"server_id": t.server_id.0.to_string(),
			"name": t.name, "image": t.image.clone().unwrap_or_default(), "command": t.command,
			"schedule": t.schedule, "timezone": t.timezone, "timeout_s": t.timeout_s.to_string(),
			"concurrency": t.concurrency.as_str(), "max_retries": t.max_retries.to_string(),
			"retry_backoff_s": t.retry_backoff_s.to_string(), "enabled": t.enabled,
			"_error": "",
		}),
		None => json!({
			"environment": environment.0.to_string(), "kind": kind.as_str(),
			"application_id": text(app.map(|a| a.0)), "server_id": "",
			"name": "", "image": "", "command": "",
			"schedule": "*/5 * * * *", "timezone": "UTC",
			"timeout_s": DEFAULT_TIMEOUT_S.to_string(), "concurrency": "skip",
			"max_retries": "0", "retry_backoff_s": DEFAULT_BACKOFF_S.to_string(), "enabled": true,
			"_error": "",
		}),
	}
}

#[derive(Deserialize)]
struct NewQuery {
	environment: Option<EnvironmentId>,
	kind: Option<String>,
	application: Option<ApplicationId>,
}

async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
) -> Result<Markup, Response> {
	let fail = |e| error::page(&ctx, &e);
	if !ctx.perms.can(Action::Create, Resource::Cron) {
		return Err(fail(ServerError::Forbidden));
	}
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let env = environments::service::resolve(&s.db, ctx.team.id, project.id, q.environment)
		.await
		.map_err(fail)?;
	let env = environments::service::get(&s.db, ctx.user.id, ctx.team.id, project.id, env)
		.await
		.map_err(fail)?;
	let kind = q
		.kind
		.as_deref()
		.and_then(Kind::parse)
		.unwrap_or(Kind::Exec);
	let choices = choices(&s, &ctx, project.id, env.id).await.map_err(fail)?;
	let app = q
		.application
		.filter(|a| choices.apps.iter().any(|x| x.id == *a));
	let locked = app.is_some() && kind == Kind::Exec;
	let signals = seed(None, kind, env.id, app).to_string();
	let runs = upcoming("*/5 * * * *", "UTC");
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&cron_url(ctx.team.id, project.id))
	);
	let back = app.map_or_else(
		|| cron_url(ctx.team.id, project.id),
		|a| format!("{}/cron", app_url(ctx.team.id, project.id, a)),
	);
	let title = match kind {
		Kind::Exec => "New scheduled command",
		Kind::Container => "New one-shot container",
	};
	let lede = match kind {
		Kind::Exec => "Runs a command inside a running application's container.",
		Kind::Container => {
			"Starts a container from an image, runs the command and removes it. With an application picked it inherits that app's environment and networks."
		}
	};
	let body = html! {
		div class="ui-wide ui-page-body" "data-signals"=(signals) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { (project.name) " / " (env.name) }
					h1 { (title) }
					p class="ui-lede" { (lede) }
				}
			}
			form id="cron-form" "data-on:submit"=(post) aria-label="New scheduled task" {
				(card(&html! {
					(card_section("Task", Some("Everything can be changed later except what it runs in."), None))
					(card_content(&html! {
						(fields(&Layout { ctx: &ctx, project: project.id, kind, locked_app: locked, choices: &choices, runs: &runs, timezone: "UTC", editing: false }))
						(form_error())
					}))
					(card_footer(&html! {
						a class=(BUTTON_OUTLINE) href=(back) { "Cancel" }
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create task" }
					}))
				}))
			}
		}
	};
	let crumbs: Vec<Crumb<'_>> = vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		("Scheduled tasks", Some(cron_url(ctx.team.id, project.id))),
		(title, None),
	];
	Ok(app_page(&ctx, title, &crumbs, &body))
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		let input = sig.create_input()?;
		service::create(&s.db, ctx.user.id, ctx.team.id, project, input).await
	}
	.await;
	match result {
		Ok(task) => sse::redirect(&task_url(ctx.team.id, project, task.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// The next runs for what is typed, patched under the field.
async fn preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let _ = project;
	match service::preview(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		&sig.schedule,
		&sig.timezone,
	)
	.await
	{
		Ok(runs) => sse::patch(&hint(&runs, sig.timezone.trim())).into_response(),
		Err(e) => error::action(&e),
	}
}
