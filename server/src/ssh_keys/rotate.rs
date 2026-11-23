//! Rotating a key. "Rotate" makes a new Ed25519 key and moves every server that logs in with
//! the old one over to it, one server at a time:
//!
//! 1. append the new public key to `~/.ssh/authorized_keys`, over the old key's connection;
//! 2. log in with the new key to prove it works;
//! 3. point the server at the new key;
//! 4. remove the old public key from `authorized_keys`.
//!
//! A server that fails before step 3 keeps its old key and loses nothing: the new key is
//! taken back out of `authorized_keys`. Applications cannot be moved: a git provider has to
//! be told about the new public key first, so they keep the old key and the report lists them.

use std::time::Duration;

use time::OffsetDateTime;
use zeroize::Zeroizing;

use super::keygen::{self, Kind};
use super::service::{KeyView, open_private};
use super::{SshKey, UsedBy};
use crate::agents::tunnel::{self, SshTarget};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, SshKeyId, TeamId, UserId};
use crate::scope;
use crate::servers::Server;
use crate::state::AppState;

/// What one command over ssh did.
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
	pub ok: bool,
	pub stdout: String,
	/// The start of stderr, or the exit status: never key material.
	pub detail: String,
}

/// Runs a command on a server over ssh. A seam for tests: the real one shells out to `ssh`.
pub trait SshRunner: Send + Sync {
	/// `Err` is a failure to run at all (cannot spawn, timed out); a command that ran and
	/// exited non-zero is `Ok` with `ok == false`.
	fn run(
		&self,
		target: &SshTarget,
		command: &str,
	) -> impl Future<Output = Result<ExecResult, String>> + Send;
}

/// The real thing: the system `ssh` with the server's pinned host key.
pub struct SystemSsh;

const STEP_TIMEOUT: Duration = Duration::from_secs(45);

impl SshRunner for SystemSsh {
	async fn run(&self, target: &SshTarget, command: &str) -> Result<ExecResult, String> {
		let out = tokio::time::timeout(STEP_TIMEOUT, tunnel::ssh_exec(target, command, None))
			.await
			.map_err(|_| "timed out".to_owned())?
			.map_err(|e| e.to_string())?;
		let stderr = String::from_utf8_lossy(&out.stderr);
		let stderr = stderr.trim();
		Ok(ExecResult {
			ok: out.status.success(),
			stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
			detail: if stderr.is_empty() {
				out.status.to_string()
			} else {
				stderr.chars().take(300).collect()
			},
		})
	}
}

/// How one server went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
	/// Uses the new key, and the old one is out of `authorized_keys`.
	Rotated,
	/// Uses the new key, but something afterwards did not work (the text says what).
	Warning(String),
	/// Still uses the old key.
	Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerReport {
	pub id: ServerId,
	pub name: String,
	pub outcome: Outcome,
}

/// The result of a rotation.
#[derive(Debug, Clone)]
pub struct Rotation {
	/// The new key; `None` when no server could be moved and no application needs it, so it
	/// was taken back out of the vault.
	pub new_key: Option<KeyView>,
	pub old_key: SshKeyId,
	pub servers: Vec<ServerReport>,
	/// Applications that still use the old key: update the deploy key at the git provider,
	/// then pick the new key in their settings.
	pub applications: Vec<UsedBy>,
}

/// What to tell the user about deploy keys, which no rotation can move.
pub const DEPLOY_KEY_NOTE: &str = "Update the deploy key in your git provider to the new public key, then pick the new key in each application's settings.";

const VERIFY_MARKER: &str = "podkit-key-ok";

fn verify_cmd() -> String {
	format!("echo {VERIFY_MARKER}")
}

/// Only base64 and a few separators ever go into a command: the keys podkit made.
fn safe_token(s: &str) -> bool {
	!s.is_empty()
		&& s.bytes().all(|b| {
			b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b' ' | b'.' | b'-' | b'_')
		})
}

/// Appends `line` to `authorized_keys` unless its blob is already there. Creates the file and
/// directory with the right modes and a missing final newline is added first.
fn append_cmd(line: &str, blob: &str) -> Option<String> {
	if !safe_token(line) || !safe_token(blob) {
		return None;
	}
	Some(format!(
		"set -e; umask 077; mkdir -p \"$HOME/.ssh\"; f=\"$HOME/.ssh/authorized_keys\"; touch \"$f\"; \
		if [ -s \"$f\" ] && [ -n \"$(tail -c 1 \"$f\")\" ]; then echo >> \"$f\"; fi; \
		grep -qF -e '{blob}' \"$f\" || printf '%s\\n' '{line}' >> \"$f\""
	))
}

