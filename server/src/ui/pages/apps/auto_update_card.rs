//! The Auto-update section of an image application, on its Webhooks tab beside "Deploy on a
//! new image": the pull side of the same question, podkit asking the registry instead of being
//! told. One card holds the settings (a form with the save bar) and the state of the last
//! check with Check now; the autodeploy freeze card follows it, since a freeze holds an
//! auto-update the way it holds a push.
//!
//! The page reads and writes through `auto_update::service`, which authorizes and scopes.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::chrome::dot;
use super::git_tab::{check, stamp};
use super::{AppPath, app_url, toast_ok};
use crate::auth::permission::{Action, Resource};
use crate::auto_update::rules::{self, Checked, Interval};
use crate::auto_update::service::{self, SettingsInput, View};
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, SELECT, Size, Status as Shape, Variant, button_class, card, card_content,
	card_section, save_bar, save_bar_saved,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::sse::{self, js_string};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}/auto-update";
const HELP: &str = "ui-hint";

/// The signals of the settings form; the save bar watches all of them.
pub(super) const FIELDS: &[&str] = &[
	"au_enabled",
	"au_interval",
	"au_window",
	"au_start",
	"au_end",
	"au_offset",
];

pub fn router() -> Router<AppState> {
	Router::new()
		.route(APP, post(save))
		.route(&format!("{APP}/check"), post(check_now))
}

fn text_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::Number(n) => n.to_string(),
		Value::String(s) => s,
		_ => String::new(),
	})
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names, reason = "mirrors the form field names")]
struct Signals {
	au_enabled: bool,
	au_interval: String,
	au_window: bool,
	au_start: String,
	au_end: String,
	#[serde(deserialize_with = "text_or_number")]
	au_offset: String,
}

impl Signals {
	fn input(self) -> SettingsInput {
		let (start, end) = if self.au_window {
			(self.au_start, self.au_end)
		} else {
			(String::new(), String::new())
		};
		SettingsInput {
			enabled: self.au_enabled,
			interval: self.au_interval,
			window_start: start,
			window_end: end,
			offset: self.au_offset,
		}
	}
}

fn signals(view: &View) -> Value {
	let s = view.settings();
	json!({
		"au_enabled": s.enabled,
		"au_interval": s.interval.as_str(),
		"au_window": s.window.is_some(),
		"au_start": rules::time_label(s.window.map_or(2 * 60, |w| w.start)),
		"au_end": rules::time_label(s.window.map_or(5 * 60, |w| w.end)),
		"au_offset": s.window.map_or(0, |w| w.offset).to_string(),
	})
}

/// `UTC+02:00` for 120 minutes.
fn offset_label(minutes: i64) -> String {
	let sign = if minutes < 0 { '-' } else { '+' };
	let m = minutes.abs();
	format!("UTC{sign}{:02}:{:02}", m / 60, m % 60)
}

fn fields() -> Markup {
	html! {
		(check("au-enabled", "au_enabled", "Keep this image up to date", Some("podkit checks the tag and redeploys when it names a new image.")))
		div class="flex flex-col gap-4" "data-show"="$au_enabled" {
			label class=(LABEL) {
				"Check"
				select class=(SELECT) "data-bind:au_interval" {
					@for i in Interval::ALL { option value=(i.as_str()) { (i.label()) } }
				}
			}
			(check("au-window", "au_window", "Only in a maintenance window", Some("Scheduled checks, and the deploys they start, happen only inside the window. Check now does not wait for it.")))
			div class="flex flex-col gap-4" "data-show"="$au_window" {
				div class="ui-pair" {
					label class=(LABEL) {
						"From"
						input class=(INPUT) type="time" "data-bind:au_start";
					}
					label class=(LABEL) {
						"Until"
						input class=(INPUT) type="time" "data-bind:au_end";
					}
				}
				label class=(LABEL) {
					"Clock"
					select class=(SELECT) "data-bind:au_offset" {
						@for m in (-24..=28).map(|h| h * 30) { option value=(m) { (offset_label(m)) } }
					}
					span class=(HELP) { "A fixed offset from UTC: it does not follow daylight saving. 22:00 until 04:00 wraps over midnight." }
				}
			}
		}
	}
}

