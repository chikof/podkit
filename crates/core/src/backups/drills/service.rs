//! Restore drill use cases shared by the JSON API and the web UI: authorize, then scope,
//! then validate, then touch the database or the queue.
//!
//! Reading drills needs `read backup`, starting one `create backup` (like **Back up now**:
//! it reads a backup and touches nothing else), and changing the schedule `update
//! database` (like the backup schedule).

use sqlx::PgPool;
use time::OffsetDateTime;

use super::schedule::{self, Settings};
use super::{Drill, drillable};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::users;
use crate::error::AppResult;
use crate::ids::{DatabaseId, ProjectId, RestoreDrillId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

pub const DEFAULT_PAGE_SIZE: i64 = 20;
pub const MAX_PAGE_SIZE: i64 = 200;

/// One drill as the UI lists it: the row and the name of who started it.
#[derive(Debug, Clone)]
pub struct Entry {
	pub drill: Drill,
	/// `None` for a scheduled drill, or when the user no longer exists.
	pub triggered_by: Option<String>,
}

/// What the drills card needs: the history, the schedule and whether a drill can start.
#[derive(Debug, Clone)]
pub struct Overview {
	pub entries: Vec<Entry>,
	pub settings: Settings,
	/// Why **Run drill now** cannot work, if it cannot.
	pub can_run: Result<(), String>,
}

/// A database's drills, newest first, paged with `before` (a drill id) and `limit`
/// (1..=200, default 20). Caller must hold `read backup` on the team.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	before: Option<RestoreDrillId>,
	limit: Option<i64>,
) -> AppResult<Vec<Drill>> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(db, team, project, database).await?;
	let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
	Ok(super::list(db, database.id, before, limit).await?)
}

/// The drill schedule of a database. Caller must hold `read backup` on the team.
pub async fn settings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
) -> AppResult<Settings> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(db, team, project, database).await?;
	Ok(schedule::settings(db, database.id).await?)
}

/// The drills card: the newest drills with who started each, the schedule and whether a
/// drill can start now. Caller must hold `read backup` on the team.
pub async fn overview(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
) -> AppResult<Overview> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(db, team, project, database).await?;
	let rows = super::list(db, database.id, None, DEFAULT_PAGE_SIZE).await?;
	let mut names: Vec<(UserId, Option<String>)> = Vec::new();
	let mut entries = Vec::with_capacity(rows.len());
	for drill in rows {
		let triggered_by = match drill.triggered_by {
			None => None,
			Some(id) => {
				if let Some((_, name)) = names.iter().find(|(known, _)| *known == id) {
					name.clone()
				} else {
					let name = users::name_of(db, id).await?;
					names.push((id, name.clone()));
					name
				}
			}
		};
		entries.push(Entry {
			drill,
			triggered_by,
		});
	}
	let settings = schedule::settings(db, database.id).await?;
	let can_run = match drillable(&database) {
		Err(e) => Err(e.to_string()),
		Ok(()) if super::candidate(db, database.id).await?.is_none() => {
			Err(capitalized(super::NO_BACKUP))
		}
		Ok(()) => Ok(()),
	};
	Ok(Overview {
		entries,
		settings,
		can_run,
	})
}

fn capitalized(s: &str) -> String {
	let mut chars = s.chars();
	chars.next().map_or_else(String::new, |first| {
		first.to_uppercase().chain(chars).collect()
	})
}

/// Queues a drill with the newest succeeded backup and returns at once. Caller must hold
/// `create backup` on the team.
pub async fn run_now(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
) -> AppResult<Drill> {
	authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	let drill = super::queue(s, &database, Some(user)).await?;
	audit::log(
		&s.db,
		user,
		team,
		"backup.drill",
		Target::new("restore_drill", drill.id.0)
			.named(&database.name)
			.in_project(project),
		Diff::new()
			.set("database_id", database.id.0)
			.set("backup_id", drill.backup_id.map(|b| b.0)),
	)
	.await;
	Ok(drill)
}

/// Sets or clears (`None` or blank) the drill schedule, a five-field UTC cron. Caller must
/// hold `update database` on the team.
pub async fn set_schedule(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	schedule: Option<&str>,
) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	let wanted = schedule::normalize(schedule);
	if wanted.is_some() {
		drillable(&database)?;
	}
	let next_run_at = schedule::next_due(wanted.as_deref(), OffsetDateTime::now_utc())?;
	let before = schedule::settings(&s.db, database.id).await?;
	let settings = Settings {
		schedule: wanted,
		next_run_at,
	};
	schedule::store(&s.db, database.id, &settings).await?;
	audit::log(
		&s.db,
		user,
		team,
		"database.drill_schedule",
		Target::new("database", database.id)
			.named(&database.name)
			.in_project(project),
		Diff::new().changed("drill_schedule", before.schedule, settings.schedule.clone()),
	)
	.await;
	Ok(settings)
}
