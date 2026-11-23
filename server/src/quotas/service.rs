//! Quota use cases shared by the JSON API, the web UI and the places that create or deploy
//! something: reading usage against limits, changing limits, and the checks.
//!
//! Reading needs `read quota` (every member). Changing a limit needs to be the instance's
//! administrator, the oldest account: it is the one thing in a team that its own owners may not
//! decide, because a limit exists to protect the server from the team. Someone outside the team
//! is told it does not exist, as everywhere else.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, OffsetDateTime, Time};

use super::{Level, QuotaKind, WHERE_TO_RAISE, month_of};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{self, Action, Resource, authorize};
use crate::billing::Op;
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::notifications::{self, Event, EventKind};
use crate::state::AppState;

/// Rows one CSV export holds.
pub const MAX_EXPORT: i64 = 20_000;

/// Usage records are kept this long.
const RECORD_RETENTION_DAYS: i32 = 400;

/// A team's hard limits. A kind that is not in it is not limited.
pub type Limits = BTreeMap<QuotaKind, i64>;

/// An increase a create asks for: `add` more of `kind`. A deploy asks for none (`add` 0): it is
/// refused when a limit is already passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
	/// What grows.
	pub kind: QuotaKind,
	/// By how much, in the kind's stored unit.
	pub add: i64,
}

impl Need {
	/// One more of a counted kind.
	#[must_use]
	pub const fn one(kind: QuotaKind) -> Self {
		Self { kind, add: 1 }
	}

	/// `add` more of `kind`.
	#[must_use]
	pub const fn of(kind: QuotaKind, add: i64) -> Self {
		Self { kind, add }
	}
}

/// What creating one resource asks for: one more of `count`, the memory and CPU limits it sets
/// (a resource with none reserves nothing) and a GPU when it takes one.
#[must_use]
pub fn reserved(
	count: QuotaKind,
	memory_mb: Option<i32>,
	cpu_cores: Option<f64>,
	gpu: bool,
) -> Vec<Need> {
	let mut needs = vec![Need::one(count)];
	if let Some(mb) = memory_mb.filter(|m| *m > 0) {
		needs.push(Need::of(QuotaKind::MemoryMb, i64::from(mb)));
	}
	if let Some(cores) = cpu_cores.filter(|c| c.is_finite() && *c > 0.0) {
		#[allow(
			clippy::cast_possible_truncation,
			reason = "a core count, rounded to thousandths"
		)]
		needs.push(Need::of(
			QuotaKind::CpuMillis,
			(cores * 1000.0).round() as i64,
		));
	}
	if gpu {
		needs.push(Need::one(QuotaKind::Gpus));
	}
	needs
}

// --- reading ---------------------------------------------------------------------------------

/// The limits of `team`.
///
/// # Errors
/// Database failures only.
pub async fn limits(db: &PgPool, team: TeamId) -> Result<Limits, sqlx::Error> {
	let rows: Vec<(String, i64)> =
		sqlx::query_as("SELECT kind, hard_limit FROM team_quotas WHERE team_id = $1")
			.bind(team.0)
			.fetch_all(db)
			.await?;
	Ok(rows
		.into_iter()
		.filter_map(|(kind, limit)| QuotaKind::parse(&kind).map(|k| (k, limit)))
		.collect())
}

fn month_start(now: OffsetDateTime) -> OffsetDateTime {
	Date::from_calendar_date(now.year(), now.month(), 1)
		.unwrap_or(now.date())
		.with_time(Time::MIDNIGHT)
		.assume_utc()
}