fn result_cell(result: Option<Checked>) -> Markup {
	let shape = match result {
		None => return html! { "Not checked yet" },
		Some(Checked::UpToDate | Checked::Deployed) => Shape::Running,
		Some(Checked::Busy | Checked::Frozen | Checked::OutsideWindow) => Shape::Pending,
		Some(Checked::SkippedBad | Checked::Error) => Shape::Failed,
		Some(Checked::NotRunning) => Shape::Stopped,
	};
	dot(shape, result.map_or("", Checked::label))
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

/// The state of the last check. Its id is what Check now patches.
fn status(base: &str, view: &View, can_deploy: bool) -> Markup {
	let row = view.row.as_ref();
	let enabled = view.settings().enabled;
	let check = format!("@post({})", js_string(&format!("{base}/auto-update/check")));
	html! {
		div id="auto-update-status" class="flex flex-col gap-4" {
			@if view.frozen_now && enabled {
				div class="ui-banner ui-banner-warn" role="status" {
					(crate::ui::components::icon("triangle-alert"))
					div class="ui-banner-body" { "A freeze is open now. A new image waits until it ends." }
				}
			}
			dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
				(fact("Last check", &html! {
					@match row.and_then(|r| r.last_checked_at) {
						Some(t) => time datetime=(stamp(t)) { (stamp(t)) },
						None => "Never",
					}
				}))
				(fact("Result", &result_cell(row.and_then(crate::auto_update::Row::last))))
				(fact("Next check", &html! {
					@match row.and_then(|r| r.next_check_at).filter(|_| enabled) {
						Some(t) => time datetime=(stamp(t)) { (stamp(t)) },
						None => "Off",
					}
				}))
				(fact("The tag names", &html! {
					@match row.and_then(|r| r.remote_digest.as_deref()) {
						Some(d) => code class="font-mono" title=(d) { (rules::short_digest(d)) },
						None => "Not known yet",
					}
				}))
			}
			@if let Some(r) = row.filter(|r| !r.last_detail.is_empty()) {
				p class="text-sm text-muted-foreground break-words" { (r.last_detail) }
			}
			@if let Some(bad) = row.and_then(|r| r.bad_digest.as_deref()) {
				p class="text-sm break-words" {
					"Held back: " code class="font-mono" title=(bad) { (rules::short_digest(bad)) }
					". Its update failed or was rolled back; it is skipped until the tag names another image."
				}
			}
			@if can_deploy && enabled {
				div {
					button type="button" class=(button_class(Variant::Outline, Size::Sm))
						"data-on:click"=(check) "data-indicator:_au_checking" "data-attr:disabled"="$_au_checking" { "Check now" }
				}
			}
		}
	}
}

