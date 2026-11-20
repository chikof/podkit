//! The team SSH key vault: keys are generated or imported once and picked by servers (to log
//! in) and applications (as their git deploy key). The private half is age-encrypted, like
//! every other credential, and nothing but the code that connects ever decrypts it: no API
//! response, page or signal carries it.

pub mod keygen;
pub mod rotate;
mod routes;
pub mod service;

pub use routes::router;

use std::collections::HashMap;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{ApplicationId, ProjectId, ServerId, SshKeyId, TeamId, UserId};

/// One vault entry. No `Debug`: the sealed private key stays out of logs.
#[derive(Clone, sqlx::FromRow)]
pub struct SshKey {
	pub id: SshKeyId,
	pub team_id: TeamId,
	pub name: String,
	/// `ed25519`, `rsa`, `ecdsa`, or `unknown` until the startup pass has derived it.
	pub algorithm: String,
	/// One `authorized_keys` line. Empty until derived.
	pub public_key: String,
	/// `SHA256:<base64>`. Empty until derived.
	pub fingerprint: String,
	/// age-encrypted OpenSSH private key.
	pub private_key: Vec<u8>,
	pub created_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub last_used_at: Option<OffsetDateTime>,
}

/// A resource that uses a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedBy {
	pub id: i64,
	pub name: String,
	/// Set for applications: they live under a project.
	pub project_id: Option<ProjectId>,
}

/// Which servers and applications use one key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
	pub servers: Vec<UsedBy>,
	pub applications: Vec<UsedBy>,
}

impl Usage {
	/// True when nothing uses the key, so it may be deleted.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.servers.is_empty() && self.applications.is_empty()
	}
}

pub async fn insert(db: impl PgExecutor<'_>, key: &SshKey) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO ssh_keys
				(id, team_id, name, algorithm, public_key, fingerprint, private_key, created_by)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		"#,
		key.id as SshKeyId,
		key.team_id as TeamId,
		key.name,
		key.algorithm,
		key.public_key,
		key.fingerprint,
		key.private_key,
		key.created_by as Option<UserId>,
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn list_by_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<SshKey>, sqlx::Error> {
	sqlx::query_as!(
		SshKey,
		r#"
			SELECT id AS "id: SshKeyId", team_id AS "team_id: TeamId", name, algorithm, public_key,
				fingerprint, private_key, created_by AS "created_by: UserId", created_at, last_used_at
			FROM ssh_keys WHERE team_id = $1 ORDER BY name, id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// The key, if it is in `team`. A key of another team is `None`, like one that is not there.
pub async fn find(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: SshKeyId,
) -> Result<Option<SshKey>, sqlx::Error> {
	sqlx::query_as!(
		SshKey,
		r#"
			SELECT id AS "id: SshKeyId", team_id AS "team_id: TeamId", name, algorithm, public_key,
				fingerprint, private_key, created_by AS "created_by: UserId", created_at, last_used_at
			FROM ssh_keys WHERE id = $2 AND team_id = $1
		"#,
		team as TeamId,
		id as SshKeyId
	)
	.fetch_optional(db)
	.await
}

/// The team's key with this fingerprint, if it has one.
pub async fn find_by_fingerprint(
	db: impl PgExecutor<'_>,
	team: TeamId,
	fingerprint: &str,
) -> Result<Option<SshKey>, sqlx::Error> {
	sqlx::query_as!(
		SshKey,
		r#"
			SELECT id AS "id: SshKeyId", team_id AS "team_id: TeamId", name, algorithm, public_key,
				fingerprint, private_key, created_by AS "created_by: UserId", created_at, last_used_at
			FROM ssh_keys WHERE team_id = $1 AND fingerprint = $2 ORDER BY id LIMIT 1
		"#,
		team as TeamId,
		fingerprint
	)
	.fetch_optional(db)
	.await
}

/// The sealed private key, for the code that connects. No team check: callers hold a server
/// or application row that already passed one.
pub async fn sealed_private_key(
	db: impl PgExecutor<'_>,
	id: SshKeyId,
) -> Result<Option<Vec<u8>>, sqlx::Error> {
	sqlx::query_scalar!(
		"SELECT private_key FROM ssh_keys WHERE id = $1",
		id as SshKeyId
	)
	.fetch_optional(db)
	.await
}

pub async fn delete(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: SshKeyId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"DELETE FROM ssh_keys WHERE id = $2 AND team_id = $1",
		team as TeamId,
		id as SshKeyId
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}

/// Notes that the key was just used, at most once an hour so a busy server does not write
/// on every connection.
pub async fn touch(db: impl PgExecutor<'_>, id: SshKeyId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE ssh_keys SET last_used_at = NOW()
		 WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < NOW() - INTERVAL '1 hour')",
		id as SshKeyId
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Fills in what the migration could not: the algorithm, public key and fingerprint of a
/// backfilled key.
pub async fn set_derived(
	db: impl PgExecutor<'_>,
	id: SshKeyId,
	algorithm: &str,
	public_key: &str,
	fingerprint: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE ssh_keys SET algorithm = $2, public_key = $3, fingerprint = $4 WHERE id = $1",
		id as SshKeyId,
		algorithm,
		public_key,
		fingerprint
	)
	.execute(db)
	.await?;
	Ok(())
}

/// The keys the migration backfilled and nobody has derived yet.
pub async fn underived(db: impl PgExecutor<'_>) -> Result<Vec<(SshKeyId, Vec<u8>)>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"SELECT id AS "id: SshKeyId", private_key FROM ssh_keys WHERE public_key = ''"#
	)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|r| (r.id, r.private_key)).collect())
}

/// Who uses each of the team's keys.
pub async fn usage_by_team(
	db: impl PgExecutor<'_> + Copy,
	team: TeamId,
) -> Result<HashMap<SshKeyId, Usage>, sqlx::Error> {
	let servers = sqlx::query!(
		r#"SELECT ssh_key_id AS "key!: SshKeyId", id AS "id: ServerId", name
		   FROM servers WHERE team_id = $1 AND ssh_key_id IS NOT NULL ORDER BY name"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?;
	let apps = sqlx::query!(
		r#"SELECT a.deploy_key_id AS "key!: SshKeyId", a.id AS "id: ApplicationId", a.name,
			a.project_id AS "project_id: ProjectId"
		   FROM applications a JOIN projects p ON p.id = a.project_id
		   WHERE p.team_id = $1 AND a.deploy_key_id IS NOT NULL ORDER BY a.name"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?;
	let mut out: HashMap<SshKeyId, Usage> = HashMap::new();
	for r in servers {
		out.entry(r.key).or_default().servers.push(UsedBy {
			id: r.id.0,
			name: r.name,
			project_id: None,
		});
	}
	for r in apps {
		out.entry(r.key).or_default().applications.push(UsedBy {
			id: r.id.0,
			name: r.name,
			project_id: Some(r.project_id),
		});
	}
	Ok(out)
}

/// Points a server at another key, but only while it still uses `from`: a concurrent switch
/// wins and this one reports `false`.
pub async fn switch_server(
	db: impl PgExecutor<'_>,
	server: ServerId,
	from: SshKeyId,
	to: SshKeyId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"UPDATE servers SET ssh_key_id = $3, updated_at = NOW() WHERE id = $1 AND ssh_key_id = $2",
		server as ServerId,
		from as SshKeyId,
		to as SshKeyId
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}
