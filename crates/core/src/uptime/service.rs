//! Uptime use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database. The public status page reads through [`public_page`],
//! which needs no user and hands out names and states only.
//!
//! A check can point at any host the control plane can reach, so creating one needs the
//! `create uptime` permission (owners); a plain member only reads.

use std::collections::HashSet;

use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{
	CHECK_COLUMNS, Check, DayStat, Kind, LatencyPoint, MAX_POINTS, MIN_INTERVAL_SECS, State,
	day_stats, uptime_percent,
};
use crate::applications::dashed_ip;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UptimeCheckId, UserId};
use crate::metrics::{Range, downsample};
use crate::state::AppState;

const MAX_NAME_LEN: usize = 60;
const MAX_KEYWORD_LEN: usize = 200;
/// Days the status page's strip and percentage reach back.
pub const STATUS_DAYS: i64 = 90;

fn invalid(what: &str) -> ServerError {
	ServerError::Validation(format!("invalid {what}"))
}

fn default_method() -> String {
	"GET".to_owned()
}
fn default_status() -> i32 {
	200
}
fn default_timeout() -> i32 {
	10
}
fn default_interval() -> i32 {
	60
}
fn yes() -> bool {
	true
}

/// What a create or an update sets. Empty strings and zeros mean "not set" for the fields the
/// kind does not use.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckInput {
	pub name: String,
	/// `http` or `tcp`.
	pub kind: String,
	#[serde(default)]
	pub url: String,
	#[serde(default = "default_method")]
	pub method: String,
	/// `0` accepts any answer below 400.
	#[serde(default = "default_status")]
	pub expected_status: i32,
	#[serde(default)]
	pub keyword: String,
	#[serde(default)]
	pub host: String,
	#[serde(default)]
	pub port: i32,
	#[serde(default = "default_timeout")]
	pub timeout_secs: i32,
	#[serde(default = "default_interval")]
	pub interval_secs: i32,
	/// Warn this many days before the certificate expires; `None` for no watch.
	#[serde(default)]
	pub tls_warn_days: Option<i32>,
	/// Accept any certificate, a self-signed one included. Only for an `https` URL.
	#[serde(default)]
	pub tls_skip_verify: bool,
	#[serde(default = "yes")]
	pub enabled: bool,
	#[serde(default)]
	pub status_visible: bool,
	#[serde(default)]
	pub status_name: String,
}

/// An input that passed [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valid {
	pub name: String,
	pub kind: Kind,
	pub url: Option<String>,
	pub method: String,
	pub expected_status: i32,
	pub keyword: Option<String>,
	pub host: Option<String>,
	pub port: Option<i32>,
	pub timeout_secs: i32,
	pub interval_secs: i32,
	pub tls_warn_days: Option<i32>,
	pub tls_skip_verify: bool,
	pub enabled: bool,
	pub status_visible: bool,
	pub status_name: Option<String>,
}

fn clean_name(raw: &str, what: &str) -> AppResult<String> {
	let name = raw.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
	{
		return Err(invalid(what));
	}
	Ok(name.to_owned())
}

/// A host name, or an IP address, a TCP check may connect to. Lower case, no scheme, port or
/// path; one label is fine (`nas`).
fn tcp_host(s: &str) -> bool {
	let ok_chars = s
		.bytes()
		.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.');
	s.parse::<std::net::IpAddr>().is_ok()
		|| (ok_chars
			&& (1..=253).contains(&s.len())
			&& !s.starts_with(['-', '.'])
			&& !s.ends_with(['-', '.']))
}

