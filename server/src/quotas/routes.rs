use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use super::service::{self, Grain, MAX_EXPORT};
use super::{Level, QuotaKind};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::TeamId;
use crate::state::AppState;

/// Mounted at `/api/v1/teams/{team_id}/usage`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(usage))
		.route("/history", get(history))
		.route("/export", get(export))
		.route("/limits", axum::routing::put(set_limits))
}

#[derive(Serialize)]
struct QuotaResponse {
	/// `applications`, `memory_mb`, ...
	kind: &'static str,
	label: &'static str,
	/// In the kind's own unit: millicores, MB, tokens, minutes or a count.
	used: i64,
	/// `null` when the team has no limit on it.
	limit: Option<i64>,
	/// 0 to 100; 0 without a limit.
	percent: u64,
	/// `ok`, `warning` (80% or more) or `full`.
	level: &'static str,
}

#[derive(Serialize)]
struct UsageResponse {
	/// The month the monthly kinds count, `YYYY-MM`.
	month: String,
	quotas: Vec<QuotaResponse>,
	/// Whether the caller may change the limits.
	can_edit: bool,
}

const fn level_name(level: Level) -> &'static str {
	match level {
		Level::Ok => "ok",
		Level::Warn => "warning",
		Level::Full => "full",
	}
}

/// Usage against the limits of every kind. Needs `read quota`.
async fn usage(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<UsageResponse>> {
	let rows = service::overview(&s.db, user.id, team).await?;
	Ok(Json(UsageResponse {
		month: super::month_of(OffsetDateTime::now_utc()),
		can_edit: service::is_instance_admin(&s.db, user.id).await?,
		quotas: rows
			.iter()
			.map(|r| QuotaResponse {
				kind: r.kind.key(),
				label: r.kind.label(),
				used: r.used,
				limit: r.limit,
				percent: r.percent(),
				level: level_name(r.level()),
			})
			.collect(),
	}))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
	/// `hour` (default), `day` or `month`.
	grain: Option<String>,
	/// How many days back, 1 to 366; 30 by default.
	days: Option<i64>,
}

impl HistoryQuery {
	fn parse(&self) -> AppResult<(Grain, i64)> {
		let grain = match self.grain.as_deref().map(str::trim) {
			None | Some("") => Grain::Hour,
			Some(g) => Grain::parse(g).ok_or_else(|| {
				ServerError::Validation("grain is hour, day or month.".to_owned())
			})?,
		};
		Ok((grain, self.days.unwrap_or(30).clamp(1, 366)))
	}
}

#[derive(Serialize)]
struct BucketResponse {
	/// Unix seconds.
	at: i64,
	cpu_seconds: f64,
	memory_gb_hours: f64,
	disk_gb_hours: f64,
	gpu_hours: f64,
	egress_bytes: i64,
	model_tokens: i64,
	build_minutes: f64,
}

/// Metered usage over time, oldest first. Needs `read quota`.
async fn history(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(q): Query<HistoryQuery>,
) -> AppResult<Json<Vec<BucketResponse>>> {
	let (grain, days) = q.parse()?;
	let now = OffsetDateTime::now_utc();
	let buckets = service::history(
		&s.db,
		user.id,
		team,
		grain,
		now - Duration::days(days),
		now + Duration::HOUR,
		MAX_EXPORT,
	)
	.await?;
	Ok(Json(
		buckets
			.into_iter()
			.map(|b| BucketResponse {
				at: b.at.unix_timestamp(),
				cpu_seconds: b.cpu_seconds,
				memory_gb_hours: b.memory_gb_hours,
				disk_gb_hours: b.disk_gb_hours,
				gpu_hours: b.gpu_hours,
				egress_bytes: b.egress_bytes,
				model_tokens: b.model_tokens,
				build_minutes: b.build_minutes,
			})
			.collect(),
	))
}

/// The same as CSV. Needs `read quota`.
async fn export(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(q): Query<HistoryQuery>,
) -> AppResult<impl IntoResponse> {
	let (grain, days) = q.parse()?;
	let csv = service::export(&s, user.id, team, grain, days).await?;
	Ok((
		[
			(header::CONTENT_TYPE, "text/csv; charset=utf-8"),
			(
				header::CONTENT_DISPOSITION,
				"attachment; filename=\"usage.csv\"",
			),
			(header::CACHE_CONTROL, "no-store"),
		],
		csv,
	))
}

#[derive(Debug, Deserialize)]
struct LimitsBody {
	/// A kind's key and its new limit; `null` removes the limit. Kinds left out stay as they
	/// are.
	limits: BTreeMap<String, Option<i64>>,
}

/// Sets or removes limits. Only the instance's administrator may.
async fn set_limits(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<LimitsBody>,
) -> AppResult<Json<UsageResponse>> {
	let mut changes = Vec::with_capacity(body.limits.len());
	for (key, limit) in body.limits {
		let kind = QuotaKind::parse(&key)
			.ok_or_else(|| ServerError::Validation(format!("{key} is not a quota.")))?;
		if limit.is_some_and(|l| l < 0) {
			return Err(ServerError::Validation(format!(
				"{key} cannot be negative."
			)));
		}
		changes.push((kind, limit));
	}
	service::set_limits(&s, user.id, team, &changes).await?;
	usage(State(s), user, Path(team)).await
}
