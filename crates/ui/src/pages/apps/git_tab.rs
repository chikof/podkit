//! The Git tab of an application cloned from a repository: its Autodeploy section.
//!
//! Three cards. The rules (which pushes deploy, whether to wait for the commit's checks, what
//! to post back) are one form with the save bar. The freeze windows are listed with their own
//! small form, since a form cannot hold another. The recent pushes say what became of each
//! push, and let someone who may deploy send a waiting one now.
//!
//! The page reads and writes through `autodeploy::service`, which authorizes and scopes.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::chrome::dot;
use super::previews_card;
use super::{AppPath, Tab, app_url, forbid_unless_allowed, load, page, toast_ok};
use crate::components::{
	CHECKBOX, INPUT, LABEL, SELECT, Size, Status as Shape, TEXTAREA, Tone, Variant, badge,
	button_class, card, card_content, card_section, empty_state, icon, save_bar, save_bar_saved,
	table,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::sse::{self, js_string};
use podkit_core::applications::SourceKind;
use podkit_core::applications::service::ApplicationView;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::autodeploy::rules::{self, Window};
use podkit_core::autodeploy::service::{self, FreezeInput, SettingsInput, View};
use podkit_core::autodeploy::{Freeze, PushRow, Settings, State as PushState};
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::previews::service::{
	self as previews, SettingsInput as PreviewInput, View as PreviewView,
};
use podkit_core::state::AppState;

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}/git";
const HELP: &str = "ui-hint";

/// The signals of the rules form; the save bar watches all of them.
const RULE_FIELDS: &[&str] = &[
	"ad_enabled",
	"ad_branches",
	"ad_tags",
	"ad_paths",
	"ad_wait",
	"ad_timeout",
	"ad_anyway",
	"ad_status",
];

pub fn router() -> Router<AppState> {
	Router::new()
		.route(APP, get(git_page).post(save))
		.route(
			&format!("{APP}/previews/{{preview_id}}/remove"),
			post(previews_card::remove),
		)
		.route(&format!("{APP}/freezes"), post(add_freeze))
		.route(
			&format!("{APP}/freezes/{{freeze_id}}/remove"),
			post(remove_freeze),
		)
		.route(&format!("{APP}/pushes/{{push_id}}/deploy"), post(force))
}

/// A number the browser may send as a number or as the text typed.
fn text_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::Number(n) => n.to_string(),
		Value::String(s) => s,
		_ => String::new(),
	})
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_excessive_bools,
	clippy::struct_field_names,
	reason = "mirrors the form field names"
)]
struct RuleSignals {
	ad_enabled: bool,
	ad_branches: String,
	ad_tags: bool,
	ad_paths: String,
	ad_wait: bool,
	#[serde(deserialize_with = "text_or_number")]
	ad_timeout: String,
	ad_anyway: bool,
	ad_status: bool,
	pv_enabled: bool,
	pv_command: String,
	#[serde(deserialize_with = "text_or_number")]
	pv_max: String,
	#[serde(deserialize_with = "text_or_number")]
	pv_ttl: String,
	#[serde(deserialize_with = "text_or_number")]
	pv_mem: String,
	#[serde(deserialize_with = "text_or_number")]
	pv_cpu: String,
	pv_fork: bool,
}

impl RuleSignals {
	fn previews(&self) -> PreviewInput {
		PreviewInput {
			enabled: self.pv_enabled,
			command: self.pv_command.clone(),
			max_concurrent: self.pv_max.clone(),
			ttl_days: self.pv_ttl.clone(),
			memory_limit_mb: self.pv_mem.clone(),
			cpu_limit: self.pv_cpu.clone(),
			fork_variables: self.pv_fork,
		}
	}