/// The section: the settings form (or a summary for someone who may not change it) and the
/// state of the last check.
pub(super) fn section(base: &str, view: &View, can_edit: bool, can_deploy: bool) -> Markup {
	let lead = "podkit asks the registry whether the tag names a new image and redeploys it through the normal deploy, so the health gate and automatic rollback still apply. An image whose update fails or is rolled back is skipped until the tag moves.";
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/auto-update"))
	);
	let settings = view.settings();
	html! {
		@if let Some(why) = view.unavailable {
			(card(&html! {
				(card_section("Auto-update", Some(lead), None))
				(card_content(&html! { p class="text-sm text-muted-foreground" { (why) } }))
			}))
		} @else if can_edit {
			form class="flex flex-col gap-6" "data-signals"=(signals(view).to_string()) "data-on:submit"=(save) {
				(card(&html! {
					(card_section("Auto-update", Some(lead), None))
					(card_content(&html! {
						div class="flex flex-col gap-6" {
							div class="flex flex-col gap-4" { (fields()) }
							div class="border-t border-border pt-4" { (status(base, view, can_deploy)) }
						}
					}))
				}))
				(save_bar(FIELDS))
			}
		} @else {
			(card(&html! {
				(card_section("Auto-update", Some(lead), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						p class="text-sm" {
							@if settings.enabled {
								(settings.interval.label())
								@if let Some(w) = settings.window {
									", between " (rules::time_label(w.start)) " and " (rules::time_label(w.end)) " " (offset_label(w.offset))
								}
								"."
							} @else { "Off." }
						}
						(status(base, view, can_deploy))
					}
				}))
			}))
		}
	}
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let axum::extract::Path((_, project, app)) = path;
	match service::save(&s, ctx.user.id, ctx.team.id, project, app, &sig.input()).await {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app);
			sse::patch(&status(
				&base,
				&view,
				ctx.perms.can(Action::Create, Resource::Deployment),
			))
			.and(sse::signals(&json!({ "_error": "" })))
			.and(toast_ok("Saved"))
			.and(save_bar_saved())
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn check_now(State(s): State<AppState>, ctx: TeamCtx, path: AppPath) -> Response {
	let axum::extract::Path((_, project, app)) = path;
	match service::check_now(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app);
			let toast = match view.row.as_ref().and_then(crate::auto_update::Row::last) {
				Some(Checked::Deployed) => "New image found. Deploy started",
				Some(Checked::Error) => "The check failed",
				_ => "Checked",
			};
			sse::patch(&status(
				&base,
				&view,
				ctx.perms.can(Action::Create, Resource::Deployment),
			))
			.and(toast_ok(toast))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::auto_update::Row;

	fn view(row: Option<Row>) -> View {
		View {
			row,
			image: Some("ghcr.io/me/app:latest".to_owned()),
			unavailable: None,
			frozen_now: false,
		}
	}

	fn row() -> Row {
		Row {
			application_id: 1,
			enabled: true,
			check_interval: "hourly".to_owned(),
			window_start_minute: Some(120),
			window_end_minute: Some(300),
			utc_offset_minutes: 60,
			next_check_at: None,
			last_checked_at: None,
			last_result: Some("skipped_bad".to_owned()),
			last_detail: "<script>x</script>".to_owned(),
			remote_digest: Some(format!("sha256:{}", "b".repeat(64))),
			deployment_id: None,
			deployed_digest: None,
			bad_digest: Some(format!("sha256:{}", "b".repeat(64))),
		}
	}

	#[test]
	fn auto_update_signals_cover_every_field_the_save_bar_watches() {
		let map = signals(&view(Some(row())));
		for field in FIELDS {
			assert!(map.get(*field).is_some(), "{field}");
		}
		assert_eq!(
			map.as_object().map_or(0, serde_json::Map::len),
			FIELDS.len()
		);
		assert_eq!(map["au_interval"], "hourly");
		assert_eq!(map["au_start"], "02:00");
		assert_eq!(map["au_offset"], "60");
		let off = signals(&view(None));
		assert_eq!(off["au_enabled"], false);
		assert_eq!(off["au_window"], false);
	}

	#[test]
	fn auto_update_a_window_left_off_is_not_sent() {
		let sig: Signals = serde_json::from_value(json!({
			"au_enabled": true, "au_interval": "6h", "au_window": false,
			"au_start": "02:00", "au_end": "03:00", "au_offset": 0,
		}))
		.unwrap();
		let parsed = sig.input().parse().unwrap();
		assert_eq!(parsed.window, None);
		assert_eq!(parsed.interval, Interval::SixHours);
	}

	#[test]
	fn auto_update_status_escapes_and_offers_check_now_only_to_deployers() {
		let v = view(Some(row()));
		let html = status("/b", &v, true).into_string();
		assert!(!html.contains("<script>x"), "{html}");
		assert!(html.contains("Held back"), "{html}");
		assert!(html.contains("/b/auto-update/check"), "{html}");
		assert!(!status("/b", &v, false).into_string().contains("Check now"));
	}

	#[test]
	fn auto_update_an_application_that_cannot_follow_a_tag_says_why() {
		let mut v = view(None);
		v.unavailable = Some("The image is pinned by digest");
		let html = section("/b", &v, true, true).into_string();
		assert!(html.contains("pinned by digest"), "{html}");
		assert!(!html.contains("au_enabled"), "{html}");
	}
}
