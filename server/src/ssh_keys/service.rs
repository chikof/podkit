//! Vault use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database.

use sqlx::PgPool;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use super::keygen::{self, Kind, Material};
use super::{SshKey, Usage};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{SshKeyId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

const MAX_NAME_LEN: usize = 60;

/// A key as the API and the pages see it. There is no private half here, by construction.
#[derive(Debug, Clone)]
pub struct KeyView {
	pub id: SshKeyId,
	pub team_id: TeamId,
	pub name: String,
	/// `ed25519`, `rsa`, `ecdsa` or `unknown`.
	pub algorithm: String,
	pub public_key: String,
	pub fingerprint: String,
	pub created_at: OffsetDateTime,
	pub last_used_at: Option<OffsetDateTime>,
	pub usage: Usage,
}

impl KeyView {
	pub(super) fn new(key: &SshKey, usage: Usage) -> Self {
		Self {
			id: key.id,
			team_id: key.team_id,
			name: key.name.clone(),
			algorithm: key.algorithm.clone(),
			public_key: key.public_key.clone(),
			fingerprint: key.fingerprint.clone(),
			created_at: key.created_at,
			last_used_at: key.last_used_at,
			usage,
		}
	}

	/// What the page calls the algorithm.
	#[must_use]
	pub fn algorithm_label(&self) -> &'static str {
		Kind::label(&self.algorithm)
	}
}

/// Where a new key comes from.
pub enum Source {
	/// Made here.
	Generate(Kind),
	/// A private key the user brings; refused with a passphrase.
	Import(String),
}

pub struct NewKey {
	pub name: String,
	pub source: Source,
}

/// What a server or application form chose.
pub enum KeyChoice {
	/// A key already in the vault.
	Existing(SshKeyId),
	/// A pasted private key, imported into the vault (reusing the entry if it is already there).
	Import(String),
	/// A fresh Ed25519 key.
	Generate,
}

impl KeyChoice {
	/// Reads the three ways a request can name a key: an id, a pasted key, or `generate`.
	/// `None` when none was given; more than one is a `Validation` error.
	///
	/// # Errors
	/// `Validation` when the request names a key in more than one way.
	pub fn from_request(
		id: Option<SshKeyId>,
		paste: Option<String>,
		generate: bool,
	) -> AppResult<Option<Self>> {
		let paste = paste.filter(|p| !p.trim().is_empty());
		match (id, paste, generate) {
			(None, None, false) => Ok(None),
			(Some(id), None, false) => Ok(Some(Self::Existing(id))),
			(None, Some(text), false) => Ok(Some(Self::Import(text))),
			(None, None, true) => Ok(Some(Self::Generate)),
			_ => Err(ServerError::Validation(
				"Choose one way to give the SSH key: a key from the vault, a pasted key, or a new one."
					.to_owned(),
			)),
		}
	}
}

fn validate_name(name: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if name.chars().any(char::is_control) {
		return Err(ServerError::Validation(
			"Name must not contain control characters.".to_owned(),
		));
	}
	Ok(name.to_owned())
}

fn key_error(e: &keygen::KeyError) -> ServerError {
	ServerError::Validation(e.to_string())
}

/// Makes or reads the key, off the async threads: an RSA-4096 search takes seconds.
async fn material(source: Source) -> AppResult<Material> {
	let made = tokio::task::spawn_blocking(move || match source {
		Source::Generate(kind) => keygen::generate(kind),
		Source::Import(text) => keygen::import(&text),
	})
	.await
	.map_err(|_| ServerError::Internal)?;
	made.map_err(|e| key_error(&e))
}

fn seal(s: &AppState, material: &Material) -> AppResult<Vec<u8>> {
	s.secrets
		.encrypt(&material.private_openssh)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt the key: {e}")))
}