	fn input(self) -> SettingsInput {
		SettingsInput {
			enabled: self.ad_enabled,
			branches: self.ad_branches,
			deploy_tags: self.ad_tags,
			paths: self.ad_paths,
			wait_for_ci: self.ad_wait,
			ci_timeout_min: self.ad_timeout,
			deploy_on_timeout: self.ad_anyway,
			post_status: self.ad_status,
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names, reason = "mirrors the form field names")]
struct FreezeSignals {
	fz_kind: String,
	#[serde(deserialize_with = "text_or_number")]
	fz_sday: String,
	fz_stime: String,
	#[serde(deserialize_with = "text_or_number")]
	fz_eday: String,
	fz_etime: String,
	#[serde(deserialize_with = "text_or_number")]
	fz_offset: String,
	fz_from: String,
	fz_to: String,
	fz_note: String,
}

impl FreezeSignals {
	fn input(self) -> FreezeInput {
		FreezeInput {
			kind: self.fz_kind,
			start_day: self.fz_sday,
			start_time: self.fz_stime,
			end_day: self.fz_eday,
			end_time: self.fz_etime,
			offset: self.fz_offset,
			from: self.fz_from,
			to: self.fz_to,
			note: self.fz_note,
		}
	}
}

fn rule_signals(settings: &Settings) -> Value {
	json!({
		"ad_enabled": settings.enabled,
		"ad_branches": settings.branch_patterns.join("\n"),
		"ad_tags": settings.deploy_tags,
		"ad_paths": settings.path_filters.join("\n"),
		"ad_wait": settings.wait_for_ci,
		"ad_timeout": (settings.ci_timeout_secs / 60).max(1).to_string(),
		"ad_anyway": settings.deploy_on_ci_timeout,
		"ad_status": settings.post_status,
	})
}

fn blank_freeze() -> Value {
	json!({
		"fz_kind": "recurring", "fz_sday": "4", "fz_stime": "16:00", "fz_eday": "0",
		"fz_etime": "08:00", "fz_offset": "0", "fz_from": "", "fz_to": "", "fz_note": "",
	})
}

/// The page's signals: the blank freeze form and the shared error line.
pub(super) fn page_signals() -> Value {
	let mut signals = blank_freeze();
	signals["_error"] = json!("");
	signals
}

const DAYS: [&str; 7] = [
	"Monday",
	"Tuesday",
	"Wednesday",
	"Thursday",
	"Friday",
	"Saturday",
	"Sunday",
];

/// `UTC+02:00` for 120 minutes.
fn offset_label(minutes: i32) -> String {
	let sign = if minutes < 0 { '-' } else { '+' };
	let m = minutes.abs();
	format!("UTC{sign}{:02}:{:02}", m / 60, m % 60)
}

pub(super) fn stamp(t: OffsetDateTime) -> String {
	let t = t.to_offset(time::UtcOffset::UTC);
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

pub(super) fn check(id: &str, signal: &str, label: &str, help: Option<&str>) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			div class="ui-check" {
				input id=(id) type="checkbox" class=(CHECKBOX) "data-bind"=(signal);
				label for=(id) class="cursor-pointer" { (label) }
			}
			@if let Some(help) = help {
				span class={ (HELP) " pl-6" } { (help) }
			}
		}
	}
}

/// The rules as a form.
fn rules_fields(has_provider: bool) -> Markup {
	html! {
		(check("ad-enabled", "ad_enabled", "Deploy when this repository is pushed to", None))
		div class="flex flex-col gap-4" "data-show"="$ad_enabled" {
			label class=(LABEL) {
				"Branches"
				textarea class=(TEXTAREA) rows="2" placeholder="main\nrelease/*" autocomplete="off" autocapitalize="none"
					autocorrect="off" spellcheck="false" "data-bind:ad_branches" {}
				span class=(HELP) {
					"One pattern per line; " code class="font-mono" { "*" } " stays in one path segment, " code class="font-mono" { "**" }
					" crosses them. Empty means the branch this application follows. A push to one of these deploys the application's own ref."
				}
			}
			(check("ad-tags", "ad_tags", "Tag pushes count too", Some("Tags are matched against the same patterns.")))
			label class=(LABEL) {
				"Only when these files change"
				textarea class=(TEXTAREA) rows="3" placeholder="api/**\n!docs/**" autocomplete="off" autocapitalize="none"
					autocorrect="off" spellcheck="false" "data-bind:ad_paths" {}
				span class=(HELP) {
					"One pattern per line, " code class="font-mono" { "!" } " in front to leave files out. "
					"A pattern with no slash matches that name at any depth. Empty means every push deploys."
				}
			}
			fieldset class="m-0 flex min-w-0 flex-col gap-3 border-0 p-0" {
				legend class="mb-2 text-sm font-medium" { "Commit checks" }
				@if has_provider {
					(check("ad-wait", "ad_wait", "Wait for the commit's checks to pass", Some("GitHub check suites and statuses, GitLab pipelines, Gitea statuses. A failed check means nothing deploys and the team is told.")))
					div class="flex flex-col gap-3" "data-show"="$ad_wait" {
						label class=(LABEL) {
							"Wait up to (minutes)"
							input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]{1,4}" autocomplete="off" enterkeyhint="done"
								"data-bind:ad_timeout";
						}
						(check("ad-anyway", "ad_anyway", "When the wait runs out, deploy anyway", Some("Off: the push is dropped and the team is told.")))
					}
					(check("ad-status", "ad_status", "Post the deploy's state on the commit", Some("Pending, success or failure, with a link back to podkit.")))
				} @else {
					p class="text-sm text-muted-foreground" {
						"This application is not cloned through a git provider, so podkit can't read checks or post a status. "
						"Connect a provider under Git providers and create the application from it."
					}
				}
			}
		}
	}
}