/// Checks and normalizes an input.
///
/// # Errors
/// `Validation` naming the first field that is wrong.
pub fn validate(input: &CheckInput) -> AppResult<Valid> {
	let name = clean_name(&input.name, "name")?;
	let kind = Kind::parse(input.kind.trim()).ok_or_else(|| invalid("kind"))?;
	if !(MIN_INTERVAL_SECS..=86_400).contains(&input.interval_secs) {
		return Err(ServerError::Validation(format!(
			"the interval is {MIN_INTERVAL_SECS} seconds to a day"
		)));
	}
	if !(1..=60).contains(&input.timeout_secs) || input.timeout_secs > input.interval_secs {
		return Err(invalid("timeout"));
	}
	let status_name = match input.status_name.trim() {
		"" => None,
		raw => Some(clean_name(raw, "display name")?),
	};
	let mut valid = Valid {
		name,
		kind,
		url: None,
		method: "GET".to_owned(),
		expected_status: 200,
		keyword: None,
		host: None,
		port: None,
		timeout_secs: input.timeout_secs,
		interval_secs: input.interval_secs,
		tls_warn_days: None,
		tls_skip_verify: false,
		enabled: input.enabled,
		status_visible: input.status_visible,
		status_name,
	};
	match kind {
		Kind::Http => {
			let raw = input.url.trim();
			let url = url::Url::parse(raw).map_err(|_| invalid("URL"))?;
			if !matches!(url.scheme(), "http" | "https")
				|| url.host_str().is_none()
				|| !url.username().is_empty()
				|| url.password().is_some()
				|| raw.chars().any(char::is_control)
			{
				return Err(ServerError::Validation(
					"the URL must be http or https, without a login in it".to_owned(),
				));
			}
			let method = input.method.trim().to_ascii_uppercase();
			if !matches!(method.as_str(), "GET" | "HEAD") {
				return Err(invalid("method"));
			}
			if input.expected_status != 0 && !(100..=599).contains(&input.expected_status) {
				return Err(invalid("expected status"));
			}
			let keyword = input.keyword.trim();
			if keyword.chars().count() > MAX_KEYWORD_LEN || keyword.chars().any(char::is_control) {
				return Err(invalid("keyword"));
			}
			if !keyword.is_empty() && method == "HEAD" {
				return Err(ServerError::Validation(
					"a keyword needs GET: a HEAD answer has no body".to_owned(),
				));
			}
			if let Some(days) = input.tls_warn_days
				&& !(1..=90).contains(&days)
			{
				return Err(invalid("certificate warning"));
			}
			valid.url = Some(url.to_string());
			valid.method = method;
			valid.expected_status = input.expected_status;
			valid.keyword = (!keyword.is_empty()).then(|| keyword.to_owned());
			valid.tls_warn_days = input.tls_warn_days.filter(|_| url.scheme() == "https");
			// Nothing to verify on a plain-HTTP address, so the setting is not kept there.
			valid.tls_skip_verify = input.tls_skip_verify && url.scheme() == "https";
		}
		Kind::Tcp => {
			let host = input.host.trim().to_ascii_lowercase();
			if !tcp_host(&host) {
				return Err(invalid("host"));
			}
			if !(1..=65_535).contains(&input.port) {
				return Err(invalid("port"));
			}
			valid.host = Some(host);
			valid.port = Some(input.port);
		}
	}
	Ok(valid)
}

fn name_taken(e: sqlx::Error) -> ServerError {
	if is_unique_violation(&e, "uptime_checks_team_id_name_key") {
		ServerError::Conflict("a check with that name already exists".to_owned())
	} else {
		e.into()
	}
}

// --- checks ---------------------------------------------------------------------------------

/// The team's checks, by name. Needs `read uptime`.
pub async fn checks(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Check>> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	Ok(sqlx::query_as::<_, Check>(sqlx::AssertSqlSafe(format!(
		"SELECT {CHECK_COLUMNS} FROM uptime_checks WHERE team_id = $1 ORDER BY name"
	)))
	.bind(team)
	.fetch_all(db)
	.await?)
}

