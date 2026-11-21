//! Settings → Usage & limits: what the team uses against its limits, how its usage went over
//! the last 30 days, a CSV export, and, for the instance administrator, the form that sets
//! the limits.
//!
//! Every team member may read it (`read quota`). The limits form is shown only to the
//! administrator and the service refuses anyone else, so a forged request changes nothing.
//! The history is the daily sum of the hourly `usage_records`; an hour without usage has no
//! record and shows as zero.

use std::collections::{BTreeMap, HashMap};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::Value;
use time::{Duration, OffsetDateTime, Time};

use super::frame::{settings_body, settings_crumbs};
use crate::error::ServerError;
use crate::ids::TeamId;
use crate::quotas::service::{self, Bucket, Grain, Row};
use crate::quotas::{Group, Level, QuotaKind};
use crate::state::AppState;
use crate::ui::charts::{Chart, Series, Unit, line_chart};
use crate::ui::components::{
	INPUT, LABEL, MeterLevel, Size, Tone, Variant, button_class, card, card_content, card_heading,
	icon, save_bar, save_bar_saved, usage_meter,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::sse::{self, js_string};
use crate::ui::{error, shell};

const TITLE: &str = "Usage & limits";
/// Days of history the page draws.
const DAYS: i64 = 30;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/settings/usage",
			get(page).post(set_limits),
		)
		.route("/teams/{team_id}/settings/usage/export", get(export))
}

fn url(team: TeamId) -> String {
	format!("/teams/{team}/settings/usage")
}

const fn meter_level(level: Level) -> MeterLevel {
	match level {
		Level::Ok => MeterLevel::Ok,
		Level::Warn => MeterLevel::Warn,
		Level::Full => MeterLevel::Full,
	}
}

/// The rows of one group as meters.
fn group_block(group: Group, rows: &[Row]) -> Markup {
	let mine: Vec<&Row> = rows.iter().filter(|r| r.kind.group() == group).collect();
	html! {
		div class="ui-usage-group" {
			h3 { (group.label()) }
			div class="ui-usage-list" {
				@for r in mine {
					(usage_meter(
						r.kind.label(),
						&amount(r.kind, r.used),
						r.limit.map(|l| amount(r.kind, l)).as_deref(),
						r.percent(),
						meter_level(r.level()),
					))
				}
			}
		}
	}
}

/// "3 applications" for a count, "1.5 GB" for the rest.
fn amount(kind: QuotaKind, value: i64) -> String {
	match kind {
		QuotaKind::CpuMillis
		| QuotaKind::MemoryMb
		| QuotaKind::DiskMb
		| QuotaKind::ModelTokens
		| QuotaKind::BuildMinutes => kind.show(value),
		_ => format!("{value} {}", kind.noun()),
	}
}

/// Midnight UTC of the day `at` is in.
fn day_floor(at: OffsetDateTime) -> OffsetDateTime {
	at.replace_time(Time::MIDNIGHT)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let rows = match service::overview(&s.db, ctx.user.id, ctx.team.id).await {
		Ok(r) => r,
		Err(e) => return error::page(&ctx, &e),
	};
	let admin = service::is_instance_admin(&s.db, ctx.user.id)
		.await
		.unwrap_or(false);
	let now = OffsetDateTime::now_utc();
	let first_day = day_floor(now) - Duration::days(DAYS - 1);
	let buckets = service::history(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		Grain::Day,
		first_day,
		now + Duration::HOUR,
		DAYS + 2,
	)
	.await
	.unwrap_or_default();
	let base = url(ctx.team.id);

	let content = html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_heading(
					"Usage and limits",
					Some("What this team uses now against the limits set for it. Passing 80% of a limit sends a notification; at the limit, creating and deploying more is refused until something is freed or the limit is raised."),
				))
				(card_content(&html! {
					@for group in [Group::Resources, Group::Capacity, Group::Monthly] {
						(group_block(group, &rows))
					}
					@if !admin {
						p class="mt-4 text-sm text-muted-foreground" {
							"Limits are set by the administrator of this podkit instance."
						}
					}
				}))
			}))
			(history_card(&base, &buckets, first_day, now))
			@if admin {
				(limits_card(&base, &rows))
			}
		}
	};
	let crumbs = settings_crumbs(&ctx, TITLE);
	app_page(&ctx, TITLE, &crumbs, &settings_body(&ctx, &content)).into_response()
}

// --- history ---------------------------------------------------------------------------------