fn rules_summary(settings: Option<&Settings>) -> Markup {
	let s = settings.cloned().unwrap_or_default();
	html! {
		dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
			(fact("Autodeploy", &html! { @if s.enabled { "On" } @else { "Off" } }))
			(fact("Branches", &html! {
				@if s.branch_patterns.is_empty() { "The branch the application follows" }
				@else { code class="font-mono break-words" { (s.branch_patterns.join(", ")) } }
			}))
			(fact("Path filters", &html! {
				@if s.path_filters.is_empty() { "None" }
				@else { code class="font-mono break-words" { (s.path_filters.join(", ")) } }
			}))
			(fact("Checks", &html! {
				@if s.wait_for_ci { "Waits up to " (s.ci_timeout_secs / 60) " min" } @else { "Not waited for" }
			}))
		}
	}
}

fn freeze_text(f: &Freeze) -> String {
	match f.window() {
		Some(Window::Recurring { start, end, offset }) => format!(
			"Every week, {} to {} ({})",
			rules::minute_label(start),
			rules::minute_label(end),
			offset_label(i32::try_from(offset).unwrap_or(0))
		),
		Some(Window::Once { from, to }) => format!("{} to {}", stamp(from), stamp(to)),
		None => "Unreadable freeze".to_owned(),
	}
}

fn freeze_form(base: &str) -> Markup {
	let submit = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/freezes"))
	);
	html! {
		form class="flex flex-col gap-4 border-t border-border pt-4" "data-on:submit"=(submit) {
			fieldset class="m-0 flex min-w-0 flex-col gap-3 border-0 p-0" {
				legend class="mb-2 text-sm font-medium" { "Add a freeze" }
				div class="flex flex-wrap gap-x-6 gap-y-2" {
					label class="flex items-center gap-2 text-sm font-normal" {
						input type="radio" name="fz_kind" value="recurring" "data-bind:fz_kind";
						"Every week"
					}
					label class="flex items-center gap-2 text-sm font-normal" {
						input type="radio" name="fz_kind" value="once" "data-bind:fz_kind";
						"Once"
					}
				}
			}
			div class="flex flex-col gap-4" "data-show"="$fz_kind === 'recurring'" {
				div class="ui-pair" {
					label class=(LABEL) {
						"From"
						select class=(SELECT) "data-bind:fz_sday" {
							@for (i, d) in DAYS.iter().enumerate() { option value=(i) { (d) } }
						}
					}
					label class=(LABEL) {
						"at"
						input class=(INPUT) type="time" "data-bind:fz_stime";
					}
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Until"
						select class=(SELECT) "data-bind:fz_eday" {
							@for (i, d) in DAYS.iter().enumerate() { option value=(i) { (d) } }
						}
					}
					label class=(LABEL) {
						"at"
						input class=(INPUT) type="time" "data-bind:fz_etime";
					}
				}
				span class=(HELP) { "Friday 16:00 until Monday 08:00 wraps over the weekend." }
			}
			div class="ui-pair" "data-show"="$fz_kind === 'once'" {
				label class=(LABEL) {
					"Starts"
					input class=(INPUT) type="datetime-local" "data-bind:fz_from";
				}
				label class=(LABEL) {
					"Ends"
					input class=(INPUT) type="datetime-local" "data-bind:fz_to";
				}
			}
			div class="ui-pair" {
				label class=(LABEL) {
					"Clock"
					select class=(SELECT) "data-bind:fz_offset" {
						@for m in (-24..=28).map(|h| h * 30) { option value=(m) { (offset_label(m)) } }
					}
					span class=(HELP) { "A fixed offset from UTC: it does not follow daylight saving." }
				}
				label class=(LABEL) {
					"Note (optional)"
					input class=(INPUT) type="text" maxlength="120" autocomplete="off" "data-bind:fz_note";
				}
			}
			(form_error())
			div { button type="submit" class=(button_class(Variant::Outline, Size::Md)) { "Add freeze" } }
		}
	}
}

