//! Settings → Audit log: who did what, when, with filters, a CSV export, and how long the team
//! keeps its events.
//!
//! The filters are a plain `GET` form, so a filtered view is a link and works without
//! scripts. The table, the paging (`before` is the last event of the page) and the export all
//! read the same query. Reading needs `read audit` (owner and admin by default); the
//! retention form needs `update team` and, like every settings form, ends in the save bar.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use time::macros::format_description;
use time::{Date, Duration, OffsetDateTime, Time};

use super::frame::{settings_body, settings_crumbs};
use crate::components::{
	INPUT, LABEL, SELECT, Size, Tone, Variant, audit_table, button_class, card, card_content,
	card_heading, empty_state, icon, save_bar, save_bar_saved,
};
use crate::extract::TeamCtx;
use crate::layout::app_page;
use crate::sse::{self, js_string};
use crate::{error, shell};
use podkit_core::audit::service::{self, Filter, PAGE, RETENTION_RANGE};
use podkit_core::audit::{ActorKind, Event};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{AuditEventId, TeamId, UserId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/settings/audit",
			get(page).post(set_retention),
		)
		.route("/teams/{team_id}/settings/audit/export", get(export))
}

/// The filter form's fields, as the address carries them. An empty field is "any".
#[derive(Debug, Default, Deserialize)]
struct Params {
	#[serde(default)]
	actor: String,
	#[serde(default)]
	kind: String,
	#[serde(default)]
	action: String,
	#[serde(default)]
	target: String,
	#[serde(default)]
	since: String,
	#[serde(default)]
	until: String,
	#[serde(default)]
	before: String,
}

fn day(raw: &str, name: &str) -> Result<Option<Date>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	Date::parse(raw, format_description!("[year]-[month]-[day]"))
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{name} must be a date like 2026-11-20.")))
}

impl Params {
	fn filter(&self) -> Result<Filter, ServerError> {
		let number = |s: &str, name: &str| -> Result<Option<i64>, ServerError> {
			let s = s.trim();
			if s.is_empty() {
				return Ok(None);
			}
			s.parse()
				.map(Some)
				.map_err(|_| ServerError::Validation(format!("{name} must be a number.")))
		};
		let actor_kind = match self.kind.trim() {
			"" => None,
			k => Some(ActorKind::parse(k).ok_or_else(|| {
				ServerError::Validation("Who is user, token, system or mcp.".to_owned())
			})?),
		};
		let blank = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
		Ok(Filter {
			actor: number(&self.actor, "Person")?.map(UserId),
			actor_kind,
			action: blank(&self.action),
			target_kind: blank(&self.target),
			target_id: None,
			// A day is the whole day, in UTC, the zone the table shows.
			since: day(&self.since, "From")?.map(|d| d.with_time(Time::MIDNIGHT).assume_utc()),
			until: day(&self.until, "To")?
				.map(|d| d.with_time(Time::MIDNIGHT).assume_utc() + Duration::DAY),
			before: number(&self.before, "Page")?.map(AuditEventId),
		})
	}

	/// The address of the same view with `before` set, for the next page.
	fn query(&self, before: Option<AuditEventId>) -> String {
		let mut out = url::form_urlencoded::Serializer::new(String::new());
		for (k, v) in [
			("actor", &self.actor),
			("kind", &self.kind),
			("action", &self.action),
			("target", &self.target),
			("since", &self.since),
			("until", &self.until),
		] {
			if !v.trim().is_empty() {
				out.append_pair(k, v.trim());
			}
		}
		if let Some(b) = before {
			out.append_pair("before", &b.0.to_string());
		}
		out.finish()
	}
}

fn url(team: TeamId) -> String {
	format!("/teams/{team}/settings/audit")
}

fn option(value: &str, label: &str, current: &str) -> Markup {
	html! { option value=(value) selected[value == current] { (label) } }
}

async fn page(State(s): State<AppState>, ctx: TeamCtx, Query(p): Query<Params>) -> Response {
	let base = url(ctx.team.id);
	let filter = match p.filter() {
		Ok(f) => f,
		Err(e) => return error::page(&ctx, &e),
	};
	// One more than a page, to know whether there is a next one.
	let mut events = match service::list(&s.db, ctx.user.id, ctx.team.id, &filter, PAGE + 1).await {
		Ok(e) => e,
		Err(e) => return error::page(&ctx, &e),
	};
	let more = i64::try_from(events.len()).is_ok_and(|n| n > PAGE);
	events.truncate(usize::try_from(PAGE).unwrap_or(50));
	let actions = service::actions(&s.db, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or_default();
	// Who can be filtered on: the people of the team, when this person may see them.
	let people = if ctx.perms.can(Action::Read, Resource::Membership) {
		podkit_core::members::service::list(&s.db, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or_default()
	} else {
		Vec::new()
	};
	let days = service::retention(&s.db, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or(365);
	let can_set = ctx.perms.can(Action::Update, Resource::Team);
	let filtered = !p.query(None).is_empty();
	let export = format!("{base}/export?{}", p.query(None));
	let next = events
		.last()
		.filter(|_| more)
		.map(|e| format!("{base}?{}", p.query(Some(e.id))));

	let content = html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_heading("Audit log", Some("Every change made in this team: who made it, from where, and what changed. Secrets are never recorded, only that they changed.")))
				(card_content(&html! {
					(filters(&p, &people, &actions, &base, &export))
					@if events.is_empty() {
						(empty_state(
							"history",
							if filtered { "Nothing matches." } else { "Nothing yet." },
							Some(if filtered {
								"No event fits these filters."
							} else {
								"Changes made from now on show up here."
							}),
							None,
						))
					} @else {
						(audit_table("audit-events", "Audit events", ctx.team.id, &events))
						@if let Some(next) = &next {
							p class="ui-hint" { a href=(next) class="ui-link" { "Older events" } }
						}
					}
				}))
			}))
			(retention_card(&base, days, can_set))
		}
	};
	let crumbs = settings_crumbs(&ctx, "Audit log");
	app_page(&ctx, "Audit log", &crumbs, &settings_body(&ctx, &content)).into_response()
}

