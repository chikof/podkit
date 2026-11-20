//! Scoped API tokens: `Authorization: Bearer pk_<prefix>_<secret>`.
//!
//! A token acts as the user who made it, narrowed three ways: by **scope** (what kind of
//! call), by **team** (the one it was made in) and optionally by **project**. It never
//! widens anything: every handler still asks RBAC about the user, so the effective power is
//! the user's permissions intersected with the token's scopes and limits.
//!
//! [`gate`] runs in front of the whole JSON API. It resolves the token, applies the scope
//! and limits to the request's method and path, and hands the result to the `AuthUser`
//! extractor as a request extension. Handlers that reveal secrets ask the grant for
//! `read:sensitive` themselves. The web UI never installs the gate, so a token presented to
//! a UI route is just an invalid session.
//!
//! The secret is shown once and only its SHA-256 is stored.

mod gate;
mod routes;
pub mod service;
#[cfg(test)]
mod tests;

use time::OffsetDateTime;

use crate::ids::{ApiTokenId, ProjectId, TeamId, UserId};

pub use gate::gate;
pub use routes::router;

/// Every token starts with this, so a pasted one is recognisable (and scannable).
pub const PREFIX: &str = "pk_";

/// What a token may do. Scopes do not nest except as listed on [`Grant::allows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
	/// `GET`s. Secrets stay redacted.
	Read,
	/// `read`, and secrets such as database passwords are included.
	ReadSensitive,
	/// Create and update.
	Write,
	/// Deploy, restart, stop.
	Deploy,
	/// Delete.
	Delete,
	/// Everything, and the only scope that may manage tokens.
	Root,
}

impl Scope {
	pub const ALL: [Self; 6] = [
		Self::Read,
		Self::ReadSensitive,
		Self::Write,
		Self::Deploy,
		Self::Delete,
		Self::Root,
	];

	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Read => "read",
			Self::ReadSensitive => "read:sensitive",
			Self::Write => "write",
			Self::Deploy => "deploy",
			Self::Delete => "delete",
			Self::Root => "root",
		}
	}

	pub fn parse(raw: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|s| s.as_str() == raw)
	}
}

/// A stored token. The hash never leaves the service.
#[derive(Debug, Clone)]
pub struct ApiToken {
	pub id: ApiTokenId,
	pub name: String,
	/// The public half of the token, shown in lists so it can be told apart.
	pub prefix: String,
	pub scopes: Vec<Scope>,
	/// Empty means every project of the team.
	pub project_ids: Vec<ProjectId>,
	pub expires_at: Option<OffsetDateTime>,
	pub last_used_at: Option<OffsetDateTime>,
	pub created_at: OffsetDateTime,
}

/// What an authenticated token may do, attached to the request.
#[derive(Debug, Clone)]
pub struct Grant {
	pub team: TeamId,
	pub user: UserId,
	pub scopes: Vec<Scope>,
	pub projects: Vec<ProjectId>,
}

impl Grant {
	/// Whether the token carries `need`. `root` carries every scope and `read:sensitive`
	/// carries `read`.
	pub fn allows(&self, need: Scope) -> bool {
		self.scopes.iter().any(|have| {
			*have == need
				|| *have == Scope::Root
				|| (*have == Scope::ReadSensitive && need == Scope::Read)
		})
	}

	/// Whether the token may see `project`.
	pub fn sees(&self, project: ProjectId) -> bool {
		self.projects.is_empty() || self.projects.contains(&project)
	}

	/// Whether the token is limited to some projects.
	pub fn is_limited(&self) -> bool {
		!self.projects.is_empty()
	}
}