/// Decrypts a vault key's private half. For the code that connects, never for a response.
///
/// # Errors
/// `SshKeyNotFound` when the key is gone; `Internal` when it will not decrypt.
pub async fn open_private(s: &AppState, id: SshKeyId) -> AppResult<Zeroizing<String>> {
	let sealed = super::sealed_private_key(&s.db, id)
		.await?
		.ok_or(ServerError::SshKeyNotFound)?;
	s.secrets.decrypt(&sealed).map(Zeroizing::new).map_err(|e| {
		tracing::error!(key = %id, error = %e, "ssh key decrypt failed");
		ServerError::Internal
	})
}

/// Lists a team's keys with who uses each; needs `read ssh_key`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<KeyView>> {
	authorize(db, user, team, Action::Read, Resource::SshKey).await?;
	let keys = super::list_by_team(db, team).await?;
	let mut usage = super::usage_by_team(db, team).await?;
	Ok(keys
		.iter()
		.map(|k| KeyView::new(k, usage.remove(&k.id).unwrap_or_default()))
		.collect())
}

/// One key; needs `read ssh_key`. A key of another team is `SshKeyNotFound`.
pub async fn get(db: &PgPool, user: UserId, team: TeamId, id: SshKeyId) -> AppResult<KeyView> {
	authorize(db, user, team, Action::Read, Resource::SshKey).await?;
	let key = scope::ssh_key(db, team, id).await?;
	let usage = super::usage_by_team(db, team)
		.await?
		.remove(&key.id)
		.unwrap_or_default();
	Ok(KeyView::new(&key, usage))
}

async fn store(
	s: &AppState,
	user: UserId,
	team: TeamId,
	name: &str,
	source: Source,
) -> AppResult<SshKey> {
	let material = material(source).await?;
	let key = SshKey {
		id: SshKeyId::generate(),
		team_id: team,
		name: name.to_owned(),
		algorithm: material.kind.as_str().to_owned(),
		public_key: material.public_key.clone(),
		fingerprint: material.fingerprint.clone(),
		private_key: seal(s, &material)?,
		created_by: Some(user),
		created_at: OffsetDateTime::now_utc(),
		last_used_at: None,
	};
	super::insert(&s.db, &key).await?;
	Ok(key)
}

/// Generates or imports a key into the vault; needs `create ssh_key`. An imported key the
/// team already holds is a `Conflict` naming the entry.
pub async fn create(s: &AppState, user: UserId, team: TeamId, new: NewKey) -> AppResult<KeyView> {
	authorize(&s.db, user, team, Action::Create, Resource::SshKey).await?;
	let name = validate_name(&new.name)?;
	let imported = matches!(new.source, Source::Import(_));
	if let Source::Import(text) = &new.source {
		// Check the key before anything is stored; the duplicate check needs its fingerprint.
		let read = keygen::import(text).map_err(|e| key_error(&e))?;
		if let Some(existing) = super::find_by_fingerprint(&s.db, team, &read.fingerprint).await? {
			return Err(ServerError::Conflict(format!(
				"This key is already in the vault as \"{}\".",
				existing.name
			)));
		}
	}
	let key = store(s, user, team, &name, new.source).await?;
	audit::log(
		&s.db,
		user,
		team,
		"ssh_key.create",
		Target::new("ssh_key", key.id).named(&key.name),
		Diff::new()
			.set("name", key.name.clone())
			.set("algorithm", key.algorithm.clone())
			.set("fingerprint", key.fingerprint.clone())
			.set("imported", imported)
			.secret("private_key"),
	)
	.await;
	Ok(KeyView::new(&key, Usage::default()))
}

/// The key a form ended up with.
#[derive(Debug, Clone, Copy)]
pub struct Resolved {
	pub id: SshKeyId,
	/// Made by this call, so a caller whose own insert then fails can take it back out.
	pub created: bool,
}

