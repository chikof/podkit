//! One check: its state and facts, the response-time chart, the incidents and the form that
//! edits it. `GET …/{id}/chart` is the 30-second refresh of the chart.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use super::{form, list_url, state_badge};
use crate::auth::permission::{Action, Resource};
use crate::ids::{TeamId, UptimeCheckId};
use crate::metrics::Range;
use crate::state::AppState;
use crate::ui::charts::{Chart, Series, Unit, line_chart};
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, MetricsSection, Size, Tone, Variant, badge, button_class, card,
	card_content, card_footer, card_section, dialog, icon, metrics_body, metrics_section,
	percent_text, range_phrase,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::pages::projects::cards::relative_time;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};
use crate::uptime::probe::days_left;
use crate::uptime::service::{self, Incident};
use crate::uptime::{Check, Kind, LatencyPoint, State as CheckState, runner, uptime_percent};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/uptime/{check_id}", get(page))
		.route("/teams/{team_id}/uptime/{check_id}/chart", get(chart))
		.route("/teams/{team_id}/uptime/{check_id}/edit", post(edit))
		.route("/teams/{team_id}/uptime/{check_id}/run", post(run))
		.route("/teams/{team_id}/uptime/{check_id}/delete", post(remove))
}

fn url(team: TeamId, check: UptimeCheckId) -> String {
	format!("{}/{check}", list_url(team))
}

#[derive(Deserialize)]
struct RangeQuery {
	range: Option<String>,
}

impl RangeQuery {
	/// An unknown or missing range is the default one: the link may be old.
	fn range(&self) -> Range {
		self.range
			.as_deref()
			.and_then(Range::parse)
			.unwrap_or(Range::Day)
	}
}

/// The response-time chart. A failed run is a hole in the line.
fn charts(points: &[LatencyPoint], range: Range, now: i64) -> Markup {
	let from = now - range.duration().whole_seconds();
	let xs: Vec<i64> = points.iter().map(|p| p.at).collect();
	let chart = Chart {
		id: "latency",
		title: "Response time",
		range: range_phrase(range),
		unit: Unit::Millis,
		max: None,
		from,
		to: now,
		xs: &xs,
		series: vec![Series {
			label: "Response time",
			color: 2,
			values: points.iter().map(|p| p.ms).collect(),
		}],
	};
	metrics_body(&line_chart(&chart), now)
}

fn facts(check: &Check, days: &[crate::uptime::DayStat], now: OffsetDateTime) -> Markup {
	html! {
		dl class="ui-dest-facts" {
			dt { "Checks" }
			dd class="ui-mono" { (check.target()) }
			dt { "Every" }
			dd { (check.interval_secs) " s, gives up after " (check.timeout_secs) " s" }
			@if check.kind() == Kind::Http {
				dt { "Expects" }
				dd {
					@if check.expected_status == 0 { "any answer below 400" } @else { "status " (check.expected_status) }
					@if let Some(k) = &check.keyword { ", containing " q { (k) } }
					", " (check.method)
				}
			}
			dt { "State" }
			dd {
				(state_badge(check.state()))
				@if let Some(since) = check.state_since { " since " (relative_time(since, now)) }
			}
			@if let Some(at) = check.last_checked_at {
				dt { "Last run" }
				dd {
					(relative_time(at, now))
					@if let Some(ms) = check.last_latency_ms { ", " (ms) " ms" }
				}
			}
			@if let Some(e) = check.last_error.as_deref().filter(|_| check.state() != CheckState::Up) {
				dt { "Last error" }
				dd { (e) }
			}
			@if let Some(at) = check.tls_expires_at {
				dt { "Certificate" }
				dd {
					@let left = days_left(at, now);
					@if left < 0 { "expired" } @else { "expires in " (left) " day" @if left != 1 { "s" } }
				}
			}
			@if let Some(p) = uptime_percent(days) {
				dt { "Uptime" }
				dd { (percent_text(p)) " over 30 days" }
			}
			dt { "Made by" }
			dd { @if check.auto_key.is_some() { "podkit, for an application domain" } @else { "a person" } }
		}
	}
}

fn incident_list(incidents: &[Incident], now: OffsetDateTime) -> Markup {
	html! {
		@if incidents.is_empty() {
			p class="text-sm text-muted-foreground" { "No incidents. A check goes down after " (crate::uptime::DOWN_AFTER) " failed runs in a row." }
		} @else {
			ul class="m-0 grid list-none p-0" {
				@for i in incidents {
					li class="ui-list-row" {
						div class="ui-list-main" {
							strong { "Down " (relative_time(i.started_at, now)) }
							p {
								@match i.ended_at {
									Some(end) => { "Back up after " (runner::span(end - i.started_at)) },
									None => { (badge(Tone::Danger, "Still down")) },
								}
								@if let Some(e) = &i.error { " · " (e) }
							}
						}
					}
				}
			}
		}
	}
}