/// A check of this team; another team's id is `UptimeCheckNotFound`, like a missing one.
async fn check_in(db: &PgPool, team: TeamId, id: UptimeCheckId) -> AppResult<Check> {
	sqlx::query_as::<_, Check>(sqlx::AssertSqlSafe(format!(
		"SELECT {CHECK_COLUMNS} FROM uptime_checks WHERE id = $1 AND team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::UptimeCheckNotFound)
}

/// One check. Needs `read uptime`.
pub async fn check(db: &PgPool, user: UserId, team: TeamId, id: UptimeCheckId) -> AppResult<Check> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	check_in(db, team, id).await
}

/// The team's checks with the last `days` days of each. Needs `read uptime`.
pub async fn checks_with_days(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	days: i64,
	now: OffsetDateTime,
) -> AppResult<Vec<(Check, Vec<DayStat>)>> {
	let list = checks(db, user, team).await?;
	let ids: Vec<UptimeCheckId> = list.iter().map(|c| c.id).collect();
	let mut stats = day_stats(db, &ids, days, now).await?;
	Ok(list
		.into_iter()
		.map(|c| {
			let days = stats.remove(&c.id).unwrap_or_default();
			(c, days)
		})
		.collect())
}

/// One check's per-day figures for the last `days` days. Needs `read uptime`.
pub async fn check_days(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: UptimeCheckId,
	days: i64,
	now: OffsetDateTime,
) -> AppResult<Vec<DayStat>> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	check_in(db, team, id).await?;
	Ok(day_stats(db, &[id], days, now)
		.await?
		.remove(&id)
		.unwrap_or_default())
}

/// Adds a check; it runs on the next pass. Needs `create uptime`.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	input: &CheckInput,
) -> AppResult<Check> {
	authorize(&s.db, user, team, Action::Create, Resource::Uptime).await?;
	let v = validate(input)?;
	let check = sqlx::query_as::<_, Check>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO uptime_checks (id, team_id, name, kind, url, method, expected_status, keyword,
			host, port, timeout_secs, interval_secs, tls_warn_days, enabled, status_visible, status_name,
			tls_skip_verify)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
		RETURNING {CHECK_COLUMNS}"
	)))
	.bind(UptimeCheckId::generate())
	.bind(team)
	.bind(&v.name)
	.bind(v.kind.as_str())
	.bind(&v.url)
	.bind(&v.method)
	.bind(v.expected_status)
	.bind(&v.keyword)
	.bind(&v.host)
	.bind(v.port)
	.bind(v.timeout_secs)
	.bind(v.interval_secs)
	.bind(v.tls_warn_days)
	.bind(v.enabled)
	.bind(v.status_visible)
	.bind(&v.status_name)
	.bind(v.tls_skip_verify)
	.fetch_one(&s.db)
	.await
	.map_err(name_taken)?;
	s.jobs.notify.notify_one();
	audit::log(
		&s.db,
		user,
		team,
		"uptime_check.create",
		Target::new("uptime_check", check.id.0).named(&check.name),
		Diff::new()
			.set("kind", v.kind.as_str())
			.set("target", target_label(&check))
			.set("interval_secs", i64::from(v.interval_secs))
			.set("status_visible", v.status_visible)
			.set("tls_skip_verify", v.tls_skip_verify),
	)
	.await;
	Ok(check)
}

/// What a check points at, for the log: a URL without credentials, or `host:port`.
fn target_label(c: &Check) -> String {
	match (&c.url, &c.host) {
		(Some(url), _) => audit::scrub_url(url),
		(None, Some(host)) => format!("{host}:{}", c.port.unwrap_or_default()),
		(None, None) => String::new(),
	}
}

/// Replaces a check's settings. A new target starts over: no verdict, no streaks. Needs
/// `update uptime`.
pub async fn update(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: UptimeCheckId,
	input: &CheckInput,
) -> AppResult<Check> {
	authorize(&s.db, user, team, Action::Update, Resource::Uptime).await?;
	let current = check_in(&s.db, team, id).await?;
	let v = validate(input)?;
	let moved = current.kind() != v.kind
		|| current.url != v.url
		|| current.host != v.host
		|| current.port != v.port;
	let check = sqlx::query_as::<_, Check>(sqlx::AssertSqlSafe(format!(
		"UPDATE uptime_checks SET name = $3, kind = $4, url = $5, method = $6, expected_status = $7,
			keyword = $8, host = $9, port = $10, timeout_secs = $11, interval_secs = $12,
			tls_warn_days = $13, enabled = $14, status_visible = $15, status_name = $16,
			state = CASE WHEN $17 THEN 'unknown' ELSE state END,
			fail_streak = CASE WHEN $17 THEN 0 ELSE fail_streak END,
			ok_streak = CASE WHEN $17 THEN 0 ELSE ok_streak END,
			tls_expires_at = CASE WHEN $17 THEN NULL ELSE tls_expires_at END,
			tls_checked_at = CASE WHEN $17 THEN NULL ELSE tls_checked_at END,
			tls_skip_verify = $18,
			next_check_at = NOW()
		WHERE id = $1 AND team_id = $2 RETURNING {CHECK_COLUMNS}"
	)))
	.bind(id)
	.bind(team)
	.bind(&v.name)
	.bind(v.kind.as_str())
	.bind(&v.url)
	.bind(&v.method)
	.bind(v.expected_status)
	.bind(&v.keyword)
	.bind(&v.host)
	.bind(v.port)
	.bind(v.timeout_secs)
	.bind(v.interval_secs)
	.bind(v.tls_warn_days)
	.bind(v.enabled)
	.bind(v.status_visible)
	.bind(&v.status_name)
	.bind(moved)
	.bind(v.tls_skip_verify)
	.fetch_optional(&s.db)
	.await
	.map_err(name_taken)?
	.ok_or(ServerError::UptimeCheckNotFound)?;
	if moved {
		// An outage of the old target is over with it.
		sqlx::query(
			"UPDATE uptime_incidents SET ended_at = NOW() WHERE check_id = $1 AND ended_at IS NULL",
		)
		.bind(id)
		.execute(&s.db)
		.await?;
	}
	s.jobs.notify.notify_one();
	audit::log(
		&s.db,
		user,
		team,
		"uptime_check.update",
		Target::new("uptime_check", check.id.0).named(&check.name),
		Diff::new()
			.changed("name", current.name.as_str(), check.name.as_str())
			.changed("target", target_label(&current), target_label(&check))
			.changed("enabled", current.enabled, check.enabled)
			.changed(
				"interval_secs",
				i64::from(current.interval_secs),
				i64::from(check.interval_secs),
			)
			.changed(
				"status_visible",
				current.status_visible,
				check.status_visible,
			)
			.changed(
				"tls_skip_verify",
				current.tls_skip_verify,
				check.tls_skip_verify,
			),
	)
	.await;
	Ok(check)
}

