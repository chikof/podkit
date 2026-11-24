//! Terminal use cases: who may open a shell in which container, the session record and its
//! audit trail, the team's transcript option and reading transcripts back.
//!
//! Order of refusals (the tests pin it): a token without `root` is refused; someone outside
//! the team is told the team does not exist (404); a member without `create terminal` is
//! refused (403); a resource that is not in this team's project is a 404.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::pump::{self, MAX_TRANSCRIPT, Outcome};
use crate::agents::AgentTerminal;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{self, Action, Resource, authorize};
use crate::auth::tokens::{Grant, Scope};
use crate::deployments;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DatabaseId, ProjectId, StackId, TeamId, TerminalSessionId, UserId,
};
use crate::scope;
use crate::stacks;
use crate::state::AppState;

/// What a terminal is opened in, as the URL names it: `app-12`, `database-3`,
/// `stack-5-web` (a stack's service).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
	App(ApplicationId),
	Database(DatabaseId),
	Stack(StackId, String),
}

impl Subject {
	pub fn parse(raw: &str) -> Option<Self> {
		let (kind, rest) = raw.split_once('-')?;
		let number = |s: &str| s.parse::<i64>().ok().filter(|n| *n > 0);
		match kind {
			"app" => Some(Self::App(ApplicationId(number(rest)?))),
			"database" => Some(Self::Database(DatabaseId(number(rest)?))),
			"stack" => {
				let (id, service) = rest.split_once('-')?;
				let ok = !service.is_empty()
					&& service.len() <= 128
					&& service
						.bytes()
						.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
				if !ok {
					return None;
				}
				Some(Self::Stack(StackId(number(id)?), service.to_owned()))
			}
			_ => None,
		}
	}

	/// The inverse of [`Subject::parse`], for building links.
	pub fn slug(&self) -> String {
		match self {
			Self::App(id) => format!("app-{id}"),
			Self::Database(id) => format!("database-{id}"),
			Self::Stack(id, service) => format!("stack-{id}-{service}"),
		}
	}

	const fn kind(&self) -> &'static str {
		match self {
			Self::App(_) => "application",
			Self::Database(_) => "database",
			Self::Stack(..) => "stack",
		}
	}
}

/// Who is asking: a person with a session, or an API token acting as them.
#[derive(Debug, Clone)]
pub struct Caller {
	pub user: UserId,
	pub grant: Option<Grant>,
}

/// A terminal whose checks have passed: the caller may open it, and the container is known.
/// Nothing has been connected or recorded yet.
pub struct Prepared {
	user: UserId,
	team: TeamId,
	project: ProjectId,
	server: crate::servers::Server,
	container: String,
	kind: &'static str,
	target_id: i64,
	name: String,
	service: Option<String>,
}

/// An opened terminal: what the end of the session needs to write down.
pub struct Session {
	pub id: TerminalSessionId,
	pub record: bool,
	ctx: audit::Ctx,
	target_kind: &'static str,
	target_id: i64,
	name: String,
	project: ProjectId,
	service: Option<String>,
	started: OffsetDateTime,
}

/// Runs every check and finds the container. The refusals come in the order of the module
/// docs; nothing is connected yet, so a refusal is still an ordinary HTTP answer.
///
/// # Errors
/// See the module docs; also `Conflict` when the container is not running.
pub async fn prepare(
	s: &AppState,
	caller: &Caller,
	team: TeamId,
	project: ProjectId,
	subject: &Subject,
) -> AppResult<Prepared> {
	// A token reaches a shell only with `root`, and only in its own team and projects.
	if let Some(grant) = &caller.grant {
		if grant.team != team {
			return Err(ServerError::NotFound);
		}
		if !grant.allows(Scope::Root) {
			return Err(ServerError::MissingScope(Scope::Root.as_str()));
		}
		if !grant.sees(project) {
			return Err(ServerError::ProjectNotFound);
		}
		audit::note_token(grant.token);
	}
	if !permission::granted(&s.db, caller.user, team)
		.await?
		.is_member()
	{
		return Err(ServerError::NotFound);
	}
	authorize(&s.db, caller.user, team, Action::Create, Resource::Terminal).await?;

	let (server_id, container, target_id, name, service) = match subject {
		Subject::App(id) => {
			let app = scope::application(&s.db, team, project, *id).await?;
			let running = deployments::latest_running(&s.db, app.id).await?;
			(
				app.server_id,
				running.and_then(|d| d.container_id),
				app.id.0,
				app.name,
				None,
			)
		}
		Subject::Database(id) => {
			let db = scope::database(&s.db, team, project, *id).await?;
			(db.server_id, db.container_id, db.id.0, db.name, None)
		}
		Subject::Stack(id, service) => {
			let stack = scope::stack(&s.db, team, project, *id).await?;
			let svc = stacks::find_service(&s.db, stack.id, service)
				.await?
				.ok_or(ServerError::NotFound)?;
			(
				stack.server_id,
				svc.container_id,
				stack.id.0,
				format!("{} / {service}", stack.name),
				Some(service.clone()),
			)
		}
	};
	let Some(container) = container else {
		return Err(ServerError::Conflict(
			"It isn't running, so there is no shell to open.".to_owned(),
		));
	};
	let server = scope::server(&s.db, team, server_id).await?;
	Ok(Prepared {
		user: caller.user,
		team,
		project,
		server,
		container,
		kind: subject.kind(),
		target_id,
		name,
		service,
	})
}