/// The freeze card. Its id is what the handlers patch.
pub(super) fn freeze_card(
	base: &str,
	freezes: &[Freeze],
	frozen_now: bool,
	can_edit: bool,
) -> Markup {
	let now = OffsetDateTime::now_utc();
	html! {
		div id="autodeploy-freezes" {
			(card(&html! {
				(card_section(
					"Freeze windows",
					Some("A push or an image auto-update during a freeze waits and deploys when the freeze ends, unless someone with deploy rights sends it sooner."),
					None,
				))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						@if frozen_now {
							div class="ui-banner ui-banner-warn" role="status" {
								(icon("triangle-alert"))
								div class="ui-banner-body" { "A freeze is open now. Pushes and auto-updates wait." }
							}
						}
						@if freezes.is_empty() {
							p class="text-sm text-muted-foreground" { "No freezes. Deploys from pushes and auto-update start at once." }
						} @else {
							ul class="m-0 flex list-none flex-col gap-2 p-0" {
								@for f in freezes {
									li class="flex flex-wrap items-center justify-between gap-3 text-sm" {
										span class="flex min-w-0 flex-col gap-1" {
											span class="font-medium break-words" { (freeze_text(f)) }
											@if !f.note.is_empty() { span class="text-muted-foreground break-words" { (f.note) } }
										}
										span class="flex items-center gap-3" {
											@if f.window().is_some_and(|w| w.open_at(now)) { (badge(Tone::Warning, "Open now")) }
											@if can_edit {
												button type="button" class=(button_class(Variant::Outline, Size::Sm))
													"data-on:click"=(format!("@post({})", js_string(&format!("{base}/freezes/{}/remove", f.id)))) { "Remove" }
											}
										}
									}
								}
							}
						}
						@if can_edit { (freeze_form(base)) }
					}
				}))
			}))
		}
	}
}

fn state_cell(state: Option<PushState>) -> Markup {
	let (shape, word) = match state {
		Some(PushState::WaitingCi) => (Shape::Pending, "Waiting for checks"),
		Some(PushState::Frozen) => (Shape::Pending, "Held by a freeze"),
		Some(PushState::Deployed) => (Shape::Running, "Deployed"),
		Some(PushState::Superseded) => (Shape::Stopped, "Replaced"),
		Some(PushState::Skipped) => (Shape::Stopped, "Skipped"),
		Some(PushState::Filtered) => (Shape::Stopped, "No matching files"),
		Some(PushState::CiFailed) => (Shape::Failed, "Checks failed"),
		Some(PushState::CiTimeout) => (Shape::Failed, "Checks timed out"),
		None => (Shape::Stopped, "Unknown"),
	};
	dot(shape, word)
}

/// The recent pushes card. Its id is what the handlers patch.
fn pushes_card(base: &str, pushes: &[PushRow], can_deploy: bool) -> Markup {
	let rows: Vec<Vec<Markup>> = pushes
		.iter()
		.map(|p| {
			let short: String = p.commit_sha.chars().take(7).collect();
			let waiting = p.state().is_some_and(PushState::pending);
			vec![
				html! {
					span class="flex min-w-0 flex-col gap-1" {
						span { @if short.is_empty() { "No commit id" } @else { code class="font-mono" { (short) } } }
						@if !p.message.is_empty() { span class="text-muted-foreground break-words" { (p.message) } }
					}
				},
				html! { code class="font-mono break-all" { (p.git_ref.strip_prefix("refs/heads/").or_else(|| p.git_ref.strip_prefix("refs/tags/")).unwrap_or(&p.git_ref)) } },
				html! {
					span class="flex flex-col gap-1" {
						(state_cell(p.state()))
						@if !p.detail.is_empty() { span class="text-muted-foreground break-words" { (p.detail) } }
					}
				},
				html! { time datetime=(stamp(p.created_at)) { (stamp(p.created_at)) } },
				html! {
					@if waiting && can_deploy {
						button type="button" class=(button_class(Variant::Outline, Size::Sm))
							"data-on:click"=(format!("@post({})", js_string(&format!("{base}/pushes/{}/deploy", p.id)))) { "Deploy now" }
					}
				},
			]
		})
		.collect();
	html! {
		div id="autodeploy-pushes" {
			(card(&html! {
				(card_section("Recent pushes", Some("What became of the latest pushes to this repository."), None))
				(card_content(&html! {
					@if rows.is_empty() {
						(empty_state("git-branch", "No pushes yet", Some("Pushes that reach this application show up here with what was done about them."), None))
					} @else {
						(table("autodeploy-pushes-table", "Recent pushes", &["Commit", "Branch", "Result", "When", ""], &rows))
					}
				}))
			}))
		}
	}
}