/// Asks for a run on the next pass instead of at the interval. Needs `update uptime`.
pub async fn run_now(s: &AppState, user: UserId, team: TeamId, id: UptimeCheckId) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Uptime).await?;
	check_in(&s.db, team, id).await?;
	sqlx::query("UPDATE uptime_checks SET next_check_at = NOW() WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(&s.db)
		.await?;
	s.jobs.notify.notify_one();
	Ok(())
}

/// Deletes a check with its history. One podkit made by itself is remembered, so it does not
/// come back. Needs `delete uptime`.
pub async fn remove(db: &PgPool, user: UserId, team: TeamId, id: UptimeCheckId) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Uptime).await?;
	let current = check_in(db, team, id).await?;
	let mut tx = db.begin().await?;
	if let Some(key) = &current.auto_key {
		sqlx::query(
			"INSERT INTO uptime_dismissed (team_id, auto_key) VALUES ($1, $2) ON CONFLICT DO NOTHING",
		)
		.bind(team)
		.bind(key)
		.execute(&mut *tx)
		.await?;
	}
	sqlx::query("DELETE FROM uptime_checks WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(&mut *tx)
		.await?;
	tx.commit().await?;
	audit::log(
		db,
		user,
		team,
		"uptime_check.delete",
		Target::new("uptime_check", id.0).named(&current.name),
		Diff::new(),
	)
	.await;
	Ok(())
}

// --- history --------------------------------------------------------------------------------

/// A check's response times over `range` ending at `now`, oldest first, at most
/// [`MAX_POINTS`] long. A failed run is a point with no time. Needs `read uptime`.
pub async fn series(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: UptimeCheckId,
	range: Range,
	now: OffsetDateTime,
) -> AppResult<Vec<LatencyPoint>> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	check_in(db, team, id).await?;
	let from = now - range.duration();
	let rows = sqlx::query_as::<_, (OffsetDateTime, Option<i32>)>(
		"SELECT at, latency_ms FROM uptime_results
		WHERE check_id = $1 AND at >= $2 AND at <= $3 ORDER BY at",
	)
	.bind(id)
	.bind(from)
	.bind(now)
	.fetch_all(db)
	.await?;
	let points = rows
		.into_iter()
		.map(|(at, ms)| LatencyPoint {
			at: at.unix_timestamp(),
			ms: ms.map(f64::from),
		})
		.collect();
	Ok(downsample(
		points,
		from.unix_timestamp(),
		now.unix_timestamp(),
		MAX_POINTS,
	))
}

/// A stretch of being down.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Incident {
	pub started_at: OffsetDateTime,
	pub ended_at: Option<OffsetDateTime>,
	pub error: Option<String>,
}

/// The latest incidents of a check, newest first. Needs `read uptime`.
pub async fn incidents(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: UptimeCheckId,
	limit: i64,
) -> AppResult<Vec<Incident>> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	check_in(db, team, id).await?;
	Ok(sqlx::query_as::<_, Incident>(
		"SELECT started_at, ended_at, error FROM uptime_incidents
		WHERE check_id = $1 ORDER BY started_at DESC LIMIT $2",
	)
	.bind(id)
	.bind(limit)
	.fetch_all(db)
	.await?)
}

// --- the status page and the team's settings --------------------------------------------------