/// Connects to the container's agent and records the start.
///
/// # Errors
/// `Conflict` when the container is not running, `Validation` when it is gone,
/// `AgentUnavailable` otherwise.
pub async fn open(
	s: &AppState,
	prepared: Prepared,
	cols: u16,
	rows: u16,
) -> AppResult<(Session, AgentTerminal)> {
	let client = s.agents.client(s, &prepared.server).await?;
	let agent = client.terminal(&prepared.container, cols, rows).await?;

	let record = sqlx::query_scalar!(
		"SELECT terminal_record FROM teams WHERE id = $1",
		prepared.team as TeamId
	)
	.fetch_one(&s.db)
	.await?;
	let id = TerminalSessionId::generate();
	sqlx::query!(
		"INSERT INTO terminal_sessions (id, team_id, user_id, target_kind, target_id, service)
		VALUES ($1, $2, $3, $4, $5, $6)",
		id as TerminalSessionId,
		prepared.team as TeamId,
		prepared.user as UserId,
		prepared.kind,
		prepared.target_id,
		prepared.service,
	)
	.execute(&s.db)
	.await?;

	let session = Session {
		id,
		record,
		ctx: audit::by(prepared.user, prepared.team),
		target_kind: prepared.kind,
		target_id: prepared.target_id,
		name: prepared.name,
		project: prepared.project,
		service: prepared.service,
		started: OffsetDateTime::now_utc(),
	};
	audit::record(
		&s.db,
		&session.ctx,
		"terminal.start",
		session.target(),
		Diff::new()
			.set("session", id.0)
			.set("service", session.service.clone())
			.set("recorded", record),
	)
	.await;
	Ok((session, agent))
}

impl Session {
	fn target(&self) -> Target {
		Target::new(self.target_kind, self.target_id)
			.named(self.name.clone())
			.in_project(self.project)
	}

	/// The options the copy loop runs with.
	pub fn options(&self) -> pump::Options {
		pump::Options {
			idle: pump::IDLE_TIMEOUT,
			record: self.record.then_some(MAX_TRANSCRIPT),
		}
	}
}

/// Writes down how the session ended: the row, the transcript if the team records, and the
/// audit event. Keystrokes are never part of it, only how many there were.
pub async fn finish(s: &AppState, session: &Session, outcome: &Outcome) {
	let ended = sqlx::query!(
		"UPDATE terminal_sessions
		SET ended_at = NOW(), end_reason = $2, transcript = $3, transcript_truncated = $4
		WHERE id = $1",
		session.id as TerminalSessionId,
		outcome.reason.as_str(),
		outcome.transcript.as_deref(),
		outcome.truncated,
	)
	.execute(&s.db)
	.await;
	if let Err(e) = ended {
		tracing::error!(session = %session.id, "failed to close the terminal session row: {e}");
	}
	let seconds = (OffsetDateTime::now_utc() - session.started).whole_seconds();
	audit::record(
		&s.db,
		&session.ctx,
		"terminal.end",
		session.target(),
		Diff::new()
			.set("session", session.id.0)
			.set("service", session.service.clone())
			.set("reason", outcome.reason.as_str())
			.set("exit_code", outcome.exit_code)
			.set("seconds", seconds)
			.set(
				"bytes_typed",
				i64::try_from(outcome.bytes_in).unwrap_or(i64::MAX),
			)
			.set(
				"bytes_printed",
				i64::try_from(outcome.bytes_out).unwrap_or(i64::MAX),
			),
	)
	.await;
}

// --- the team's transcript option ----------------------------------------------------

/// Whether the team keeps what its terminals print. Anyone in the team may ask: the
/// terminal page tells people their session is recorded.
///
/// # Errors
/// `NotFound` outside the team.
pub async fn recording(db: &PgPool, user: UserId, team: TeamId) -> AppResult<bool> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::NotFound);
	}
	Ok(sqlx::query_scalar!(
		"SELECT terminal_record FROM teams WHERE id = $1",
		team as TeamId
	)
	.fetch_one(db)
	.await?)
}