fn filters(
	p: &Params,
	people: &[podkit_core::members::MemberDetail],
	actions: &[String],
	base: &str,
	export: &str,
) -> Markup {
	html! {
		form method="get" action=(base) class="ui-audit-filters" {
			label class=(LABEL) {
				"Person"
				select class=(SELECT) name="actor" {
					(option("", "Anyone", &p.actor))
					@for m in people {
						(option(&m.user_id.0.to_string(), &m.display_name, &p.actor))
					}
				}
			}
			label class=(LABEL) {
				"Through"
				select class=(SELECT) name="kind" {
					(option("", "Anything", &p.kind))
					(option("user", "A person signed in", &p.kind))
					(option("token", "An API token", &p.kind))
					(option("mcp", "An MCP client", &p.kind))
					(option("system", "podkit itself", &p.kind))
				}
			}
			label class=(LABEL) {
				"Action"
				select class=(SELECT) name="action" {
					(option("", "Any action", &p.action))
					@for a in actions {
						(option(a, a, &p.action))
					}
				}
			}
			label class=(LABEL) {
				"Resource"
				input class=(INPUT) type="text" name="target" value=(p.target) maxlength="40"
					placeholder="application, server…" autocomplete="off";
			}
			label class=(LABEL) {
				"From"
				input class=(INPUT) type="date" name="since" value=(p.since);
			}
			label class=(LABEL) {
				"To"
				input class=(INPUT) type="date" name="until" value=(p.until);
			}
			div class="ui-audit-filters-act" {
				button type="submit" class=(button_class(Variant::Default, Size::Md)) { "Filter" }
				a href=(base) class=(button_class(Variant::Ghost, Size::Md)) { "Clear" }
				a href=(export) download class=(button_class(Variant::Outline, Size::Md)) {
					(icon("download")) " Export CSV"
				}
			}
		}
	}
}

fn retention_card(base: &str, days: i32, can_set: bool) -> Markup {
	let signals = format!("{{days: {days}, _error: ''}}");
	card(&html! {
		(card_heading("Retention", Some("How long events are kept. Older ones are deleted by the daily cleanup.")))
		(card_content(&html! {
			@if can_set {
				form class="flex flex-col gap-4" data-signals=(signals)
					"data-on:submit"={ "evt.preventDefault(); @post(" (js_string(base)) ")" } {
					label class=(LABEL) {
						"Keep events for (days)"
						input class={ (INPUT) " max-w-xs" } type="number" inputmode="numeric" required
							min=(RETENTION_RANGE.start()) max=(RETENTION_RANGE.end()) "data-bind:days";
					}
					(save_bar(&["days"]))
				}
			} @else {
				p class="text-sm" { "Events are kept for " (days) " days." }
				p class="mt-2 text-sm text-muted-foreground" { "Only people who can update the team may change this." }
			}
		}))
	})
}

#[derive(Deserialize)]
struct RetentionSignals {
	days: i32,
}

async fn set_retention(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: tower_cookies::Cookies,
	ReadSignals(sig): ReadSignals<RetentionSignals>,
) -> Response {
	match service::set_retention(&s, ctx.user.id, ctx.team.id, sig.days).await {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Retention saved.");
			sse::signals(&serde_json::json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&url(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn export(State(s): State<AppState>, ctx: TeamCtx, Query(p): Query<Params>) -> Response {
	let filter = match p.filter() {
		Ok(f) => f,
		Err(e) => return error::page(&ctx, &e),
	};
	match service::export(&s, ctx.user.id, ctx.team.id, &filter).await {
		Ok(csv) => {
			let stamp = OffsetDateTime::now_utc().date();
			(
				[
					(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
					(
						header::CONTENT_DISPOSITION,
						format!(
							"attachment; filename=\"audit-{}-{stamp}.csv\"",
							ctx.team.slug
						),
					),
					(header::CACHE_CONTROL, "no-store".to_owned()),
				],
				csv,
			)
				.into_response()
		}
		Err(e) => error::page(&ctx, &e),
	}
}

/// The latest events about one resource, for its page's Activity card. A person who may not
/// read the log gets `None`, and the page shows no card.
pub async fn activity(
	s: &AppState,
	ctx: &TeamCtx,
	target_kind: &str,
	target_id: i64,
) -> Option<(Vec<Event>, String)> {
	if !ctx.perms.can(Action::Read, Resource::Audit) {
		return None;
	}
	let filter = Filter {
		target_kind: Some(target_kind.to_owned()),
		target_id: Some(target_id),
		..Filter::default()
	};
	let events = service::list(&s.db, ctx.user.id, ctx.team.id, &filter, 10)
		.await
		.ok()?;
	let all = format!("{}?target={target_kind}", url(ctx.team.id));
	Some((events, all))
}