/// Removes every line holding `blob` from `authorized_keys`, keeping the rest. A result that
/// would be empty is not written: that would lock everyone out.
fn remove_cmd(blob: &str) -> Option<String> {
	if !safe_token(blob) {
		return None;
	}
	Some(format!(
		"f=\"$HOME/.ssh/authorized_keys\"; [ -f \"$f\" ] || exit 0; t=\"$(mktemp)\" && {{ \
		grep -vF -e '{blob}' \"$f\" > \"$t\"; rc=$?; \
		if [ \"$rc\" -le 1 ] && [ -s \"$t\" ]; then cat \"$t\" > \"$f\"; fi; rm -f \"$t\"; }}"
	))
}

/// The name of the key that replaces `base`: `<name> (rotated 2026-11-19)`, however many
/// times it has been rotated.
fn rotated_name(base: &str, today: time::Date) -> String {
	let stem = base
		.rfind(" (rotated ")
		.filter(|_| base.ends_with(')'))
		.map_or(base, |i| &base[..i]);
	let name = format!("{stem} (rotated {today})");
	if name.chars().count() <= 120 {
		return name;
	}
	let keep = 120 - format!(" (rotated {today})").chars().count();
	format!(
		"{} (rotated {today})",
		stem.chars().take(keep).collect::<String>()
	)
}

struct Plan<'a> {
	old_id: SshKeyId,
	new_id: SshKeyId,
	old_sealed: &'a [u8],
	new_private: &'a Zeroizing<String>,
	old_blob: &'a str,
	new_line: &'a str,
	new_blob: &'a str,
}

/// Rotates `old` for the whole team's servers. See the module docs for the steps.
///
/// # Errors
/// `Forbidden` without `update ssh_key` and `update server`; `SshKeyNotFound` for another
/// team's key; `Validation` for a key nothing uses. Per-server trouble is in the report, not
/// an error.
#[allow(
	clippy::too_many_lines,
	reason = "the steps of a rotation read better in one place"
)]
pub async fn rotate<R: SshRunner>(
	s: &AppState,
	runner: &R,
	user: UserId,
	team: TeamId,
	old_id: SshKeyId,
) -> AppResult<Rotation> {
	authorize(&s.db, user, team, Action::Update, Resource::SshKey).await?;
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let old = scope::ssh_key(&s.db, team, old_id).await?;
	let usage = super::usage_by_team(&s.db, team)
		.await?
		.remove(&old.id)
		.unwrap_or_default();
	if usage.is_empty() {
		return Err(ServerError::Validation(
			"No server or application uses this key, so there is nothing to rotate. Delete it and make a new one instead."
				.to_owned(),
		));
	}

	let old_private = open_private(s, old.id).await?;
	let old_material =
		keygen::derive(&old_private).map_err(|e| ServerError::Validation(e.to_string()))?;
	let old_blob = keygen::blob(&old_material.public_key)
		.ok_or(ServerError::Internal)?
		.to_owned();

	let made = tokio::task::spawn_blocking(|| keygen::generate(Kind::Ed25519))
		.await
		.map_err(|_| ServerError::Internal)?
		.map_err(|e| ServerError::Validation(e.to_string()))?;
	let new_blob = keygen::blob(&made.public_key)
		.ok_or(ServerError::Internal)?
		.to_owned();
	let sealed = s
		.secrets
		.encrypt(&made.private_openssh)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt the key: {e}")))?;
	let new_key = SshKey {
		id: SshKeyId::generate(),
		team_id: team,
		name: rotated_name(&old.name, OffsetDateTime::now_utc().date()),
		algorithm: made.kind.as_str().to_owned(),
		public_key: made.public_key.clone(),
		fingerprint: made.fingerprint.clone(),
		private_key: sealed,
		created_by: Some(user),
		created_at: OffsetDateTime::now_utc(),
		last_used_at: None,
	};
	super::insert(&s.db, &new_key).await?;

	let plan = Plan {
		old_id: old.id,
		new_id: new_key.id,
		old_sealed: &old.private_key,
		new_private: &made.private_openssh,
		old_blob: &old_blob,
		new_line: &made.public_key,
		new_blob: &new_blob,
	};

	let mut servers = Vec::with_capacity(usage.servers.len());
	for used in &usage.servers {
		let id = ServerId(used.id);
		let outcome = match scope::server(&s.db, team, id).await {
			Ok(server) => rotate_server(s, runner, &server, &plan).await,
			Err(_) => Outcome::Failed("the server no longer exists".to_owned()),
		};
		servers.push(ServerReport {
			id,
			name: used.name.clone(),
			outcome,
		});
	}

	let moved = servers
		.iter()
		.any(|r| !matches!(r.outcome, Outcome::Failed(_)));
	let kept = moved || !usage.applications.is_empty();
	let new_view = if kept {
		let new_usage = super::usage_by_team(&s.db, team)
			.await?
			.remove(&new_key.id)
			.unwrap_or_default();
		let _ = super::touch(&s.db, new_key.id).await;
		Some(KeyView::new(&new_key, new_usage))
	} else {
		super::delete(&s.db, team, new_key.id).await?;
		None
	};

	let moved_ids: Vec<i64> = servers
		.iter()
		.filter(|r| !matches!(r.outcome, Outcome::Failed(_)))
		.map(|r| r.id.0)
		.collect();
	audit::log(
		&s.db,
		user,
		team,
		"ssh_key.rotate",
		Target::new("ssh_key", old.id).named(&old.name),
		Diff::new()
			.set("new_key_id", new_view.as_ref().map(|k| k.id.0))
			.set("servers_moved", moved_ids)
			.set("servers_failed", servers.len() - moved_count(&servers))
			.secret("private_key"),
	)
	.await;
	Ok(Rotation {
		new_key: new_view,
		old_key: old.id,
		servers,
		applications: usage.applications,
	})
}

