//! Env var use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database.
//!
//! Values go in and never come out: callers get an [`EnvVarKey`], which has
//! no value, not even the encrypted one.

use sqlx::PgPool;

use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvVarId, ProjectId, TeamId, UserId};
use crate::state::AppState;
use crate::{scope, validate};

/// More pasted lines than this is almost certainly a mistake.
const MAX_BULK_LINES: usize = 500;

/// An env var as callers may see it: its name only.
#[derive(Debug, Clone)]
pub struct EnvVarKey {
	pub id: EnvVarId,
	pub application_id: ApplicationId,
	pub key: String,
}

impl From<&super::EnvVar> for EnvVarKey {
	fn from(e: &super::EnvVar) -> Self {
		Self {
			id: e.id,
			application_id: e.application_id,
			key: e.key.clone(),
		}
	}
}

fn check_key(key: &str) -> AppResult<()> {
	if validate::env_key(key) {
		Ok(())
	} else {
		Err(ServerError::Validation(
			"Name must be letters, digits and underscores, and not start with a digit.".to_owned(),
		))
	}
}

fn seal(s: &AppState, value: &str) -> AppResult<Vec<u8>> {
	s.secrets
		.encrypt(value)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt value: {e}")))
}

/// Sets an env var, creating it or replacing its value; needs
/// `create env_var`. The value is not echoed back.
pub async fn upsert(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	key: &str,
	value: &str,
) -> AppResult<EnvVarKey> {
	authorize(&s.db, user, team, Action::Create, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	check_key(key)?;
	let encrypted = seal(s, value)?;
	let env_var = super::upsert(&s.db, app.id, key, &encrypted).await?;
	Ok(EnvVarKey::from(&env_var))
}

/// The keys of an application's env vars, ordered by key; needs
/// `read env_var`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<EnvVarKey>> {
	authorize(db, user, team, Action::Read, Resource::EnvVar).await?;
	let app = scope::application(db, team, project, app).await?;
	let env_vars = super::list(db, app.id).await?;
	Ok(env_vars.iter().map(EnvVarKey::from).collect())
}

/// Deletes an env var of the application; needs `delete env_var`.
pub async fn remove(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: EnvVarId,
) -> AppResult<EnvVarId> {
	authorize(db, user, team, Action::Delete, Resource::EnvVar).await?;
	let app = scope::application(db, team, project, app).await?;
	if !super::delete(db, app.id, id).await? {
		return Err(ServerError::EnvVarNotFound);
	}
	Ok(id)
}

// --- bulk paste --------------------------------------------------------------

/// One line of a pasted `.env` that did not parse. It says what is wrong and
/// where, never what the line contained: it may be a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
	/// 1-based.
	pub line: usize,
	pub message: &'static str,
}

/// What a bulk paste did: how many variables it set, and the lines it skipped.
#[derive(Debug, Clone, Default)]
pub struct BulkReport {
	pub saved: usize,
	pub errors: Vec<LineError>,
}

fn unquote(value: &str) -> Option<&str> {
	let b = value.as_bytes();
	if b.len() >= 2 && matches!(b[0], b'"' | b'\'') && b[b.len() - 1] == b[0] {
		Some(&value[1..value.len() - 1])
	} else {
		None
	}
}

/// `(line number, key, value)` of each good line of a pasted `.env`.
type Entry = (usize, String, String);

/// Parses `.env` text. Blank lines and `#` comments are skipped; an optional
/// `export ` prefix is accepted. A value is either wrapped in matching quotes
/// (taken as is) or unquoted, where surrounding whitespace and a trailing
/// ` # comment` are dropped.
pub fn parse_dotenv(text: &str) -> (Vec<Entry>, Vec<LineError>) {
	let (mut entries, mut errors) = (Vec::new(), Vec::new());
	for (i, raw) in text.lines().enumerate() {
		let line = i + 1;
		let trimmed = raw.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let trimmed = trimmed
			.strip_prefix("export ")
			.map_or(trimmed, str::trim_start);
		let Some((key, value)) = trimmed.split_once('=') else {
			errors.push(LineError {
				line,
				message: "expected KEY=VALUE",
			});
			continue;
		};
		let key = key.trim();
		if !validate::env_key(key) {
			errors.push(LineError {
				line,
				message: "the name may only use letters, digits and underscores, and not start with a digit",
			});
			continue;
		}
		let value = value.trim();
		let value = match unquote(value) {
			Some(inner) => inner,
			None => value.split(" #").next().unwrap_or(value).trim_end(),
		};
		entries.push((line, key.to_owned(), value.to_owned()));
	}
	(entries, errors)
}

/// Sets every valid line of a pasted `.env` and reports the others by line
/// number; needs `create env_var`. The valid lines are saved together, so a
/// database failure saves none of them.
pub async fn bulk_set(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	text: &str,
) -> AppResult<BulkReport> {
	authorize(&s.db, user, team, Action::Create, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	if text.lines().count() > MAX_BULK_LINES {
		return Err(ServerError::Validation(format!(
			"Paste at most {MAX_BULK_LINES} lines at a time."
		)));
	}
	let (entries, errors) = parse_dotenv(text);
	let mut tx = s.db.begin().await?;
	for (_, key, value) in &entries {
		let encrypted = seal(s, value)?;
		super::upsert(&mut *tx, app.id, key, &encrypted).await?;
	}
	tx.commit().await?;
	Ok(BulkReport {
		saved: entries.len(),
		errors,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_comments_quotes_and_export() {
		let text = "# comment\n\nA=1\nexport B = two words \nC=\"q # not comment\"\nD='x'\nE=v # trailing\nF=\n";
		let (entries, errors) = parse_dotenv(text);
		assert!(errors.is_empty(), "{errors:?}");
		let got: Vec<_> = entries
			.iter()
			.map(|(l, k, v)| (*l, k.as_str(), v.as_str()))
			.collect();
		assert_eq!(
			got,
			[
				(3, "A", "1"),
				(4, "B", "two words"),
				(5, "C", "q # not comment"),
				(6, "D", "x"),
				(7, "E", "v"),
				(8, "F", ""),
			]
		);
	}

	#[test]
	fn bad_lines_are_reported_by_number_without_their_content() {
		let (entries, errors) = parse_dotenv("OK=1\nnot a pair S3CR3T\n1BAD=2\n=3\nGOOD=4");
		assert_eq!(entries.len(), 2);
		assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), [2, 3, 4]);
		assert!(errors.iter().all(|e| !e.message.contains("S3CR3T")));
	}
}
