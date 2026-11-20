//! Audit log use cases shared by the JSON API and the web UI: who may read the log,
//! filter it, export it, and how long it is kept.

use sqlx::PgPool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{ActorKind, Diff, Event, Target, by, record};
use crate::auth::permission::{self, Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApiTokenId, AuditEventId, ProjectId, TeamId, UserId};
use crate::state::AppState;

/// Most events one page or one export holds.
pub const MAX_LIMIT: i64 = 1000;
pub const PAGE: i64 = 50;
pub const MAX_EXPORT: i64 = 10_000;
/// Retention a team may choose, in days.
pub const RETENTION_RANGE: std::ops::RangeInclusive<i32> = 1..=3650;

/// Which events to list. Every field narrows; `None` leaves it open.
#[derive(Debug, Clone, Default)]
pub struct Filter {
	/// Events by this user (directly or through one of their tokens).
	pub actor: Option<UserId>,
	pub actor_kind: Option<ActorKind>,
	/// An action (`app.deploy`) or its start (`app.`).
	pub action: Option<String>,
	pub target_kind: Option<String>,
	pub target_id: Option<i64>,
	pub since: Option<OffsetDateTime>,
	pub until: Option<OffsetDateTime>,
	/// Only events older than this one: the next page.
	pub before: Option<AuditEventId>,
}

/// Reading the log needs `read audit`. Someone outside the team is told the team does not
/// exist, the same as for any other resource of another team.
async fn allow(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::NotFound);
	}
	authorize(db, user, team, Action::Read, Resource::Audit).await
}

/// The newest events matching `filter`, at most `limit`; needs `read audit`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	filter: &Filter,
	limit: i64,
) -> AppResult<Vec<Event>> {
	allow(db, user, team).await?;
	fetch(db, team, filter, limit.clamp(1, MAX_LIMIT)).await
}

async fn fetch(db: &PgPool, team: TeamId, f: &Filter, limit: i64) -> AppResult<Vec<Event>> {
	let rows = sqlx::query!(
		r#"
			SELECT e.id AS "id: AuditEventId", e.actor_kind, e.actor_user_id AS "actor_user_id: UserId",
				u.name AS "actor_name?", e.actor_token_id AS "actor_token_id: ApiTokenId",
				t.name AS "token_name?", e.action, e.target_kind, e.target_id,
				e.project_id AS "project_id: ProjectId", e.summary, e.diff, e.ip, e.user_agent,
				e.created_at
			FROM audit_events e
			LEFT JOIN users u ON u.id = e.actor_user_id
			LEFT JOIN api_tokens t ON t.id = e.actor_token_id
			WHERE e.team_id = $1
				AND ($2::bigint IS NULL OR e.actor_user_id = $2)
				AND ($3::text IS NULL OR e.actor_kind = $3)
				AND ($4::text IS NULL OR left(e.action, length($4)) = $4)
				AND ($5::text IS NULL OR e.target_kind = $5)
				AND ($6::bigint IS NULL OR e.target_id = $6)
				AND ($7::timestamptz IS NULL OR e.created_at >= $7)
				AND ($8::timestamptz IS NULL OR e.created_at < $8)
				AND ($9::bigint IS NULL OR (e.created_at, e.id) < (
					SELECT b.created_at, b.id FROM audit_events b WHERE b.id = $9 AND b.team_id = $1
				))
			ORDER BY e.created_at DESC, e.id DESC
			LIMIT $10
		"#,
		team as TeamId,
		f.actor.map(|u| u.0),
		f.actor_kind.map(ActorKind::as_str),
		f.action.as_deref(),
		f.target_kind.as_deref(),
		f.target_id,
		f.since,
		f.until,
		f.before.map(|b| b.0),
		limit,
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| Event {
			id: r.id,
			actor_kind: ActorKind::parse(&r.actor_kind).unwrap_or(ActorKind::System),
			actor_user: r.actor_user_id,
			actor_name: r.actor_name,
			actor_token: r.actor_token_id,
			actor_token_name: r.token_name,
			action: r.action,
			target_kind: r.target_kind,
			target_id: r.target_id,
			project: r.project_id,
			summary: r.summary,
			diff: r.diff,
			ip: r.ip,
			user_agent: r.user_agent,
			created_at: r.created_at,
		})
		.collect())
}