/// How much of `kind` the team uses now. See the module docs of [`crate::quotas`] for what
/// each kind counts.
///
/// # Errors
/// Database failures only.
pub async fn used(
	db: &PgPool,
	team: TeamId,
	kind: QuotaKind,
	now: OffsetDateTime,
) -> Result<i64, sqlx::Error> {
	let team = team.0;
	let sql = match kind {
		QuotaKind::Applications => {
			"SELECT COUNT(*) FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1"
		}
		QuotaKind::Databases => {
			"SELECT COUNT(*) FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1"
		}
		QuotaKind::Models => {
			"SELECT COUNT(*) FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1"
		}
		QuotaKind::Stacks => {
			"SELECT COUNT(*) FROM stacks s JOIN projects p ON p.id = s.project_id WHERE p.team_id = $1"
		}
		QuotaKind::Previews => {
			"SELECT COUNT(*) FROM environment_clones WHERE team_id = $1 AND environment_id IS NOT NULL"
		}
		QuotaKind::Gpus => {
			"SELECT COUNT(*) FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1 AND m.gpu"
		}
		QuotaKind::CpuMillis => {
			"SELECT COALESCE(SUM(ROUND(x.cpu * 1000.0)), 0)::BIGINT FROM (
				SELECT a.cpu_limit::float8 AS cpu FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1
				UNION ALL
				SELECT d.cpu_limit::float8 FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1
			) x"
		}
		QuotaKind::MemoryMb => {
			"SELECT COALESCE(SUM(x.mb), 0)::BIGINT FROM (
				SELECT a.memory_limit_mb::bigint AS mb FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1
				UNION ALL
				SELECT d.memory_limit_mb::bigint FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1
				UNION ALL
				SELECT m.memory_limit_mb::bigint FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1
			) x"
		}
		QuotaKind::DiskMb => {
			// The latest volume size of each resource, if it was measured in the last day.
			"WITH res AS (
				SELECT 'application'::text AS kind, a.id AS rid FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1
				UNION ALL SELECT 'database', d.id FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1
				UNION ALL SELECT 'model', m.id FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1
				UNION ALL SELECT 'stack', s.id FROM stacks s JOIN projects p ON p.id = s.project_id WHERE p.team_id = $1
			)
			SELECT COALESCE(CEIL(SUM(latest.volume_bytes) / 1048576.0), 0)::BIGINT FROM (
				SELECT DISTINCT ON (e.kind, e.resource_id) e.volume_bytes
				FROM resource_extra_metrics e JOIN res r ON r.kind = e.kind AND r.rid = e.resource_id
				WHERE e.volume_bytes IS NOT NULL AND e.at > NOW() - INTERVAL '1 day'
				ORDER BY e.kind, e.resource_id, e.at DESC
			) latest"
		}
		QuotaKind::ModelTokens => {
			"SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::BIGINT FROM model_usage WHERE team_id = $1 AND hour >= $2"
		}
		QuotaKind::BuildMinutes => {
			"SELECT COALESCE(CEIL(SUM(EXTRACT(EPOCH FROM (d.finished_at - d.started_at))::float8 / 60.0)), 0)::BIGINT
			FROM deployments d
			JOIN applications a ON a.id = d.application_id
			JOIN projects p ON p.id = a.project_id
			WHERE p.team_id = $1 AND a.source_kind = 'git' AND d.finished_at IS NOT NULL AND d.started_at >= $2"
		}
	};
	let query = sqlx::query_scalar::<_, i64>(sql).bind(team);
	if kind.monthly() {
		query.bind(month_start(now)).fetch_one(db).await
	} else {
		query.fetch_one(db).await
	}
}

/// One line of the usage page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
	/// What is limited.
	pub kind: QuotaKind,
	/// How much is used.
	pub used: i64,
	/// The hard limit, if the team has one.
	pub limit: Option<i64>,
}

impl Row {
	/// The soft and hard limit state; `Ok` with no limit.
	#[must_use]
	pub fn level(&self) -> Level {
		self.limit.map_or(Level::Ok, |l| Level::of(self.used, l))
	}

	/// How full the limit is, 0 to 100; 0 with no limit.
	#[must_use]
	pub fn percent(&self) -> u64 {
		match self.limit {
			Some(l) if l > 0 => {
				u64::try_from((i128::from(self.used.max(0)) * 100 / i128::from(l)).min(100))
					.unwrap_or(100)
			}
			Some(_) => 100,
			None => 0,
		}
	}
}

/// Whether the caller is a member of `team`; someone who is not is told it does not exist.
async fn member(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	if permission::granted(db, user, team).await?.is_member() {
		Ok(())
	} else {
		Err(ServerError::NotFound)
	}
}

async fn allow_read(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	member(db, user, team).await?;
	authorize(db, user, team, Action::Read, Resource::Quota).await
}

/// Whether `user` is the instance's administrator: the oldest account, which `first-user`
/// registration makes the person who set podkit up.
///
/// # Errors
/// Database failures only.
pub async fn is_instance_admin(db: &PgPool, user: UserId) -> Result<bool, sqlx::Error> {
	let first: Option<i64> =
		sqlx::query_scalar("SELECT id FROM users ORDER BY created_at, id LIMIT 1")
			.fetch_optional(db)
			.await?;
	Ok(first == Some(user.0))
}