fn moved_count(servers: &[ServerReport]) -> usize {
	servers
		.iter()
		.filter(|r| !matches!(r.outcome, Outcome::Failed(_)))
		.count()
}

async fn rotate_server<R: SshRunner>(
	s: &AppState,
	runner: &R,
	server: &Server,
	plan: &Plan<'_>,
) -> Outcome {
	let failed = |what: String| Outcome::Failed(what);
	let (Some(append), Some(remove_old), Some(remove_new)) = (
		append_cmd(plan.new_line, plan.new_blob),
		remove_cmd(plan.old_blob),
		remove_cmd(plan.new_blob),
	) else {
		return failed("the key has unexpected characters".to_owned());
	};

	// The target is the old key's: that is the connection the new line travels over.
	let Ok(old_target) = SshTarget::for_server(server, plan.old_sealed, &s.secrets) else {
		return failed("The server has no pinned host key yet. Bootstrap it first.".to_owned());
	};
	let new_target =
		old_target.with_private_key(Zeroizing::new(plan.new_private.as_str().to_owned()));

	// 1. The new key joins authorized_keys.
	match runner.run(&old_target, &append).await {
		Ok(r) if r.ok => {}
		Ok(r) => return failed(format!("Could not add the new key: {}", r.detail)),
		Err(e) => return failed(format!("Could not reach the server: {e}")),
	}

	// 2. It must log in.
	let verified = match runner.run(&new_target, &verify_cmd()).await {
		Ok(r) if r.ok && r.stdout.contains(VERIFY_MARKER) => Ok(()),
		Ok(r) => Err(r.detail),
		Err(e) => Err(e),
	};
	if let Err(why) = verified {
		let _ = runner.run(&old_target, &remove_new).await;
		return failed(format!(
			"The new key does not log in ({why}). The server still uses the old key."
		));
	}

	// 3. The server switches.
	match super::switch_server(&s.db, server.id, plan.old_id, plan.new_id).await {
		Ok(true) => {}
		Ok(false) => {
			let _ = runner.run(&old_target, &remove_new).await;
			return failed("The server was moved to another key meanwhile.".to_owned());
		}
		Err(e) => {
			tracing::error!(server_id = %server.id, error = %e, "ssh key switch failed");
			let _ = runner.run(&old_target, &remove_new).await;
			return failed("A database error stopped the switch.".to_owned());
		}
	}
	// A cached tunnel still runs on the old key.
	s.agents.forget(server.id);

	// 4. The old key leaves authorized_keys.
	match runner.run(&new_target, &remove_old).await {
		Ok(r) if r.ok => Outcome::Rotated,
		Ok(r) => Outcome::Warning(format!(
			"Now uses the new key, but the old public key could not be removed from authorized_keys: {}",
			r.detail
		)),
		Err(e) => Outcome::Warning(format!(
			"Now uses the new key, but the old public key could not be removed from authorized_keys: {e}"
		)),
	}
}

#[cfg(test)]
#[path = "rotate_tests.rs"]
mod tests;
