//! Git providers: a team connects a GitHub App, a GitLab or a Gitea once, and applications
//! pick a repository from it instead of carrying a token each. Deploys clone with a token
//! that is minted or read when the clone starts; push webhooks start the deploys.
//!
//! Everything a provider authenticates with (an access token, or a GitHub App's private key
//! and webhook secret) is one age-encrypted JSON document in `credentials`. It is decrypted
//! only to talk to the provider and never reaches an API response, a page or a log.

pub mod api;
mod ci;
pub mod flow;
mod gitea;
mod github;
mod gitlab;
mod hooks;
pub mod jwt;
mod pulls;
mod routes;
pub mod service;

pub(crate) use hooks::branch_of;
pub use pulls::PullRequest;
pub use routes::{public_router, router};

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{ApplicationId, GitProviderId, ProjectId, TeamId, UserId};

/// Which kind of git host a provider is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	/// A GitHub App, made through the app manifest flow.
	GithubApp,
	/// A GitLab, hosted or self-managed, reached with an access token.
	Gitlab,
	/// A Gitea (or Forgejo), reached with an access token.
	Gitea,
}

impl Kind {
	/// The stored form.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::GithubApp => "github_app",
			Self::Gitlab => "gitlab",
			Self::Gitea => "gitea",
		}
	}

	/// Reads the stored form.
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"github_app" => Some(Self::GithubApp),
			"gitlab" => Some(Self::Gitlab),
			"gitea" => Some(Self::Gitea),
			_ => None,
		}
	}

	/// What a page calls it.
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::GithubApp => "GitHub App",
			Self::Gitlab => "GitLab",
			Self::Gitea => "Gitea",
		}
	}

	/// The address a provider of this kind is at when none is given.
	#[must_use]
	pub fn default_base_url(self) -> Option<&'static str> {
		match self {
			Self::GithubApp => Some("https://github.com"),
			Self::Gitlab => Some("https://gitlab.com"),
			Self::Gitea => None,
		}
	}
}

/// What `credentials` holds once decrypted. No `Debug`: nothing here may reach a log.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credentials {
	/// A GitHub App: its id, its private key (PEM) and the secret its webhooks are signed with.
	GithubApp {
		/// The app's numeric id.
		app_id: i64,
		/// The app's URL name, for the install link.
		slug: String,
		/// PKCS#1 or PKCS#8 PEM.
		private_key: String,
		/// Empty when the app was made without a webhook.
		webhook_secret: String,
	},
	/// A personal or group access token of a GitLab or Gitea.
	Token {
		/// The token.
		token: String,
	},
}

/// One provider row. No `Debug`: the sealed credentials stay out of logs.
#[derive(Clone, sqlx::FromRow)]
pub struct GitProvider {
	pub id: GitProviderId,
	pub team_id: TeamId,
	pub kind: String,
	pub name: String,
	pub base_url: String,
	/// age-encrypted JSON of [`Credentials`].
	pub credentials: Vec<u8>,
	pub created_by: Option<UserId>,
	pub created_at: OffsetDateTime,
}

impl GitProvider {
	/// The kind, or `None` for a row this build does not know.
	#[must_use]
	pub fn kind(&self) -> Option<Kind> {
		Kind::parse(&self.kind)
	}
}

/// An application that clones through a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedBy {
	pub id: i64,
	pub name: String,
	pub project_id: ProjectId,
	pub repo: String,
}

pub async fn insert(db: impl PgExecutor<'_>, p: &GitProvider) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO git_providers (id, team_id, kind, name, base_url, credentials, created_by)
			VALUES ($1, $2, $3, $4, $5, $6, $7)
		"#,
		p.id as GitProviderId,
		p.team_id as TeamId,
		p.kind,
		p.name,
		p.base_url,
		p.credentials,
		p.created_by as Option<UserId>,
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn list_by_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<GitProvider>, sqlx::Error> {
	sqlx::query_as!(
		GitProvider,
		r#"
			SELECT id AS "id: GitProviderId", team_id AS "team_id: TeamId", kind, name, base_url,
				credentials, created_by AS "created_by: UserId", created_at
			FROM git_providers WHERE team_id = $1 ORDER BY name, id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// The provider, if it is in `team`. One of another team is `None`, like one that is not there.
pub async fn find(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: GitProviderId,
) -> Result<Option<GitProvider>, sqlx::Error> {
	sqlx::query_as!(
		GitProvider,
		r#"
			SELECT id AS "id: GitProviderId", team_id AS "team_id: TeamId", kind, name, base_url,
				credentials, created_by AS "created_by: UserId", created_at
			FROM git_providers WHERE id = $2 AND team_id = $1
		"#,
		team as TeamId,
		id as GitProviderId
	)
	.fetch_optional(db)
	.await
}

/// Lookup by id alone, with no team check. Only for callers that hold a row that already
/// passed one (an application being deployed) and for the provider's own webhook.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: GitProviderId,
) -> Result<Option<GitProvider>, sqlx::Error> {
	sqlx::query_as!(
		GitProvider,
		r#"
			SELECT id AS "id: GitProviderId", team_id AS "team_id: TeamId", kind, name, base_url,
				credentials, created_by AS "created_by: UserId", created_at
			FROM git_providers WHERE id = $1
		"#,
		id as GitProviderId
	)
	.fetch_optional(db)
	.await
}

pub async fn delete(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: GitProviderId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"DELETE FROM git_providers WHERE id = $2 AND team_id = $1",
		team as TeamId,
		id as GitProviderId
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}

/// The applications that clone through each of a team's providers.
pub async fn usage_by_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<std::collections::HashMap<GitProviderId, Vec<UsedBy>>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT a.git_provider_id AS "provider!: GitProviderId", a.id AS "id: ApplicationId",
				a.name, a.project_id AS "project_id: ProjectId", a.git_repo AS "repo!"
			FROM applications a JOIN projects p ON p.id = a.project_id
			WHERE p.team_id = $1 AND a.git_provider_id IS NOT NULL ORDER BY a.name
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?;
	let mut out: std::collections::HashMap<GitProviderId, Vec<UsedBy>> =
		std::collections::HashMap::new();
	for r in rows {
		out.entry(r.provider).or_default().push(UsedBy {
			id: r.id.0,
			name: r.name,
			project_id: r.project_id,
			repo: r.repo,
		});
	}
	Ok(out)
}

/// The git applications that clone `repo` through `provider`, whatever branch they follow:
/// autodeploy decides per application whether a push to a ref deploys.
pub async fn applications_for_push(
	db: impl PgExecutor<'_>,
	provider: GitProviderId,
	repo: &str,
) -> Result<Vec<ApplicationId>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT id AS "id: ApplicationId" FROM applications
			WHERE git_provider_id = $1 AND lower(git_repo) = lower($2)
				AND source_kind = 'git'
			ORDER BY id
		"#,
		provider as GitProviderId,
		repo
	)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Records a webhook delivery. `false` when `key` was seen before: a replay.
pub async fn first_delivery(
	db: impl PgExecutor<'_> + Copy,
	key: &str,
) -> Result<bool, sqlx::Error> {
	sqlx::query!("DELETE FROM git_webhook_deliveries WHERE seen_at < NOW() - INTERVAL '7 days'")
		.execute(db)
		.await?;
	let done = sqlx::query!(
		"INSERT INTO git_webhook_deliveries (delivery_id) VALUES ($1) ON CONFLICT DO NOTHING",
		key
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}

#[cfg(test)]
mod live_tests;
#[cfg(test)]
pub(crate) mod stubs;
#[cfg(test)]
pub(crate) mod tests;