/// Every kind with what the team uses and its limit; needs `read quota`.
///
/// # Errors
/// `NotFound` outside the team, `Forbidden` without the permission.
pub async fn overview(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Row>> {
	allow_read(db, user, team).await?;
	let limits = limits(db, team).await?;
	let now = OffsetDateTime::now_utc();
	let mut rows = Vec::with_capacity(QuotaKind::ALL.len());
	for kind in QuotaKind::ALL {
		rows.push(Row {
			kind,
			used: used(db, team, kind, now).await?,
			limit: limits.get(&kind).copied(),
		});
	}
	Ok(rows)
}

// --- changing limits ---------------------------------------------------------------------------

/// Sets (`Some`) or removes (`None`) the limits in `changes`, for the instance administrator.
///
/// # Errors
/// `NotFound` outside the team, `Forbidden` for anyone but the instance administrator.
pub async fn set_limits(
	s: &AppState,
	user: UserId,
	team: TeamId,
	changes: &[(QuotaKind, Option<i64>)],
) -> AppResult<()> {
	member(&s.db, user, team).await?;
	if !is_instance_admin(&s.db, user).await? {
		return Err(ServerError::Forbidden);
	}
	let diff = write_limits(&s.db, team, changes).await?;
	audit::log(
		&s.db,
		user,
		team,
		"quota.update",
		Target::none("quota"),
		diff,
	)
	.await;
	evaluate_team(s, team).await;
	Ok(())
}

/// Writes `changes` and returns what changed, for the audit log.
async fn write_limits(
	db: &PgPool,
	team: TeamId,
	changes: &[(QuotaKind, Option<i64>)],
) -> AppResult<Diff> {
	let mut tx = db.begin().await?;
	let mut diff = Diff::new();
	for &(kind, new) in changes {
		let old: Option<i64> = sqlx::query_scalar(
			"SELECT hard_limit FROM team_quotas WHERE team_id = $1 AND kind = $2 FOR UPDATE",
		)
		.bind(team.0)
		.bind(kind.key())
		.fetch_optional(&mut *tx)
		.await?;
		if let Some(limit) = new {
			sqlx::query(
				"INSERT INTO team_quotas (team_id, kind, hard_limit) VALUES ($1, $2, $3)
				ON CONFLICT (team_id, kind) DO UPDATE SET hard_limit = $3, updated_at = NOW()",
			)
			.bind(team.0)
			.bind(kind.key())
			.bind(limit)
			.execute(&mut *tx)
			.await?;
		} else {
			sqlx::query("DELETE FROM team_quotas WHERE team_id = $1 AND kind = $2")
				.bind(team.0)
				.bind(kind.key())
				.execute(&mut *tx)
				.await?;
			// Without a limit there is no level to remember.
			sqlx::query("DELETE FROM quota_levels WHERE team_id = $1 AND kind = $2")
				.bind(team.0)
				.bind(kind.key())
				.execute(&mut *tx)
				.await?;
		}
		diff = diff.changed(kind.key(), old, new);
	}
	tx.commit().await?;
	Ok(diff)
}

/// Applies limits a billing provider's webhook asked for (a plan changed). Recorded as podkit
/// itself acting.
///
/// # Errors
/// Database failures only.
#[cfg(feature = "cloud")]
pub async fn apply_from_provider(
	s: &AppState,
	team: TeamId,
	changes: &[(QuotaKind, Option<i64>)],
) -> AppResult<()> {
	let diff = write_limits(&s.db, team, changes).await?;
	audit::record(
		&s.db,
		&audit::system(team),
		"quota.update",
		Target::none("quota"),
		diff.set("source", "billing"),
	)
	.await;
	evaluate_team(s, team).await;
	Ok(())
}

// --- the checks --------------------------------------------------------------------------------

fn phrase(kind: QuotaKind, value: i64) -> String {
	match kind {
		QuotaKind::CpuMillis | QuotaKind::MemoryMb | QuotaKind::DiskMb => kind.show(value),
		_ => format!("{value} {}", kind.noun()),
	}
}

/// The sentence a refusal carries: which limit, how much is used, what the request needs and
/// where to raise it.
fn refusal(kind: QuotaKind, used: i64, limit: i64, add: i64) -> String {
	let need = if add > 0 && used < limit {
		format!(", and this needs {} more", phrase(kind, add))
	} else {
		String::new()
	};
	let tail = if kind.monthly() {
		" The count starts again on the 1st."
	} else {
		""
	};
	format!(
		"{} limit reached: {} in use, {} allowed{need}.{tail} {WHERE_TO_RAISE}",
		kind.label(),
		phrase(kind, used),
		phrase(kind, limit),
	)
}

/// Whether `need` is refused given what is `used` and the `limit`: a counted or reserved kind
/// when the total would pass the limit, a monthly kind once none is left.
fn refused(need: Need, used: i64, limit: i64) -> bool {
	if need.kind.monthly() {
		used >= limit
	} else {
		used.saturating_add(need.add) > limit
	}
}

/// Checks `needs` for something about to be created in `team`, then asks the billing provider.
/// A passed 80% sends one warning; a refusal sends one "limit reached" and returns the sentence
/// for the person who asked.
///
/// # Errors
/// [`ServerError::QuotaExceeded`] naming the limit.
pub async fn check_create(
	s: &AppState,
	team: TeamId,
	resource: &str,
	needs: &[Need],
) -> AppResult<()> {
	check(s, team, Op::Create, resource, needs).await
}

/// Checks the limits a deploy must respect: reserved CPU, memory and GPUs, disk, and the
/// month's build minutes. Nothing is added, so a team that is over (a limit was lowered) cannot
/// deploy until it is back under.
///
/// # Errors
/// [`ServerError::QuotaExceeded`] naming the limit.
pub async fn check_deploy(s: &AppState, team: TeamId, resource: &str) -> AppResult<()> {
	const DEPLOY: [Need; 5] = [
		Need::of(QuotaKind::CpuMillis, 0),
		Need::of(QuotaKind::MemoryMb, 0),
		Need::of(QuotaKind::DiskMb, 0),
		Need::of(QuotaKind::Gpus, 0),
		Need::of(QuotaKind::BuildMinutes, 0),
	];
	check(s, team, Op::Deploy, resource, &DEPLOY).await
}

/// [`check_deploy`] for something in a project, whose team is looked up.
///
/// # Errors
/// As [`check_deploy`].
pub async fn check_deploy_in_project(
	s: &AppState,
	project: crate::ids::ProjectId,
	resource: &str,
) -> AppResult<()> {
	let team: Option<i64> = sqlx::query_scalar("SELECT team_id FROM projects WHERE id = $1")
		.bind(project.0)
		.fetch_optional(&s.db)
		.await?;
	match team {
		Some(team) => check_deploy(s, TeamId(team), resource).await,
		None => Ok(()),
	}
}

async fn check(
	s: &AppState,
	team: TeamId,
	op: Op,
	resource: &str,
	needs: &[Need],
) -> AppResult<()> {
	let limits = limits(&s.db, team).await?;
	let now = OffsetDateTime::now_utc();
	for &need in needs {
		let Some(&limit) = limits.get(&need.kind) else {
			continue;
		};
		let used = used(&s.db, team, need.kind, now).await?;
		if refused(need, used, limit) {
			announce(s, team, need.kind, Level::Full, used, limit, now).await;
			return Err(ServerError::QuotaExceeded(refusal(
				need.kind, used, limit, need.add,
			)));
		}
		let after = if need.kind.monthly() {
			used
		} else {
			used.saturating_add(need.add)
		};
		let level = Level::of(after, limit);
		if level > Level::Ok {
			announce(s, team, need.kind, level, after, limit, now).await;
		}
	}
	crate::billing::entitlement(s, team, op, resource).await
}

// --- announcing --------------------------------------------------------------------------------

/// Remembers that `kind` is at `level` and, when that is higher than last time, tells the team.
/// A level that goes down is remembered too, so crossing again later tells again.
async fn announce(
	s: &AppState,
	team: TeamId,
	kind: QuotaKind,
	level: Level,
	used: i64,
	limit: i64,
	now: OffsetDateTime,
) {
	let period = if kind.monthly() {
		month_of(now)
	} else {
		String::new()
	};
	let previous = async {
		let row: Option<(String, i16)> = sqlx::query_as(
			"SELECT period, level FROM quota_levels WHERE team_id = $1 AND kind = $2",
		)
		.bind(team.0)
		.bind(kind.key())
		.fetch_optional(&s.db)
		.await?;
		Ok::<_, sqlx::Error>(match row {
			Some((p, l)) if p == period => Level::from_rank(l),
			_ => Level::Ok,
		})
	}
	.await;
	let previous = match previous {
		Ok(p) => p,
		Err(e) => {
			tracing::warn!(%team, kind = kind.key(), "quota level could not be read: {e}");
			return;
		}
	};
	if level == previous {
		return;
	}
	let saved = sqlx::query(
		"INSERT INTO quota_levels (team_id, kind, period, level) VALUES ($1, $2, $3, $4)
		ON CONFLICT (team_id, kind) DO UPDATE SET period = $3, level = $4",
	)
	.bind(team.0)
	.bind(kind.key())
	.bind(&period)
	.bind(level.rank())
	.execute(&s.db)
	.await;
	if let Err(e) = saved {
		tracing::warn!(%team, kind = kind.key(), "quota level could not be saved: {e}");
		return;
	}
	if level <= previous {
		return;
	}
	let link = format!("/teams/{team}/settings/usage");
	let event = if level == Level::Full {
		Event::new(
			team,
			EventKind::QuotaExceeded,
			format!("{team}:{}", kind.key()),
			format!("{} limit reached", kind.label()),
		)
		.body(refusal(kind, used, limit, 0))
	} else {
		Event::new(
			team,
			EventKind::QuotaWarning,
			format!("{team}:{}", kind.key()),
			format!(
				"{} at {}% of the limit",
				kind.label(),
				percent_of(used, limit)
			),
		)
		.body(format!(
			"{} in use of {} allowed. {WHERE_TO_RAISE}",
			phrase(kind, used),
			phrase(kind, limit)
		))
	};
	notifications::emit(s, event.link(link)).await;
}

fn percent_of(used: i64, limit: i64) -> i64 {
	if limit <= 0 {
		return 100;
	}
	i64::try_from(i128::from(used) * 100 / i128::from(limit)).unwrap_or(i64::MAX)
}

/// Recomputes every limit of `team` and announces what changed: warnings and "limit reached"
/// for a level that went up, silence for one that went down.
pub async fn evaluate_team(s: &AppState, team: TeamId) {
	let limits = match limits(&s.db, team).await {
		Ok(l) => l,
		Err(e) => {
			tracing::warn!(%team, "quotas could not be read: {e}");
			return;
		}
	};
	let now = OffsetDateTime::now_utc();
	for (kind, limit) in limits {
		match used(&s.db, team, kind, now).await {
			Ok(used) => announce(s, team, kind, Level::of(used, limit), used, limit, now).await,
			Err(e) => tracing::warn!(%team, kind = kind.key(), "usage could not be read: {e}"),
		}
	}
}

/// Hourly: every team that has a limit.
pub async fn evaluate_all(s: &AppState) {
	let teams: Result<Vec<i64>, _> = sqlx::query_scalar("SELECT DISTINCT team_id FROM team_quotas")
		.fetch_all(&s.db)
		.await;
	match teams {
		Ok(teams) => {
			for team in teams {
				evaluate_team(s, TeamId(team)).await;
			}
		}
		Err(e) => tracing::warn!("quotas could not be evaluated: {e}"),
	}
}

/// The gateway keys of `server` whose team has used up its monthly model tokens. The routing
/// table leaves these keys without models, so the gateway refuses what they ask for.
///
/// # Errors
/// Database failures only.
pub async fn keys_out_of_tokens(
	db: &PgPool,
	server: crate::ids::ServerId,
	now: OffsetDateTime,
) -> Result<Vec<String>, sqlx::Error> {
	sqlx::query_scalar(
		"WITH over AS (
			SELECT q.team_id FROM team_quotas q
			WHERE q.kind = 'model_tokens' AND q.hard_limit <= (
				SELECT COALESCE(SUM(mu.input_tokens + mu.output_tokens), 0)
				FROM model_usage mu WHERE mu.team_id = q.team_id AND mu.hour >= $2
			)
		)
		SELECT k.key_hash FROM gateway_keys k
			JOIN applications a ON a.id = k.application_id
			JOIN projects p ON p.id = a.project_id
			WHERE a.server_id = $1 AND p.team_id IN (SELECT team_id FROM over)
		UNION
		SELECT k.key_hash FROM playground_keys k
			JOIN models m ON m.id = k.model_id
			JOIN projects p ON p.id = m.project_id
			WHERE m.server_id = $1 AND p.team_id IN (SELECT team_id FROM over)",
	)
	.bind(server.0)
	.bind(month_start(now))
	.fetch_all(db)
	.await
}