fn rules_card(
	base: &str,
	view: &View,
	previews: &PreviewView,
	can_edit: bool,
	has_provider: bool,
) -> Markup {
	let mut initial = rule_signals(view.settings.as_ref().unwrap_or(&Settings::form_default()));
	if let (Some(map), Some(extra)) = (
		initial.as_object_mut(),
		previews_card::signals(&previews.settings)
			.as_object()
			.cloned(),
	) {
		map.extend(extra);
	}
	let save = format!("evt.preventDefault(); @post({})", js_string(base));
	let fields: Vec<&str> = RULE_FIELDS
		.iter()
		.chain(previews_card::FIELDS)
		.copied()
		.collect();
	html! {
		@if can_edit {
			form class="flex flex-col gap-6" "data-signals"=(initial.to_string()) "data-on:submit"=(save) {
				(card(&html! {
					(card_section(
						"Autodeploy",
						Some("What a push to the repository does. Without these rules a push to the followed branch deploys at once."),
						None,
					))
					(card_content(&html! { div class="flex flex-col gap-4" { (rules_fields(has_provider)) } }))
				}))
				(previews_card::settings_card(previews, true))
				(save_bar(&fields))
			}
		} @else {
			(card(&html! {
				(card_section("Autodeploy", None, None))
				(card_content(&rules_summary(view.settings.as_ref())))
			}))
			(previews_card::settings_card(previews, false))
		}
	}
}

fn tab_content(
	base: &str,
	app: &ApplicationView,
	view: &View,
	previews: &PreviewView,
	can_edit: bool,
	can_deploy: bool,
	can_remove: bool,
) -> Markup {
	html! {
		div class="flex flex-col gap-6" data-signals=(page_signals().to_string()) {
			(rules_card(base, view, previews, can_edit, app.git_provider_id.is_some()))
			(freeze_card(base, &view.freezes, view.frozen_now, can_edit))
			(pushes_card(base, &view.pushes, can_deploy))
			@if previews.has_provider || !previews.previews.is_empty() {
				(previews_card::list_card(base, &previews.previews, can_remove))
			}
		}
	}
}