/// A team's status page as set.
#[derive(Debug, Clone)]
pub struct StatusPage {
	pub enabled: bool,
	pub title: Option<String>,
	/// The path part after `/status/`.
	pub slug: String,
}

/// Whether the team's status page is on and what it is called. Needs `read uptime`.
pub async fn status_page(db: &PgPool, user: UserId, team: TeamId) -> AppResult<StatusPage> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	let (slug, enabled, title) = sqlx::query_as::<_, (String, Option<bool>, Option<String>)>(
		"SELECT t.slug, sp.enabled, sp.title FROM teams t
		LEFT JOIN uptime_status_pages sp ON sp.team_id = t.id WHERE t.id = $1",
	)
	.bind(team)
	.fetch_one(db)
	.await?;
	Ok(StatusPage {
		enabled: enabled.unwrap_or(false),
		title,
		slug,
	})
}

/// Switches the status page on or off and sets its title. Needs `update uptime`.
pub async fn set_status_page(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	enabled: bool,
	title: &str,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Uptime).await?;
	let title = match title.trim() {
		"" => None,
		raw => Some(clean_name(raw, "title")?),
	};
	sqlx::query(
		"INSERT INTO uptime_status_pages (team_id, enabled, title) VALUES ($1, $2, $3)
		ON CONFLICT (team_id) DO UPDATE SET enabled = EXCLUDED.enabled, title = EXCLUDED.title",
	)
	.bind(team)
	.bind(enabled)
	.bind(&title)
	.execute(db)
	.await?;
	audit::log(
		db,
		user,
		team,
		"uptime_status_page.update",
		Target::none("uptime_status_page"),
		Diff::new().set("enabled", enabled).set("title", title),
	)
	.await;
	Ok(())
}

/// Whether podkit makes checks for application domains. Needs `read uptime`.
pub async fn auto_checks(db: &PgPool, user: UserId, team: TeamId) -> AppResult<bool> {
	authorize(db, user, team, Action::Read, Resource::Uptime).await?;
	Ok(sqlx::query_scalar::<_, bool>(
		"SELECT COALESCE((SELECT auto_checks FROM uptime_settings WHERE team_id = $1), TRUE)",
	)
	.bind(team)
	.fetch_one(db)
	.await?)
}

/// Opts the team in or out of checks made for application domains. Checks that exist stay.
/// Needs `update uptime`.
pub async fn set_auto_checks(db: &PgPool, user: UserId, team: TeamId, on: bool) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Uptime).await?;
	sqlx::query(
		"INSERT INTO uptime_settings (team_id, auto_checks) VALUES ($1, $2)
		ON CONFLICT (team_id) DO UPDATE SET auto_checks = EXCLUDED.auto_checks",
	)
	.bind(team)
	.bind(on)
	.execute(db)
	.await?;
	audit::log(
		db,
		user,
		team,
		"uptime_settings.update",
		Target::none("uptime_settings"),
		Diff::new().set("auto_checks", on),
	)
	.await;
	Ok(())
}

/// One line of the public page: a name, a state and the last days. Nothing about the target.
#[derive(Debug, Clone)]
pub struct PublicCheck {
	pub name: String,
	pub state: State,
	pub days: Vec<DayStat>,
	pub percent: Option<f64>,
}

/// What `/status/<slug>` shows.
#[derive(Debug, Clone)]
pub struct PublicPage {
	pub title: String,
	pub checks: Vec<PublicCheck>,
}

/// The public page of the team with this slug: `None` unless its status page is on, which
/// makes a team with none look exactly like a slug nobody has. Only checks picked for the page
/// and not paused are in it.
///
/// # Errors
/// The database rejects a read.
pub async fn public_page(
	db: &PgPool,
	slug: &str,
	now: OffsetDateTime,
) -> Result<Option<PublicPage>, sqlx::Error> {
	let Some((team, name, title)) = sqlx::query_as::<_, (TeamId, String, Option<String>)>(
		"SELECT t.id, t.name, sp.title FROM teams t
		JOIN uptime_status_pages sp ON sp.team_id = t.id AND sp.enabled
		WHERE t.slug = $1",
	)
	.bind(slug)
	.fetch_optional(db)
	.await?
	else {
		return Ok(None);
	};
	let rows = sqlx::query_as::<_, (UptimeCheckId, String, String)>(
		"SELECT id, COALESCE(NULLIF(status_name, ''), name), state FROM uptime_checks
		WHERE team_id = $1 AND status_visible AND enabled ORDER BY COALESCE(NULLIF(status_name, ''), name)",
	)
	.bind(team)
	.fetch_all(db)
	.await?;
	let ids: Vec<UptimeCheckId> = rows.iter().map(|r| r.0).collect();
	let mut stats = day_stats(db, &ids, STATUS_DAYS, now).await?;
	let checks = rows
		.into_iter()
		.map(|(id, name, state)| {
			let days = stats.remove(&id).unwrap_or_default();
			PublicCheck {
				name,
				state: State::parse(&state),
				percent: uptime_percent(&days),
				days,
			}
		})
		.collect();
	Ok(Some(PublicPage {
		title: title.unwrap_or(name),
		checks,
	}))
}