/// Picks the key a server or application will use: an existing one, a pasted one imported
/// into the vault under `name` (reusing the entry when the team already holds it), or a
/// fresh Ed25519 one. Needs `read ssh_key` for the first and `create ssh_key` for the others.
pub async fn resolve(
	s: &AppState,
	user: UserId,
	team: TeamId,
	choice: KeyChoice,
	name: &str,
) -> AppResult<Resolved> {
	match choice {
		KeyChoice::Existing(id) => {
			authorize(&s.db, user, team, Action::Read, Resource::SshKey).await?;
			Ok(Resolved {
				id: scope::ssh_key(&s.db, team, id).await?.id,
				created: false,
			})
		}
		KeyChoice::Import(text) => {
			authorize(&s.db, user, team, Action::Create, Resource::SshKey).await?;
			let read = keygen::import(&text).map_err(|e| key_error(&e))?;
			if let Some(existing) =
				super::find_by_fingerprint(&s.db, team, &read.fingerprint).await?
			{
				return Ok(Resolved {
					id: existing.id,
					created: false,
				});
			}
			let name = validate_name(name)?;
			let key = store(s, user, team, &name, Source::Import(text)).await?;
			Ok(Resolved {
				id: key.id,
				created: true,
			})
		}
		KeyChoice::Generate => {
			authorize(&s.db, user, team, Action::Create, Resource::SshKey).await?;
			let name = validate_name(name)?;
			let key = store(s, user, team, &name, Source::Generate(Kind::Ed25519)).await?;
			Ok(Resolved {
				id: key.id,
				created: true,
			})
		}
	}
}

/// Takes back a key [`resolve`] made when what it was for did not happen. Best effort.
pub async fn discard(s: &AppState, team: TeamId, resolved: Resolved) {
	if resolved.created {
		let _ = super::delete(&s.db, team, resolved.id).await;
	}
}

/// Deletes a key nothing uses; needs `delete ssh_key`. A key a server or application still
/// uses is a `Conflict` that names them.
pub async fn remove(db: &PgPool, user: UserId, team: TeamId, id: SshKeyId) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::SshKey).await?;
	let key = scope::ssh_key(db, team, id).await?;
	let usage = super::usage_by_team(db, team)
		.await?
		.remove(&key.id)
		.unwrap_or_default();
	if !usage.is_empty() {
		return Err(in_use(&usage));
	}
	super::delete(db, team, key.id).await.map_err(|e| {
		if crate::db::is_reference_violation(&e) {
			ServerError::Conflict("This key is still in use.".to_owned())
		} else {
			e.into()
		}
	})?;
	audit::log(
		db,
		user,
		team,
		"ssh_key.delete",
		Target::new("ssh_key", key.id).named(&key.name),
		Diff::new()
			.set("name", key.name.clone())
			.set("fingerprint", key.fingerprint.clone()),
	)
	.await;
	Ok(())
}

fn in_use(usage: &Usage) -> ServerError {
	let names: Vec<&str> = usage
		.servers
		.iter()
		.chain(&usage.applications)
		.map(|u| u.name.as_str())
		.collect();
	ServerError::Conflict(format!(
		"This key is in use by {}. Switch them to another key first.",
		names.join(", ")
	))
}

/// Rotates a key: see [`super::rotate`]. Needs `update ssh_key` and `update server`.
pub async fn rotate(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: SshKeyId,
) -> AppResult<super::rotate::Rotation> {
	super::rotate::rotate(s, &super::rotate::SystemSsh, user, team, id).await
}

/// At startup: fills in the public key and fingerprint of keys the migration backfilled,
/// which only the server can read. A key that will not derive is logged and left as it is.
pub async fn derive_missing(s: &AppState) -> AppResult<usize> {
	let mut done = 0;
	for (id, sealed) in super::underived(&s.db).await? {
		let derived = s
			.secrets
			.decrypt(&sealed)
			.map(Zeroizing::new)
			.map_err(|e| e.to_string())
			.and_then(|pem| keygen::derive(&pem).map_err(|e| e.to_string()));
		match derived {
			Ok(m) => {
				super::set_derived(&s.db, id, m.kind.as_str(), &m.public_key, &m.fingerprint)
					.await?;
				done += 1;
			}
			Err(reason) => tracing::warn!(key = %id, reason, "cannot derive the public key"),
		}
	}
	Ok(done)
}