async fn git_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
) -> Result<axum::response::Response, Response> {
	use axum::response::IntoResponse as _;
	let Path((_, project, app)) = path;
	forbid_unless_allowed(&ctx, Tab::Git)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	if app.source_kind != SourceKind::Git {
		return Err(error::page(&ctx, &ServerError::ApplicationNotFound));
	}
	let view = service::get(&s, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let previews = previews::get(&s, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id) + "/git";
	let content = tab_content(
		&base,
		&app,
		&view,
		&previews,
		ctx.perms.can(Action::Update, Resource::Application),
		ctx.perms.can(Action::Create, Resource::Deployment),
		ctx.perms.can(Action::Delete, Resource::Application),
	);
	Ok(page(&s, &ctx, &project, &app, Tab::Git, &content)
		.await
		.into_response())
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<RuleSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let preview_input = sig.previews();
	let result = async {
		service::save(&s, ctx.user.id, ctx.team.id, project, app, &sig.input()).await?;
		previews::save(&s, ctx.user.id, ctx.team.id, project, app, &preview_input).await
	}
	.await;
	match result {
		Ok(_) => sse::signals(&json!({ "_error": "" }))
			.and(toast_ok("Saved"))
			.and(save_bar_saved())
			.into_response(),
		Err(e) => error::action(&e),
	}
}

/// What the freeze and push cards show after a change.
async fn refresh(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<(String, View), ServerError> {
	let view = service::get(s, ctx.user.id, ctx.team.id, project, app).await?;
	Ok((app_url(ctx.team.id, project, app) + "/git", view))
}

async fn add_freeze(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<FreezeSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		service::add_freeze(&s, ctx.user.id, ctx.team.id, project, app, &sig.input()).await?;
		refresh(&s, &ctx, project, app).await
	}
	.await;
	match result {
		Ok((base, view)) => sse::patch(&freeze_card(
			&base,
			&view.freezes,
			view.frozen_now,
			ctx.perms.can(Action::Update, Resource::Application),
		))
		.and(sse::signals(&page_signals()))
		.and(toast_ok("Freeze added"))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove_freeze(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, freeze)): Path<(TeamId, ProjectId, ApplicationId, i64)>,
) -> Response {
	let result = async {
		service::remove_freeze(&s, ctx.user.id, ctx.team.id, project, app, freeze).await?;
		refresh(&s, &ctx, project, app).await
	}
	.await;
	match result {
		Ok((base, view)) => sse::patch(&freeze_card(
			&base,
			&view.freezes,
			view.frozen_now,
			ctx.perms.can(Action::Update, Resource::Application),
		))
		.and(toast_ok("Freeze removed"))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn force(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, push)): Path<(TeamId, ProjectId, ApplicationId, i64)>,
) -> Response {
	let result = async {
		service::force(&s, ctx.user.id, ctx.team.id, project, app, push).await?;
		refresh(&s, &ctx, project, app).await
	}
	.await;
	match result {
		Ok((base, view)) => sse::patch(&pushes_card(
			&base,
			&view.pushes,
			ctx.perms.can(Action::Create, Resource::Deployment),
		))
		.and(toast_ok("Deploy started"))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn git_tab_signals_cover_every_field_the_save_bar_watches() {
		let map = rule_signals(&Settings::form_default());
		for field in RULE_FIELDS {
			assert!(map.get(*field).is_some(), "{field}");
		}
		assert_eq!(
			map.as_object().map_or(0, serde_json::Map::len),
			RULE_FIELDS.len()
		);
		assert_eq!(map["ad_timeout"], "15");
		assert_eq!(map["ad_status"], true);
	}

	#[test]
	fn git_tab_a_blank_freeze_is_friday_to_monday() {
		let f = blank_freeze();
		let parsed: FreezeSignals = serde_json::from_value(f).unwrap();
		let n = parsed.input().parse().unwrap();
		assert_eq!(n.kind, "recurring");
		assert_eq!(n.start_minute, Some(4 * 24 * 60 + 16 * 60));
		assert_eq!(n.end_minute, Some(8 * 60));
	}

	#[test]
	fn git_tab_offset_labels() {
		assert_eq!(offset_label(0), "UTC+00:00");
		assert_eq!(offset_label(-330), "UTC-05:30");
		assert_eq!(offset_label(120), "UTC+02:00");
	}

	#[test]
	fn git_tab_pushes_escape_the_commit_message_and_detail() {
		let row = PushRow {
			id: 1,
			application_id: 1,
			git_ref: "refs/heads/<b>x</b>".to_owned(),
			commit_sha: "abcdef1234".to_owned(),
			message: "<script>alert(1)</script>".to_owned(),
			state: "ci_failed".to_owned(),
			detail: "<img src=x>".to_owned(),
			deployment_id: None,
			ci_deadline: None,
			created_at: OffsetDateTime::now_utc(),
		};
		let html = pushes_card("/b", &[row], true).into_string();
		assert!(
			!html.contains("<script>") && !html.contains("<img"),
			"{html}"
		);
		assert!(html.contains("&lt;script&gt;"), "{html}");
		assert!(html.contains("abcdef1"), "{html}");
		assert!(
			!html.contains("Deploy now"),
			"a finished push has no button: {html}"
		);
	}

	#[test]
	fn git_tab_a_waiting_push_has_a_deploy_button_only_for_those_who_may() {
		let mut row = PushRow {
			id: 7,
			application_id: 1,
			git_ref: "refs/heads/main".to_owned(),
			commit_sha: "abcdef1234".to_owned(),
			message: String::new(),
			state: "frozen".to_owned(),
			detail: String::new(),
			deployment_id: None,
			ci_deadline: None,
			created_at: OffsetDateTime::now_utc(),
		};
		assert!(
			pushes_card("/b", std::slice::from_ref(&row), true)
				.into_string()
				.contains("/b/pushes/7/deploy")
		);
		assert!(
			!pushes_card("/b", std::slice::from_ref(&row), false)
				.into_string()
				.contains("Deploy now")
		);
		row.state = "deployed".to_owned();
		assert!(
			!pushes_card("/b", &[row], true)
				.into_string()
				.contains("Deploy now")
		);
	}
}