// --- history and export --------------------------------------------------------------------

/// How usage records are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
	/// As metered.
	Hour,
	/// Per day, UTC.
	Day,
	/// Per calendar month, UTC.
	Month,
}

impl Grain {
	/// `hour`, `day` or `month`.
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"hour" => Some(Self::Hour),
			"day" => Some(Self::Day),
			"month" => Some(Self::Month),
			_ => None,
		}
	}

	const fn sql(self) -> &'static str {
		match self {
			Self::Hour => "hour",
			Self::Day => "day",
			Self::Month => "month",
		}
	}
}

/// Usage summed over one hour, day or month.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Bucket {
	/// Start of the bucket.
	pub at: OffsetDateTime,
	/// CPU time.
	pub cpu_seconds: f64,
	/// Memory held, in GB-hours.
	pub memory_gb_hours: f64,
	/// Volume data held, in GB-hours.
	pub disk_gb_hours: f64,
	/// GPU time.
	pub gpu_hours: f64,
	/// Bytes sent out through the proxy.
	pub egress_bytes: i64,
	/// Tokens the gateway served.
	pub model_tokens: i64,
	/// Minutes of builds.
	pub build_minutes: f64,
}

/// The team's usage between `since` and `until`, oldest first; needs `read quota`.
///
/// # Errors
/// `NotFound` outside the team, `Forbidden` without the permission.
pub async fn history(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	grain: Grain,
	since: OffsetDateTime,
	until: OffsetDateTime,
	limit: i64,
) -> AppResult<Vec<Bucket>> {
	allow_read(db, user, team).await?;
	Ok(sqlx::query_as::<_, Bucket>(
		"SELECT date_trunc($5, hour, 'UTC') AS at,
			COALESCE(SUM(cpu_seconds), 0)::float8 AS cpu_seconds,
			COALESCE(SUM(memory_gb_hours), 0)::float8 AS memory_gb_hours,
			COALESCE(SUM(disk_gb_hours), 0)::float8 AS disk_gb_hours,
			COALESCE(SUM(gpu_hours), 0)::float8 AS gpu_hours,
			COALESCE(SUM(egress_bytes), 0)::BIGINT AS egress_bytes,
			COALESCE(SUM(model_tokens), 0)::BIGINT AS model_tokens,
			COALESCE(SUM(build_minutes), 0)::float8 AS build_minutes
		FROM usage_records
		WHERE team_id = $1 AND hour >= $2 AND hour < $3
		GROUP BY 1 ORDER BY 1 LIMIT $4",
	)
	.bind(team.0)
	.bind(since)
	.bind(until)
	.bind(limit.clamp(1, MAX_EXPORT))
	.bind(grain.sql())
	.fetch_all(db)
	.await?)
}

