//! The search over stored logs: the form, the chips, the viewer and the stream that fills it.
//!
//! The form is a plain GET, so a search is a link and works without script. The viewer is the
//! shared log viewer; its stream is [`stream`], which answers with the newest lines that match
//! and, when the live tail is on, keeps asking the agents for what is newer than the last line
//! it showed. Every request goes through `log_search::service`, which scopes it to the team.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use datastar::consts::ElementPatchMode;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use url::form_urlencoded;

use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::log_search::levels_for;
use crate::log_search::service::{self, Answer, Source};
use crate::state::AppState;
use crate::ui::components::{BUTTON, CHECKBOX, INPUT, LABEL, SELECT, chip, chip_group, log_viewer};
use crate::ui::extract::TeamCtx;
use crate::ui::live::{Chunk, StoredLine, error_line, stored_log};
use crate::ui::{error, sse};

/// The time ranges the page offers: the word in the address, what the chip says, the seconds.
const RANGES: [(&str, &str, i64); 5] = [
	("1h", "Last hour", 3600),
	("6h", "6 hours", 6 * 3600),
	("24h", "24 hours", 86_400),
	("7d", "7 days", 7 * 86_400),
	("30d", "30 days", 30 * 86_400),
];
const DEFAULT_RANGE: &str = "24h";
/// The levels the page offers: the word in the address and what the chip says.
const LEVELS: [(&str, &str); 4] = [
	("any", "Any"),
	("info", "Info and up"),
	("warn", "Warnings and errors"),
	("error", "Errors"),
];

/// Lines the first answer holds at most. The viewer is a page of text, not an export.
const FIRST_LIMIT: u32 = 1000;
/// Lines one live-tail round holds at most.
const TAIL_LIMIT: u32 = 500;
/// How often the live tail asks again.
const TAIL_EVERY: Duration = Duration::from_secs(2);
/// A live tail stops after this long, so a forgotten tab does not search forever.
const TAIL_FOR: Duration = Duration::from_secs(3600);

/// The id of the viewer's log, which the stream appends into.
pub const LOG_ID: &str = "history-log";

/// What the address carries.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Params {
	/// One source (`app-7`); empty means all of the team's.
	pub r: String,
	pub range: String,
	pub q: String,
	/// `1` when `q` is a regular expression.
	pub regex: Option<String>,
	pub level: String,
	/// `1` for the live tail.
	pub tail: Option<String>,
	/// Only the lines of this deployment.
	pub dep: Option<i64>,
}

impl Params {
	fn range(&self) -> &'static str {
		RANGES
			.iter()
			.find(|(word, ..)| *word == self.range)
			.map_or(DEFAULT_RANGE, |(word, ..)| word)
	}

	fn level(&self) -> &'static str {
		LEVELS
			.iter()
			.find(|(word, _)| *word == self.level)
			.map_or("any", |(word, _)| word)
	}

	fn is_regex(&self) -> bool {
		self.regex.as_deref() == Some("1")
	}

	fn tails(&self) -> bool {
		self.tail.as_deref() == Some("1")
	}

	/// The same search with the source pinned to `fixed`, when the page is one resource's.
	fn pinned(&self, fixed: Option<&str>) -> Self {
		let mut p = self.clone();
		if let Some(fixed) = fixed {
			fixed.clone_into(&mut p.r);
		}
		p
	}

	/// The search these parameters ask for, at `now`.
	fn query(&self, now: OffsetDateTime) -> AppResult<service::Query> {
		let secs = RANGES
			.iter()
			.find(|(word, ..)| *word == self.range())
			.map_or(86_400, |(.., secs)| *secs);
		let now_ms = i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX);
		Ok(service::Query {
			services: if self.r.is_empty() {
				Vec::new()
			} else {
				vec![self.r.clone()]
			},
			since: Some(now_ms - secs * 1000),
			until: None,
			text: (!self.q.is_empty()).then(|| self.q.clone()),
			regex: self.is_regex(),
			levels: levels_for(self.level()).ok_or(ServerError::NotFound)?,
			deployment: self.dep,
			limit: Some(FIRST_LIMIT),
			newest: true,
		})
	}

	/// `a=b&c=d` for the address, leaving out what is the default.
	fn pairs(&self) -> Vec<(&'static str, String)> {
		let mut out = Vec::new();
		if !self.r.is_empty() {
			out.push(("r", self.r.clone()));
		}
		if self.range() != DEFAULT_RANGE {
			out.push(("range", self.range().to_owned()));
		}
		if !self.q.is_empty() {
			out.push(("q", self.q.clone()));
		}
		if self.is_regex() {
			out.push(("regex", "1".to_owned()));
		}
		if self.level() != "any" {
			out.push(("level", self.level().to_owned()));
		}
		if self.tails() {
			out.push(("tail", "1".to_owned()));
		}
		if let Some(dep) = self.dep {
			out.push(("dep", dep.to_string()));
		}
		out
	}

	fn href(&self, base: &str, extra: &[(&str, &str)]) -> String {
		let mut qs = form_urlencoded::Serializer::new(String::new());
		for (k, v) in extra {
			qs.append_pair(k, v);
		}
		for (k, v) in self.pairs() {
			qs.append_pair(k, &v);
		}
		let qs = qs.finish();
		if qs.is_empty() {
			base.to_owned()
		} else if base.contains('?') {
			format!("{base}&{qs}")
		} else {
			format!("{base}?{qs}")
		}
	}
}