/// The actions the team's log holds, for a filter to offer; needs `read audit`.
pub async fn actions(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<String>> {
	allow(db, user, team).await?;
	Ok(sqlx::query_scalar!(
		r#"SELECT DISTINCT action AS "action!" FROM audit_events WHERE team_id = $1 ORDER BY 1"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?)
}

// --- export --------------------------------------------------------------------

const CSV_HEADER: &str =
	"time,actor,actor_kind,action,target_kind,target_id,summary,ip,user_agent,diff\r\n";

/// One CSV field. A field a spreadsheet would read as a formula gets a leading `'`, so
/// that opening the export cannot run something someone put in a name.
fn csv_field(raw: &str) -> String {
	let raw = if raw.starts_with(['=', '+', '-', '@', '\t', '\r']) {
		format!("'{raw}")
	} else {
		raw.to_owned()
	};
	if raw.contains([',', '"', '\n', '\r']) {
		format!("\"{}\"", raw.replace('"', "\"\""))
	} else {
		raw
	}
}

pub fn actor_label(e: &Event) -> String {
	match (&e.actor_name, &e.actor_token_name, e.actor_kind) {
		(_, _, ActorKind::System) => "podkit".to_owned(),
		(Some(user), Some(token), _) => format!("{user} (token {token})"),
		(Some(user), None, _) => user.clone(),
		(None, _, _) => e
			.actor_user
			.map_or_else(|| "unknown".to_owned(), |u| format!("user {u}")),
	}
}

/// `events` as CSV, newest first as given.
pub fn to_csv(events: &[Event]) -> String {
	let mut out = String::from(CSV_HEADER);
	for e in events {
		let time = e.created_at.format(&Rfc3339).unwrap_or_default();
		let fields = [
			time,
			actor_label(e),
			e.actor_kind.as_str().to_owned(),
			e.action.clone(),
			e.target_kind.clone(),
			e.target_id.map(|i| i.to_string()).unwrap_or_default(),
			e.summary.clone(),
			e.ip.clone().unwrap_or_default(),
			e.user_agent.clone().unwrap_or_default(),
			e.diff.to_string(),
		];
		let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
		out.push_str(&line.join(","));
		out.push_str("\r\n");
	}
	out
}

/// The events matching `filter` as CSV, at most [`MAX_EXPORT`]; needs `read audit`.
/// Exporting is itself an event: the log says who took a copy.
pub async fn export(
	s: &AppState,
	user: UserId,
	team: TeamId,
	filter: &Filter,
) -> AppResult<String> {
	allow(&s.db, user, team).await?;
	let events = fetch(&s.db, team, filter, MAX_EXPORT).await?;
	record(
		&s.db,
		&by(user, team),
		"audit.export",
		Target::none("audit"),
		Diff::new().set("rows", i64::try_from(events.len()).unwrap_or(i64::MAX)),
	)
	.await;
	Ok(to_csv(&events))
}

// --- retention -------------------------------------------------------------------

/// Days the team keeps its events; needs `read audit`.
pub async fn retention(db: &PgPool, user: UserId, team: TeamId) -> AppResult<i32> {
	allow(db, user, team).await?;
	Ok(sqlx::query_scalar!(
		"SELECT audit_retention_days FROM teams WHERE id = $1",
		team as TeamId
	)
	.fetch_one(db)
	.await?)
}

/// Changes how long the team keeps its events; needs `update team`. Older events go at the
/// next maintenance run.
pub async fn set_retention(s: &AppState, user: UserId, team: TeamId, days: i32) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Team).await?;
	if !RETENTION_RANGE.contains(&days) {
		return Err(ServerError::Validation(format!(
			"Keep events for {} to {} days.",
			RETENTION_RANGE.start(),
			RETENTION_RANGE.end()
		)));
	}
	let old = sqlx::query_scalar!(
		"SELECT audit_retention_days FROM teams WHERE id = $1",
		team as TeamId
	)
	.fetch_one(&s.db)
	.await?;
	sqlx::query!(
		"UPDATE teams SET audit_retention_days = $2 WHERE id = $1",
		team as TeamId,
		days
	)
	.execute(&s.db)
	.await?;
	record(
		&s.db,
		&by(user, team),
		"audit.retention",
		Target::none("audit"),
		Diff::new().change("days", old, days),
	)
	.await;
	Ok(())
}

/// Deletes the events older than their team's retention. Run by maintenance.
pub async fn purge_expired(db: &PgPool) -> Result<u64, sqlx::Error> {
	let done = sqlx::query!(
		r#"
			DELETE FROM audit_events e USING teams t
			WHERE t.id = e.team_id
				AND e.created_at < NOW() - make_interval(days => t.audit_retention_days)
		"#
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected())
}
