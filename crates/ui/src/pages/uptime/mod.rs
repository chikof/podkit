//! Uptime: the team's checks with their state and the last 30 days, the settings of the public
//! status page and the form for a new check. One check's page is [`detail`]; the public page is
//! [`status`].
//!
//! Handlers here call `uptime::service`, which authorizes and scopes: another team's check
//! is a 404 here as in the API. A check's address is shown to the team and nowhere else.

mod detail;
mod form;
pub mod status;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, Tone, badge, card, card_content, card_footer, card_section,
	empty_state, percent_text, save_bar_in, save_bar_saved_in,
};
use crate::extract::TeamCtx;
use crate::layout::{app_page, form_error};
use crate::pages::projects::cards::relative_time;
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::TeamId;
use podkit_core::state::AppState;
use podkit_core::uptime::service;
use podkit_core::uptime::{Check, DayStat, State as CheckState, uptime_percent};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/uptime", get(list_page).post(create))
		.route("/teams/{team_id}/uptime/status", post(save_status))
		.merge(detail::router())
		.merge(status::router())
}

fn list_url(team: TeamId) -> String {
	format!("/teams/{team}/uptime")
}

/// `Up`, `Down` or `Checking`: always text, never color alone.
pub fn state_badge(state: CheckState) -> Markup {
	match state {
		CheckState::Up => badge(Tone::Success, "Up"),
		CheckState::Down => badge(Tone::Danger, "Down"),
		CheckState::Unknown => badge(Tone::Neutral, "Checking"),
	}
}

/// The warning on a check that takes any certificate: text, so it never rests on color.
pub fn tls_skip_badge() -> Markup {
	html! {
		span title="This check does not verify the server's certificate." {
			(badge(Tone::Warning, "Certificate not verified"))
		}
	}
}

fn check_row(team: TeamId, check: &Check, days: &[DayStat], now: OffsetDateTime) -> Markup {
	let href = format!("{}/{}", list_url(team), check.id);
	html! {
		li class="ui-probe-row" {
			div class="ui-probe-head" {
				h2 class="ui-probe-name" { a href=(href) { (check.name) } }
				@if !check.enabled { (badge(Tone::Neutral, "Paused")) }
				@if check.status_visible { (badge(Tone::Progress, "On the status page")) }
				@if check.tls_skip_verify { (tls_skip_badge()) }
				(state_badge(check.state()))
			}
			p class="ui-probe-meta" {
				span class="ui-mono" { (check.target()) }
				@if let Some(at) = check.last_checked_at {
					span { "Checked " (relative_time(at, now)) }
				}
				@if let Some(ms) = check.last_latency_ms { span { (ms) " ms" } }
				@if let Some(p) = uptime_percent(days) { span { (percent_text(p)) " over 30 days" } }
				@if let Some(error) = check.last_error.as_deref().filter(|_| check.state() != CheckState::Up) {
					span { (error) }
				}
			}
		}
	}
}

/// The signals of the status page form; its save bar watches them.
pub(super) const STATUS_FIELDS: &[&str] = &["publish", "title", "auto_checks"];

fn status_card(team: TeamId, page: &service::StatusPage, auto: bool, can_edit: bool) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/status", list_url(team)))
	);
	let path = format!("/status/{}", page.slug);
	card(&html! {
		form id="uptime-status" "data-on:submit"=(post) {
			(card_section("Status page", Some("A light public page for the checks you pick, with no address in it. Anyone with the link can read it."), None))
			(card_content(&html! {
				div class="ui-check" {
					input id="status-publish" type="checkbox" class=(CHECKBOX) disabled[!can_edit] "data-bind:publish";
					label for="status-publish" class="cursor-pointer" { "Publish the status page" }
				}
				@if page.enabled {
					p class="text-sm" { "Live at " a href=(path) class="ui-mono" { (path) } }
				}
				label class=(LABEL) {
					"Title"
					input class=(INPUT) type="text" maxlength="60" autocomplete="off" disabled[!can_edit]
						placeholder="The team's name" "data-bind:title";
				}
				div class="ui-check" {
					input id="status-auto" type="checkbox" class=(CHECKBOX) disabled[!can_edit] "data-bind:auto_checks";
					label for="status-auto" class="cursor-pointer" { "Make a check for every application domain" }
				}
				@if !auto {
					p class="text-sm text-muted-foreground" { "Automatic checks are off. The ones that exist stay until you delete them." }
				}
			}))
			@if can_edit { (save_bar_in("status", STATUS_FIELDS)) }
		}
	})
}

fn add_card(team: TeamId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&list_url(team))
	);
	card(&html! {
		form id="uptime-add" "data-on:submit"=(post) {
			(card_section("Add a check", Some("Podkit asks from the control plane every interval and tells you when the answer changes."), None))
			(card_content(&html! {
				(form::fields(true))
				(form_error())
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add check" }
			}))
		}
	})
}

async fn list_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let now = OffsetDateTime::now_utc();
	let rows = service::checks_with_days(&s.db, ctx.user.id, ctx.team.id, 30, now)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let page = service::status_page(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let auto = service::auto_checks(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Uptime);
	let can_edit = ctx.perms.can(Action::Update, Resource::Uptime);
	let mut signals = form::initial(None);
	if let Some(map) = signals.as_object_mut() {
		map.insert("publish".into(), json!(page.enabled));
		map.insert(
			"title".into(),
			json!(page.title.clone().unwrap_or_default()),
		);
		map.insert("auto_checks".into(), json!(auto));
		map.insert("_error".into(), json!(""));
	}
	let body = html! {
		div class="flex flex-col gap-6" "data-signals"=(signals.to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Uptime" }
					p class="ui-lede" {
						"Is it up, asked from outside. Podkit checks your applications' domains by itself and anything else you add."
					}
				}
			}
			@if rows.is_empty() {
				(empty_state(
					"globe",
					"No checks yet.",
					Some(if can_create { "Add one below. A check is made for each application domain once the application runs." } else { "Ask an owner to add one." }),
					None,
				))
			} @else {
				(card(&html! {
					(card_section("Checks", None, None))
					(card_content(&html! {
						ul class="m-0 grid list-none p-0" {
							@for (check, days) in &rows { (check_row(ctx.team.id, check, days, now)) }
						}
					}))
				}))
			}
			(status_card(ctx.team.id, &page, auto, can_edit))
			@if can_create { (add_card(ctx.team.id)) }
		}
	};
	Ok(app_page(&ctx, "Uptime", &[("Uptime", None)], &body))
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<form::Signals>,
) -> Response {
	match service::create(&s, ctx.user.id, ctx.team.id, &sig.input()).await {
		Ok(check) => {
			sse::redirect(&format!("{}/{}", list_url(ctx.team.id), check.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StatusSignals {
	publish: bool,
	title: String,
	auto_checks: bool,
}

async fn save_status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<StatusSignals>,
) -> Response {
	let saved = async {
		service::set_status_page(&s.db, ctx.user.id, ctx.team.id, sig.publish, &sig.title).await?;
		service::set_auto_checks(&s.db, ctx.user.id, ctx.team.id, sig.auto_checks).await
	}
	.await;
	match saved {
		Ok(()) => save_bar_saved_in("status")
			.and(sse::redirect(&list_url(ctx.team.id)))
			.into_response(),
		Err(e) => sse::signals(&serde_json::json!({ "_status_error": error::message(&e) }))
			.into_response(),
	}
}