/// Where a search form lives and what it searches.
pub struct Panel<'a> {
	pub team: TeamId,
	/// The page the form submits to (and the chips link to).
	pub action: String,
	/// Fields the page needs kept (`mode=history`).
	pub keep: Vec<(&'static str, &'static str)>,
	/// The resource this page is about; `None` offers a picker.
	pub fixed: Option<&'a str>,
	pub sources: &'a [Source],
}

pub fn stream_url(team: TeamId) -> String {
	format!("/teams/{team}/logs/stream")
}

fn select_option(value: &str, label: &str, selected: bool) -> Markup {
	html! { option value=(value) selected[selected] { (label) } }
}

/// The form, the chips and the viewer.
pub fn panel(panel: &Panel<'_>, params: &Params) -> Markup {
	let p = params.pinned(panel.fixed);
	let chips = |word: &'static str, items: Vec<(&'static str, &'static str)>, on: &str| {
		let chips = html! {
			@for (value, label) in &items {
				(chip(label, &p.href_with(&panel.action, &panel.keep, word, value), *value == on, None))
			}
		};
		chip_group(if word == "range" { "Range" } else { "Level" }, &chips)
	};
	let ranges = RANGES.iter().map(|(w, l, _)| (*w, *l)).collect();
	let stream = p.href(&stream_url(panel.team), &[]);
	html! {
		div class="flex flex-col gap-4" {
			search {
				form class="ui-logs-form" method="get" action=(panel.action) aria-label="Search the stored logs" {
					@for (k, v) in &panel.keep { input type="hidden" name=(k) value=(v); }
					input type="hidden" name="range" value=(p.range());
					input type="hidden" name="level" value=(p.level());
					@if let Some(dep) = p.dep { input type="hidden" name="dep" value=(dep); }
					@if panel.fixed.is_none() {
						label class=(LABEL) {
							"Resource"
							select class=(SELECT) name="r" {
								button { selectedcontent {} }
								(select_option("", "All resources", p.r.is_empty()))
								@for source in panel.sources {
									(select_option(&source.service, &format!("{} ({})", source.title(), source.kind.label()), source.service == p.r))
								}
							}
						}
					}
					label class=(LABEL) {
						"Text"
						input class=(INPUT) type="search" name="q" value=(p.q) maxlength="500" autocomplete="off"
							autocapitalize="none" autocorrect="off" spellcheck="false" placeholder="error, a request id, a pattern";
					}
					div class="ui-logs-checks" {
						div class="ui-check" {
							input id="logs-regex" type="checkbox" class=(CHECKBOX) name="regex" value="1" checked[p.is_regex()];
							label for="logs-regex" class="cursor-pointer" { "Regular expression" }
						}
						div class="ui-check" {
							input id="logs-tail" type="checkbox" class=(CHECKBOX) name="tail" value="1" checked[p.tails()];
							label for="logs-tail" class="cursor-pointer" { "Live tail" }
						}
					}
					div class="ui-logs-act" {
						button type="submit" class=(BUTTON) { "Search" }
					}
				}
			}
			div class="ui-logs-chips" {
				(chips("range", ranges, p.range()))
				(chips("level", LEVELS.to_vec(), p.level()))
			}
			(log_viewer(LOG_ID, "Stored logs", &stream))
		}
	}
}

impl Params {
	/// The address of this search with one field changed.
	fn href_with(
		&self,
		base: &str,
		keep: &[(&'static str, &'static str)],
		key: &str,
		value: &str,
	) -> String {
		let mut next = self.clone();
		match key {
			"range" => value.clone_into(&mut next.range),
			_ => value.clone_into(&mut next.level),
		}
		next.href(
			base,
			&keep.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>(),
		)
	}
}

// --- the stream ------------------------------------------------------------------

fn failure(e: &ServerError) -> Response {
	let status = e.status();
	if status == axum::http::StatusCode::NOT_FOUND || status == axum::http::StatusCode::FORBIDDEN {
		return status.into_response();
	}
	sse::patch_into(
		&format!("#{LOG_ID}"),
		ElementPatchMode::Append,
		&error_line(&error::message(e)),
	)
	.and(sse::signals(&json!({ "_live": false })))
	.into_response()
}