// --- checks for application domains -----------------------------------------------------------

/// The URL a check of `host` of an application uses: plain HTTP on the ingress port for the
/// generated sslip.io name; for a custom domain HTTPS when podkit has ACME set up.
fn domain_url(s: &AppState, host: &str, custom: bool) -> String {
	let config = &s.config;
	if custom && config.acme_email.is_some() {
		match config.https_port {
			443 => format!("https://{host}/"),
			port => format!("https://{host}:{port}/"),
		}
	} else {
		match config.ingress_port {
			80 => format!("http://{host}/"),
			port => format!("http://{host}:{port}/"),
		}
	}
}

/// Makes a check for every domain of every application that is running, for teams that did
/// not opt out, and drops the checks of domains that no longer exist. A check someone deleted
/// is not made again.
///
/// # Errors
/// The database rejects a read or a write.
pub async fn reconcile_auto(s: &AppState) -> Result<(), sqlx::Error> {
	let db = &s.db;
	// Every application of a team that has not opted out, with whether it runs.
	let apps = sqlx::query_as::<_, (i64, String, String, TeamId, String, bool, bool)>(
		"SELECT a.id, a.name, a.slug, p.team_id, sv.hostname, sv.is_local,
			EXISTS (SELECT 1 FROM deployments d WHERE d.application_id = a.id AND d.status = 'running')
		FROM applications a
		JOIN projects p ON p.id = a.project_id
		JOIN servers sv ON sv.id = a.server_id
		WHERE COALESCE((SELECT auto_checks FROM uptime_settings us WHERE us.team_id = p.team_id), TRUE)",
	)
	.fetch_all(db)
	.await?;
	if apps.is_empty() {
		return Ok(());
	}
	let ids: Vec<i64> = apps.iter().map(|a| a.0).collect();
	let custom = sqlx::query_as::<_, (i64, String)>(
		"SELECT application_id, hostname FROM custom_domains WHERE application_id = ANY($1)",
	)
	.bind(&ids)
	.fetch_all(db)
	.await?;
	let mut wanted: HashSet<String> = HashSet::new();
	for (id, name, slug, team, server_host, is_local, running) in &apps {
		let mut hosts = vec![(
			format!("{slug}.{}.sslip.io", dashed_ip(*is_local, server_host)),
			false,
		)];
		hosts.extend(
			custom
				.iter()
				.filter(|(app, _)| app == id)
				.map(|(_, h)| (h.clone(), true)),
		);
		for (host, is_custom) in hosts {
			let key = format!("app:{id}:{host}");
			wanted.insert(key.clone());
			if !running {
				continue;
			}
			let url = domain_url(s, &host, is_custom);
			let warn = url.starts_with("https://").then_some(14);
			let label =
				crate::notifications::event::clip(&format!("{name} · {host}"), MAX_NAME_LEN);
			sqlx::query(
				"INSERT INTO uptime_checks (id, team_id, application_id, auto_key, name, kind, url,
					expected_status, tls_warn_days)
				SELECT $1, $2, $3, $4, $5, 'http', $6, 0, $7
				WHERE NOT EXISTS (SELECT 1 FROM uptime_dismissed WHERE team_id = $2 AND auto_key = $4)
				ON CONFLICT DO NOTHING",
			)
			.bind(UptimeCheckId::generate())
			.bind(team)
			.bind(id)
			.bind(&key)
			.bind(label)
			.bind(url)
			.bind(warn)
			.execute(db)
			.await?;
		}
	}
	let keys: Vec<String> = wanted.into_iter().collect();
	sqlx::query(
		"DELETE FROM uptime_checks WHERE application_id = ANY($1) AND auto_key IS NOT NULL
		AND NOT (auto_key = ANY($2))",
	)
	.bind(&ids)
	.bind(&keys)
	.execute(db)
	.await?;
	Ok(())
}