const CSV_HEADER: &str = "time,cpu_seconds,memory_gb_hours,disk_gb_hours,gpu_hours,egress_bytes,model_tokens,build_minutes\r\n";

/// `buckets` as CSV: numbers and times only, so nothing needs quoting.
#[must_use]
pub fn to_csv(buckets: &[Bucket]) -> String {
	let mut out = String::from(CSV_HEADER);
	for b in buckets {
		let time = b.at.format(&Rfc3339).unwrap_or_default();
		let _ = write!(
			out,
			"{time},{:.1},{:.4},{:.4},{:.4},{},{},{:.2}\r\n",
			b.cpu_seconds,
			b.memory_gb_hours,
			b.disk_gb_hours,
			b.gpu_hours,
			b.egress_bytes,
			b.model_tokens,
			b.build_minutes,
		);
	}
	out
}

/// The last `days` days as CSV at `grain`; needs `read quota`. Taking a copy is recorded.
///
/// # Errors
/// As [`history`].
pub async fn export(
	s: &AppState,
	user: UserId,
	team: TeamId,
	grain: Grain,
	days: i64,
) -> AppResult<String> {
	let now = OffsetDateTime::now_utc();
	let since = now - Duration::days(days.clamp(1, 366));
	let buckets = history(
		&s.db,
		user,
		team,
		grain,
		since,
		now + Duration::HOUR,
		MAX_EXPORT,
	)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"usage.export",
		Target::none("usage"),
		Diff::new()
			.set("rows", i64::try_from(buckets.len()).unwrap_or(i64::MAX))
			.set("grain", grain.sql()),
	)
	.await;
	Ok(to_csv(&buckets))
}

/// Daily housekeeping from the maintenance loop: records older than the retention go.
pub async fn purge(db: &PgPool) {
	let result =
		sqlx::query("DELETE FROM usage_records WHERE hour < NOW() - make_interval(days => $1)")
			.bind(RECORD_RETENTION_DAYS)
			.execute(db)
			.await;
	if let Err(e) = result {
		tracing::warn!("failed to purge old usage records: {e}");
	}
}
