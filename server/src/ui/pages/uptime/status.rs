//! The public status page, `/status/<team slug>`: a plain page, in the reader's light or dark, with the checks a team picked,
//! each by the name it chose, its state and its last 90 days. It is plain HTML and CSS: no script,
//! no cookie, no request to anyone else, and it is cacheable.
//!
//! It never shows an address, a host or a port, and it answers a team that has no status page
//! exactly as it answers a slug nobody has.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{DOCTYPE, Markup, html};
use time::OffsetDateTime;

use crate::error::ServerError;
use crate::state::AppState;
use crate::ui::components::{icon, percent_text, uptime_strip};
use crate::ui::{APP_CSS_HREF, error};
use crate::uptime::State as CheckState;
use crate::uptime::service::{self, PublicCheck, PublicPage, STATUS_DAYS};

/// How long a browser or a cache may keep the page.
const CACHE: &str = "public, max-age=30, stale-while-revalidate=60";

pub fn router() -> Router<AppState> {
	Router::new().route("/status/{slug}", get(page))
}

/// The one line over the list.
fn summary(checks: &[PublicCheck]) -> (&'static str, &'static str, &'static str) {
	if checks.iter().any(|c| c.state == CheckState::Down) {
		("down", "triangle-alert", "Some systems are down")
	} else if checks.is_empty() || checks.iter().any(|c| c.state == CheckState::Unknown) {
		("unknown", "clock", "Checking the systems")
	} else {
		("up", "circle-check", "All systems operational")
	}
}

fn line(check: &PublicCheck, today: time::Date) -> Markup {
	let (word, state) = match check.state {
		CheckState::Up => ("Up", "up"),
		CheckState::Down => ("Down", "down"),
		CheckState::Unknown => ("Checking", "unknown"),
	};
	let tone = match check.state {
		CheckState::Up => crate::ui::components::Tone::Success,
		CheckState::Down => crate::ui::components::Tone::Danger,
		CheckState::Unknown => crate::ui::components::Tone::Neutral,
	};
	let label = match check.percent {
		Some(p) => format!("Last {STATUS_DAYS} days: {} up", percent_text(p)),
		None => format!("Last {STATUS_DAYS} days: no data yet"),
	};
	html! {
		li class="ui-probe-row" data-check data-state=(state) {
			div class="ui-probe-head" {
				h2 class="ui-probe-name" { (check.name) }
				(crate::ui::components::badge(tone, word))
			}
			(uptime_strip(&check.days, STATUS_DAYS, today, &label))
			p class="ui-probe-meta" {
				span { (STATUS_DAYS) " days ago" }
				@if let Some(p) = check.percent { span { (percent_text(p)) " uptime" } }
				span { "Today" }
			}
		}
	}
}

fn document(page: &PublicPage, today: time::Date) -> Markup {
	let (state, glyph, text) = summary(&page.checks);
	html! {
		(DOCTYPE)
		html lang="en" {
			head {
				meta charset="utf-8";
				meta name="viewport" content="width=device-width, initial-scale=1";
				meta name="color-scheme" content="light dark";
				meta name="robots" content="noindex";
				title { (page.title) " · Status" }
				link rel="stylesheet" href=(*APP_CSS_HREF);
			}
			body class="min-h-screen font-sans" {
				main id="main" class="ui-pub" {
					div class="ui-pub-wrap" {
						header class="ui-pub-head" { h1 { (page.title) } }
						p class="ui-pub-summary" data-state=(state) role="status" { (icon(glyph)) (text) }
						@if page.checks.is_empty() {
							p class="ui-lede" { "Nothing is listed on this page yet." }
						} @else {
							ul class="ui-pub-list" {
								@for check in &page.checks { (line(check, today)) }
							}
						}
						p class="ui-pub-foot" { "Reload for the latest. The page is cached for 30 seconds. All times are UTC." }
					}
				}
			}
		}
	}
}

async fn page(State(s): State<AppState>, Path(slug): Path<String>) -> Response {
	let now = OffsetDateTime::now_utc();
	match service::public_page(&s.db, &slug, now).await {
		Ok(Some(page)) => (
			[(header::CACHE_CONTROL, CACHE)],
			document(&page, now.date()),
		)
			.into_response(),
		Ok(None) => error::bare(None, &ServerError::NotFound),
		Err(e) => {
			tracing::warn!("cannot render a status page: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		}
	}
}