/// Turns transcripts on or off for the team; needs `update team`. Only sessions opened
/// afterwards are affected.
///
/// # Errors
/// `Forbidden` without the permission.
pub async fn set_recording(
	s: &AppState,
	user: UserId,
	team: TeamId,
	enabled: bool,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Team).await?;
	let old = sqlx::query_scalar!(
		"SELECT terminal_record FROM teams WHERE id = $1",
		team as TeamId
	)
	.fetch_one(&s.db)
	.await?;
	sqlx::query!(
		"UPDATE teams SET terminal_record = $2 WHERE id = $1",
		team as TeamId,
		enabled
	)
	.execute(&s.db)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"terminal.record",
		Target::none("team"),
		Diff::new().change("record", old, enabled),
	)
	.await;
	Ok(())
}

// --- reading sessions back -----------------------------------------------------------

/// One row of the session list.
#[derive(Debug, Clone)]
pub struct SessionRow {
	pub id: TerminalSessionId,
	pub user_id: UserId,
	pub target_kind: String,
	pub target_id: i64,
	pub service: Option<String>,
	pub started_at: OffsetDateTime,
	pub ended_at: Option<OffsetDateTime>,
	pub end_reason: Option<String>,
	pub has_transcript: bool,
}

/// The team's latest terminal sessions, newest first; needs `read audit`.
///
/// # Errors
/// `NotFound` outside the team, `Forbidden` without the permission.
pub async fn sessions(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	limit: i64,
) -> AppResult<Vec<SessionRow>> {
	allow_read(db, user, team).await?;
	Ok(sqlx::query!(
		r#"
			SELECT id AS "id: TerminalSessionId", user_id AS "user_id: UserId", target_kind, target_id,
				service, started_at, ended_at, end_reason, (transcript IS NOT NULL) AS "has_transcript!"
			FROM terminal_sessions WHERE team_id = $1
			ORDER BY started_at DESC, id DESC LIMIT $2
		"#,
		team as TeamId,
		limit.clamp(1, 200),
	)
	.fetch_all(db)
	.await?
	.into_iter()
	.map(|r| SessionRow {
		id: r.id,
		user_id: r.user_id,
		target_kind: r.target_kind,
		target_id: r.target_id,
		service: r.service,
		started_at: r.started_at,
		ended_at: r.ended_at,
		end_reason: r.end_reason,
		has_transcript: r.has_transcript,
	})
	.collect())
}

/// What the terminal printed in one session, as the raw bytes it sent (escape codes and
/// all); needs `read audit`, and an API token also needs `read:sensitive` because a
/// transcript can hold anything a shell prints. Reading it is itself audited.
///
/// # Errors
/// `NotFound` when the session is not the team's or kept nothing.
pub async fn transcript(
	s: &AppState,
	caller: &Caller,
	team: TeamId,
	id: TerminalSessionId,
) -> AppResult<Vec<u8>> {
	if let Some(grant) = &caller.grant
		&& !grant.allows(Scope::ReadSensitive)
	{
		return Err(ServerError::MissingScope(Scope::ReadSensitive.as_str()));
	}
	allow_read(&s.db, caller.user, team).await?;
	let bytes = sqlx::query_scalar!(
		r#"SELECT transcript FROM terminal_sessions WHERE id = $1 AND team_id = $2"#,
		id as TerminalSessionId,
		team as TeamId
	)
	.fetch_optional(&s.db)
	.await?
	.flatten()
	.ok_or(ServerError::NotFound)?;
	audit::log(
		&s.db,
		caller.user,
		team,
		"terminal.transcript",
		Target::none("team"),
		Diff::new().set("session", id.0),
	)
	.await;
	Ok(bytes)
}

async fn allow_read(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::NotFound);
	}
	authorize(db, user, team, Action::Read, Resource::Audit).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn subjects_parse_and_round_trip() {
		for (raw, want) in [
			("app-12", Subject::App(ApplicationId(12))),
			("database-3", Subject::Database(DatabaseId(3))),
			("stack-5-web", Subject::Stack(StackId(5), "web".to_owned())),
			(
				"stack-5-my-api_1.x",
				Subject::Stack(StackId(5), "my-api_1.x".to_owned()),
			),
		] {
			let got = Subject::parse(raw).unwrap();
			assert_eq!(got, want, "{raw}");
			assert_eq!(got.slug(), raw);
		}
		for bad in [
			"",
			"app",
			"app-",
			"app-x",
			"app-0",
			"app--3",
			"database-1-2",
			"stack-1",
			"stack-1-",
			"stack-1-a b",
			"stack-1-../x",
			"host-1",
			"stack-x-web",
			"stack--1-web",
		] {
			assert_eq!(Subject::parse(bad), None, "{bad:?}");
		}
	}
}