fn delete_dialog(team: TeamId, check: &Check) -> Markup {
	let action = format!(
		"@post({})",
		js_string(&format!("{}/delete", url(team, check.id)))
	);
	dialog(
		"delete-check-dialog",
		"Delete this check?",
		Some("Its history and incidents go with it."),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Delete " strong class="break-all" { (check.name) } "?" }
				@if check.auto_key.is_some() {
					p class="text-sm text-muted-foreground" { "Podkit made this check for an application domain. It will not make it again." }
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(action)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete check" }
		},
	)
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, UptimeCheckId)>,
	Query(query): Query<RangeQuery>,
) -> Result<Markup, Response> {
	let now = OffsetDateTime::now_utc();
	let range = query.range();
	let check = service::check(&s.db, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let days = service::check_days(&s.db, ctx.user.id, ctx.team.id, id, 30, now)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let points = service::series(&s.db, ctx.user.id, ctx.team.id, id, range, now)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let incidents = service::incidents(&s.db, ctx.user.id, ctx.team.id, id, 20)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_edit = ctx.perms.can(Action::Update, Resource::Uptime);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Uptime);
	let page_url = url(ctx.team.id, id);
	let refresh_url = format!("{page_url}/chart");
	let section = MetricsSection {
		id: "uptime-metrics",
		page_url: &page_url,
		refresh_url: &refresh_url,
		range,
	};
	let mut signals = form::initial(Some(&check));
	if let Some(map) = signals.as_object_mut() {
		map.insert("_error".into(), json!(""));
		map.insert("_delete_error".into(), json!(""));
	}
	let run = format!("@post({})", js_string(&format!("{page_url}/run")));
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{page_url}/edit"))
	);
	let body = html! {
		div class="flex flex-col gap-6" "data-signals"=(signals.to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { (check.name) }
					p class="ui-lede" { (state_badge(check.state())) @if !check.enabled { " " (badge(Tone::Neutral, "Paused")) } }
				}
				div class="flex flex-wrap gap-2" {
					@if can_edit {
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(run) { (icon("refresh-cw")) "Check now" }
					}
					@if can_delete {
						button type="button" class=(BUTTON_OUTLINE) command="show-modal" commandfor="delete-check-dialog" aria-haspopup="dialog"
							"data-on:click"="el.commandForElement || document.getElementById('delete-check-dialog').showModal()" {
							(icon("trash-2")) "Delete"
						}
					}
				}
			}
			(card(&html! { (card_content(&facts(&check, &days, now))) }))
			(metrics_section(&section, &charts(&points, range, now.unix_timestamp())))
			(card(&html! {
				(card_section("Incidents", None, None))
				(card_content(&incident_list(&incidents, now)))
			}))
			@if can_edit {
				(card(&html! {
					form id="uptime-edit" "data-on:submit"=(post) {
						(card_section("Settings", Some("Changing the address starts the check over."), None))
						(card_content(&html! {
							(form::fields(check.kind() == Kind::Http))
							(form_error())
						}))
						(card_footer(&html! {
							button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save" }
						}))
					}
				}))
			}
			@if can_delete { (delete_dialog(ctx.team.id, &check)) }
		}
	};
	let crumbs = [
		("Uptime", Some(list_url(ctx.team.id))),
		(check.name.as_str(), None),
	];
	Ok(app_page(&ctx, &check.name, &crumbs, &body))
}

/// The refresh: the chart alone, as a Datastar patch.
async fn chart(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, UptimeCheckId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	let now = OffsetDateTime::now_utc();
	match service::series(&s.db, ctx.user.id, ctx.team.id, id, range, now).await {
		Ok(points) => sse::patch(&charts(&points, range, now.unix_timestamp())).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn edit(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, UptimeCheckId)>,
	ReadSignals(sig): ReadSignals<form::Signals>,
) -> Response {
	match service::update(&s, ctx.user.id, ctx.team.id, id, &sig.input()).await {
		Ok(_) => sse::redirect(&url(ctx.team.id, id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn run(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, UptimeCheckId)>,
) -> Response {
	match service::run_now(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => sse::toast("Checking now.").into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, UptimeCheckId)>,
) -> Response {
	match service::remove(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => sse::redirect(&list_url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}