fn history_card(
	base: &str,
	buckets: &[Bucket],
	first_day: OffsetDateTime,
	now: OffsetDateTime,
) -> Markup {
	let xs: Vec<i64> = (0..DAYS)
		.map(|i| (first_day + Duration::days(i)).unix_timestamp())
		.collect();
	let by_day: BTreeMap<i64, &Bucket> =
		buckets.iter().map(|b| (b.at.unix_timestamp(), b)).collect();
	let series = |label: &'static str, f: &dyn Fn(&Bucket) -> f64| Series {
		label,
		color: 1,
		values: xs
			.iter()
			.map(|x| Some(by_day.get(x).map_or(0.0, |b| f(b))))
			.collect(),
	};
	let chart = |id: &'static str, title: &'static str, unit: Unit, s: Series<'static>| {
		line_chart(&Chart {
			id,
			title,
			range: "last 30 days",
			unit,
			max: None,
			from: xs[0],
			to: now.unix_timestamp(),
			xs: &xs,
			series: vec![s],
		})
	};
	card(&html! {
		(card_heading("History", Some("Metered every hour from the samples podkit already takes, shown per day (UTC).")))
		(card_content(&html! {
			@if buckets.is_empty() {
				p class="text-sm text-muted-foreground" {
					"Nothing metered yet. Usage is written once an hour, so the first figures appear within an hour of something running."
				}
			} @else {
				div class="flex flex-col gap-4" "data-signals"="{_tables: false}" {
					div class="flex flex-wrap gap-3" {
						button type="button" class=(button_class(Variant::Outline, Size::Sm)) aria-pressed="false"
							"data-on:click"="$_tables = !$_tables"
							"data-attr:aria-pressed"="$_tables ? 'true' : 'false'" {
							"Data tables"
						}
					}
					div class="grid gap-x-8 gap-y-8 lg:grid-cols-2" "data-class"="{'lg:grid-cols-2': !$_tables}" {
						(chart("usage-cpu", "CPU time per day", Unit::Rate("CPU-h"), series("CPU-hours", &|b| b.cpu_seconds / 3600.0)))
						(chart("usage-memory", "Memory held per day", Unit::Rate("GB-h"), series("Memory GB-hours", &|b| b.memory_gb_hours)))
						(chart("usage-tokens", "Model tokens per day", Unit::Count, series("Tokens", &|b| b.model_tokens as f64)))
						(chart("usage-egress", "Traffic out per day", Unit::Bytes, series("Bytes out", &|b| b.egress_bytes as f64)))
					}
				}
			}
			div class="ui-usage-actions mt-4" {
				a href={ (base) "/export?grain=hour&days=30" } download class=(button_class(Variant::Outline, Size::Md)) {
					(icon("download")) " Hourly CSV"
				}
				a href={ (base) "/export?grain=day&days=90" } download class=(button_class(Variant::Outline, Size::Md)) {
					(icon("download")) " Daily CSV"
				}
			}
		}))
	})
}

#[derive(Debug, Default, Deserialize)]
struct ExportParams {
	grain: Option<String>,
	days: Option<i64>,
}

async fn export(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(p): Query<ExportParams>,
) -> Response {
	let grain = match p.grain.as_deref().map(str::trim) {
		None | Some("") => Grain::Hour,
		Some(g) => match Grain::parse(g) {
			Some(g) => g,
			None => {
				return error::page(
					&ctx,
					&ServerError::Validation("The grain is hour, day or month.".to_owned()),
				);
			}
		},
	};
	match service::export(&s, ctx.user.id, ctx.team.id, grain, p.days.unwrap_or(30)).await {
		Ok(csv) => {
			let stamp = OffsetDateTime::now_utc().date();
			(
				[
					(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
					(
						header::CONTENT_DISPOSITION,
						format!(
							"attachment; filename=\"usage-{}-{stamp}.csv\"",
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

// --- limits ----------------------------------------------------------------------------------

fn signal(kind: QuotaKind) -> String {
	format!("q_{}", kind.key())
}

fn limits_card(base: &str, rows: &[Row]) -> Markup {
	let initial = rows
		.iter()
		.map(|r| {
			let value = r.limit.map(|l| r.kind.input_value(l)).unwrap_or_default();
			format!("{}: {}", signal(r.kind), js_string(&value))
		})
		.collect::<Vec<_>>()
		.join(", ");
	let signals = format!("{{{initial}, _error: ''}}");
	let names: Vec<String> = QuotaKind::ALL.iter().map(|k| signal(*k)).collect();
	let names: Vec<&str> = names.iter().map(String::as_str).collect();
	card(&html! {
		(card_heading(
			"Team limits",
			Some("Only you, as the instance administrator, see this. Leave a field empty for no limit. The limits apply to this team only."),
		))
		(card_content(&html! {
			form class="flex flex-col gap-6" data-signals=(signals)
				"data-on:submit"={ "evt.preventDefault(); @post(" (js_string(base)) ")" } {
				@for group in [Group::Resources, Group::Capacity, Group::Monthly] {
					div class="ui-usage-group" {
						h3 { (group.label()) }
						div class="ui-quota-grid" {
							@for kind in QuotaKind::ALL.into_iter().filter(|k| k.group() == group) {
								label class=(LABEL) {
									(kind.label())
									@if !kind.input_unit().is_empty() {
										" " span class="ui-quota-unit" { "(" (kind.input_unit()) ")" }
									}
									input class=(INPUT) type="text" inputmode="decimal" autocomplete="off"
										maxlength="18" placeholder="No limit"
										"data-bind"=(signal(kind));
								}
							}
						}
					}
				}
				(save_bar(&names))
			}
		}))
	})
}

/// The value of a signal as text: a string as it is, a number as written, null as empty.
fn text_of(v: Option<&Value>) -> String {
	match v {
		Some(Value::String(s)) => s.clone(),
		Some(Value::Number(n)) => n.to_string(),
		_ => String::new(),
	}
}

async fn set_limits(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: tower_cookies::Cookies,
	ReadSignals(sig): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let mut changes = Vec::with_capacity(QuotaKind::ALL.len());
	for kind in QuotaKind::ALL {
		match kind.parse_input(&text_of(sig.get(&signal(kind)))) {
			Ok(limit) => changes.push((kind, limit)),
			Err(message) => return error::action(&ServerError::Validation(message)),
		}
	}
	match service::set_limits(&s, ctx.user.id, ctx.team.id, &changes).await {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Limits saved.");
			sse::signals(&serde_json::json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&url(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}