/// A line already shown: its time, resource and text.
type Shown = (i64, String, String);

fn lines_of(answer: &Answer, only_after: Option<(i64, &HashSet<Shown>)>) -> Vec<StoredLine> {
	answer
		.merged
		.lines
		.iter()
		.filter(|l| match only_after {
			None => true,
			Some((last, seen)) => {
				l.record.ts > last
					|| (l.record.ts == last
						&& !seen.contains(&(
							l.record.ts,
							l.record.svc.clone(),
							l.record.line.clone(),
						)))
			}
		})
		.map(|l| StoredLine {
			ts: l.record.ts,
			stream: l.record.stream,
			level: l.record.level,
			source: answer.label(&l.record.svc),
			line: l.record.line.clone(),
		})
		.collect()
}

/// What to tell the reader about the answer beyond its lines.
fn notes(answer: &Answer, shown: usize) -> Vec<Chunk> {
	let mut out = Vec::new();
	if !answer.unreachable.is_empty() {
		out.push(Chunk::Note(format!(
			"{} did not answer, so its lines are missing.",
			answer.unreachable.join(", ")
		)));
	}
	if answer.merged.partial {
		out.push(Chunk::Note(
			"The search stopped early to stay quick, so some lines may be missing. Narrow the time range or add a filter.".to_owned(),
		));
	}
	if answer.merged.truncated {
		out.push(Chunk::Note(format!(
			"More lines match than fit: these are the newest {shown}. Narrow the time range or add a filter to reach older ones."
		)));
	}
	if shown == 0 && answer.unreachable.is_empty() {
		out.push(Chunk::Note(
			"No stored lines match. Logs are kept from when podkit first saw the container."
				.to_owned(),
		));
	}
	out
}

struct Tail {
	s: AppState,
	user: UserId,
	team: TeamId,
	query: service::Query,
	last: i64,
	seen: HashSet<(i64, String, String)>,
	since: Instant,
}

impl Tail {
	/// Remembers the lines at the newest time, so the next round can tell which are new.
	fn note(&mut self, answer: &Answer) {
		for l in &answer.merged.lines {
			if l.record.ts > self.last {
				self.last = l.record.ts;
				self.seen.clear();
			}
			if l.record.ts == self.last {
				self.seen
					.insert((l.record.ts, l.record.svc.clone(), l.record.line.clone()));
			}
		}
	}
}

fn tail_chunks(tail: Tail) -> BoxStream<'static, Chunk> {
	stream::unfold(Some(tail), |tail| async move {
		let mut tail = tail?;
		loop {
			tokio::time::sleep(TAIL_EVERY).await;
			if tail.since.elapsed() > TAIL_FOR {
				return Some((
					Chunk::Note(
						"The live tail stopped after an hour. Search again to carry on.".to_owned(),
					),
					None,
				));
			}
			let mut query = tail.query.clone();
			query.since = Some(tail.last);
			query.until = None;
			query.newest = false;
			query.limit = Some(TAIL_LIMIT);
			match service::search(&tail.s, tail.user, tail.team, &query).await {
				Ok(answer) => {
					let fresh = lines_of(&answer, Some((tail.last, &tail.seen)));
					tail.note(&answer);
					if !fresh.is_empty() {
						return Some((Chunk::Lines(fresh), Some(tail)));
					}
				}
				Err(e) => return Some((Chunk::Note(error::message(&e)), None)),
			}
		}
	})
	.boxed()
}

/// `GET /teams/{team}/logs/stream`: the newest matching lines, then (with the tail on) what
/// comes after them.
pub async fn stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(params): Query<Params>,
) -> Response {
	let now = OffsetDateTime::now_utc();
	let query = match params.query(now) {
		Ok(q) => q,
		Err(e) => return failure(&e),
	};
	let answer = match service::search(&s, ctx.user.id, ctx.team.id, &query).await {
		Ok(a) => a,
		Err(e) => return failure(&e),
	};
	let lines = lines_of(&answer, None);
	let mut head = notes(&answer, lines.len());
	let mut tail = Tail {
		s: s.clone(),
		user: ctx.user.id,
		team: ctx.team.id,
		query,
		// With nothing shown yet, whatever turns up from a moment ago is new.
		last: i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX) - 5000,
		seen: HashSet::new(),
		since: Instant::now(),
	};
	if !lines.is_empty() {
		tail.last = 0;
		tail.note(&answer);
		head.insert(0, Chunk::Lines(lines));
	}
	let chunks = if params.tails() {
		stream::iter(head).chain(tail_chunks(tail)).boxed()
	} else {
		stream::iter(head).boxed()
	};
	stored_log(chunks, LOG_ID, s.shutdown.clone())
}
