//! The presets a team keeps.

use serde::Serialize;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{PresetId, TeamId, UserId};

/// Where a kept preset came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
	/// Made from a project of this team.
	Saved,
	/// A file somebody handed in: nobody here wrote it, and it is shown as unofficial.
	Imported,
}

impl Origin {
	/// As the `origin` column holds it.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Saved => "saved",
			Self::Imported => "imported",
		}
	}
}

impl TryFrom<String> for Origin {
	type Error = String;

	fn try_from(text: String) -> Result<Self, String> {
		match text.as_str() {
			"saved" => Ok(Self::Saved),
			"imported" => Ok(Self::Imported),
			_ => Err(format!("unknown preset origin {text:?}")),
		}
	}
}

/// One kept preset.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Preset {
	pub id: PresetId,
	pub team_id: TeamId,
	pub slug: String,
	pub name: String,
	pub description: String,
	#[sqlx(try_from = "String")]
	pub origin: Origin,
	/// The project file. Left out of lists.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub file: String,
	pub created_by: Option<UserId>,
	#[serde(serialize_with = "rfc3339")]
	pub created_at: OffsetDateTime,
}

fn rfc3339<S: serde::Serializer>(t: &OffsetDateTime, to: S) -> Result<S::Ok, S::Error> {
	let text = t
		.format(&time::format_description::well_known::Rfc3339)
		.map_err(serde::ser::Error::custom)?;
	to.serialize_str(&text)
}

/// # Errors
/// A unique violation when the team has a preset of that slug.
pub async fn insert(db: impl PgExecutor<'_>, preset: &Preset) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO presets (id, team_id, slug, name, description, origin, file, created_by) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
	)
	.bind(preset.id)
	.bind(preset.team_id)
	.bind(&preset.slug)
	.bind(&preset.name)
	.bind(&preset.description)
	.bind(preset.origin.as_str())
	.bind(&preset.file)
	.bind(preset.created_by)
	.execute(db)
	.await?;
	Ok(())
}

/// The team's presets by name, without their files.
///
/// # Errors
/// Database failures.
pub async fn list(db: impl PgExecutor<'_>, team: TeamId) -> Result<Vec<Preset>, sqlx::Error> {
	sqlx::query_as(
		"SELECT id, team_id, slug, name, description, origin, '' AS file, created_by, created_at \
		 FROM presets WHERE team_id = $1 ORDER BY name, id",
	)
	.bind(team)
	.fetch_all(db)
	.await
}

/// A team's presets with their files, for a page that draws what each one holds.
///
/// # Errors
/// Database failures.
pub async fn list_whole(db: impl PgExecutor<'_>, team: TeamId) -> Result<Vec<Preset>, sqlx::Error> {
	sqlx::query_as(
		"SELECT id, team_id, slug, name, description, origin, file, created_by, created_at \
		 FROM presets WHERE team_id = $1 ORDER BY name, id",
	)
	.bind(team)
	.fetch_all(db)
	.await
}

/// # Errors
/// Database failures.
pub async fn get(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: PresetId,
) -> Result<Option<Preset>, sqlx::Error> {
	sqlx::query_as(
		"SELECT id, team_id, slug, name, description, origin, file, created_by, created_at \
		 FROM presets WHERE team_id = $1 AND id = $2",
	)
	.bind(team)
	.bind(id)
	.fetch_optional(db)
	.await
}

/// Whether a preset was removed.
///
/// # Errors
/// Database failures.
pub async fn delete(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: PresetId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query("DELETE FROM presets WHERE team_id = $1 AND id = $2")
		.bind(team)
		.bind(id)
		.execute(db)
		.await?;
	Ok(done.rows_affected() == 1)
}
