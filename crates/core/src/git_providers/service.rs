//! Provider use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database or the provider.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::api::{CloneAccess, Identity, RepoPage, Target, valid_repo};
use super::{Credentials, GitProvider, Kind, UsedBy, github};
use crate::applications::Application;
use crate::audit::{self, Diff};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{GitProviderId, TeamId, UserId};
use crate::state::AppState;
use crate::{applications, scope};

const MAX_NAME_LEN: usize = 60;

/// A provider as callers may see it. There is no credential in it, by construction.
#[derive(Debug, Clone)]
pub struct ProviderView {
	pub id: GitProviderId,
	pub team_id: TeamId,
	pub kind: Kind,
	pub name: String,
	pub base_url: String,
	pub created_at: OffsetDateTime,
	/// The GitHub App's URL name, for the link that installs it.
	pub app_slug: Option<String>,
	/// Whether GitHub can call podkit: a webhook secret is stored and podkit has a public
	/// address. Always `true` for the kinds whose webhooks are made per application.
	pub push_webhooks: bool,
	pub used_by: Vec<UsedBy>,
}

impl ProviderView {
	/// Where the app is installed from, for a GitHub App.
	#[must_use]
	pub fn install_url(&self) -> Option<String> {
		self.app_slug
			.as_ref()
			.map(|slug| format!("{}/apps/{slug}/installations/new", self.base_url))
	}
}

/// A token provider to connect.
pub struct NewTokenProvider {
	pub kind: Kind,
	pub name: String,
	/// Required for Gitea; GitLab defaults to gitlab.com.
	pub base_url: Option<String>,
	pub token: String,
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

/// A provider's address as stored: `http` or `https`, a host, no credentials, query or
/// trailing slash.
///
/// # Errors
/// `Validation` for anything else.
pub fn normalize_base_url(raw: &str) -> AppResult<String> {
	let bad = || {
		ServerError::Validation(
			"The address must be a web address like https://git.example.com.".to_owned(),
		)
	};
	let url = url::Url::parse(raw.trim()).map_err(|_| bad())?;
	if !matches!(url.scheme(), "http" | "https")
		|| url.host_str().is_none()
		|| !url.username().is_empty()
		|| url.password().is_some()
		|| url.query().is_some()
		|| url.fragment().is_some()
	{
		return Err(bad());
	}
	Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn seal(s: &AppState, credentials: &Credentials) -> AppResult<Vec<u8>> {
	let json = serde_json::to_string(credentials).map_err(|_| ServerError::Internal)?;
	s.secrets.encrypt(&json).map_err(|e| {
		tracing::error!(error = %e, "git provider credentials could not be encrypted");
		ServerError::Internal
	})
}

/// Decrypts a provider's credentials. For the code that talks to the provider, never for a
/// response.
pub(super) fn open(s: &AppState, provider: &GitProvider) -> AppResult<(Kind, Credentials)> {
	let kind = provider.kind().ok_or(ServerError::Internal)?;
	let json = s.secrets.decrypt(&provider.credentials).map_err(|e| {
		tracing::error!(provider = %provider.id, error = %e, "git provider credentials will not decrypt");
		ServerError::Internal
	})?;
	let credentials = serde_json::from_str(&json).map_err(|_| ServerError::Internal)?;
	Ok((kind, credentials))
}

fn view(s: &AppState, p: &GitProvider, used_by: Vec<UsedBy>) -> AppResult<ProviderView> {
	let (kind, credentials) = open(s, p)?;
	let (app_slug, push_webhooks) = match &credentials {
		Credentials::GithubApp {
			slug,
			webhook_secret,
			..
		} => (
			Some(slug.clone()),
			!webhook_secret.is_empty() && s.config.public_url.is_some(),
		),
		Credentials::Token { .. } => (None, s.config.public_url.is_some()),
	};
	Ok(ProviderView {
		id: p.id,
		team_id: p.team_id,
		kind,
		name: p.name.clone(),
		base_url: p.base_url.clone(),
		created_at: p.created_at,
		app_slug,
		push_webhooks,
		used_by,
	})
}

/// Lists a team's providers with the applications behind each; needs `read git_provider`.
pub async fn list(s: &AppState, user: UserId, team: TeamId) -> AppResult<Vec<ProviderView>> {
	authorize(&s.db, user, team, Action::Read, Resource::GitProvider).await?;
	let rows = super::list_by_team(&s.db, team).await?;
	let mut usage = super::usage_by_team(&s.db, team).await?;
	rows.iter()
		.map(|p| view(s, p, usage.remove(&p.id).unwrap_or_default()))
		.collect()
}

/// One provider; needs `read git_provider`. One of another team is `GitProviderNotFound`.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
) -> AppResult<ProviderView> {
	authorize(&s.db, user, team, Action::Read, Resource::GitProvider).await?;
	let p = scope::git_provider(&s.db, team, id).await?;
	let apps_using = super::usage_by_team(&s.db, team)
		.await?
		.remove(&p.id)
		.unwrap_or_default();
	view(s, &p, apps_using)
}

/// Connects a GitLab or Gitea with an access token; needs `create git_provider`. The token
/// is tried first, so a wrong one or a wrong address is refused with the reason and nothing
/// is stored.
pub async fn create_token(
	s: &AppState,
	user: UserId,
	team: TeamId,
	new: NewTokenProvider,
) -> AppResult<ProviderView> {
	authorize(&s.db, user, team, Action::Create, Resource::GitProvider).await?;
	if new.kind == Kind::GithubApp {
		return Err(ServerError::Validation(
			"A GitHub App is connected through GitHub, not with a token.".to_owned(),
		));
	}
	let name = validate_name(&new.name)?;
	let base_url = match (
		new.base_url.as_deref().map(str::trim),
		new.kind.default_base_url(),
	) {
		(Some(url), _) if !url.is_empty() => normalize_base_url(url)?,
		(_, Some(default)) => default.to_owned(),
		_ => {
			return Err(ServerError::Validation(format!(
				"Give the address of your {}, like https://git.example.com.",
				new.kind.label()
			)));
		}
	};
	let token = new.token.trim();
	if token.is_empty() {
		return Err(ServerError::Validation(
			"The access token is required.".to_owned(),
		));
	}
	// Resolved now, so a typo or an address podkit will not use is refused when it is typed.
	s.outbound
		.vet_url(&base_url)
		.await
		.map_err(|_| crate::net::outbound::refused())?;
	let credentials = Credentials::Token {
		token: token.to_owned(),
	};
	Target {
		kind: new.kind,
		base_url: &base_url,
		credentials: &credentials,
		http: s.outbound.vetted_client(),
	}
	.test()
	.await?;
	let provider = GitProvider {
		id: GitProviderId::generate(),
		team_id: team,
		kind: new.kind.as_str().to_owned(),
		name,
		base_url,
		credentials: seal(s, &credentials)?,
		created_by: Some(user),
		created_at: OffsetDateTime::now_utc(),
	};
	store(s, &provider).await?;
	audit::log(
		&s.db,
		user,
		team,
		"git_provider.create",
		audit::Target::new("git_provider", provider.id).named(&provider.name),
		Diff::new()
			.set("name", provider.name.clone())
			.set("kind", provider.kind.clone())
			.set("base_url", audit::scrub_url(&provider.base_url))
			.secret("token"),
	)
	.await;
	view(s, &provider, Vec::new())
}

/// Stores a GitHub App the manifest flow produced.
pub(super) async fn create_github(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
	name: &str,
	base_url: &str,
	app: &github::Converted,
) -> AppResult<ProviderView> {
	authorize(&s.db, user, team, Action::Create, Resource::GitProvider).await?;
	let credentials = Credentials::GithubApp {
		app_id: app.app_id,
		slug: app.slug.clone(),
		private_key: app.private_key.clone(),
		webhook_secret: app.webhook_secret.clone(),
	};
	let provider = GitProvider {
		id,
		team_id: team,
		kind: Kind::GithubApp.as_str().to_owned(),
		name: name.to_owned(),
		base_url: base_url.to_owned(),
		credentials: seal(s, &credentials)?,
		created_by: Some(user),
		created_at: OffsetDateTime::now_utc(),
	};
	store(s, &provider).await?;
	audit::log(
		&s.db,
		user,
		team,
		"git_provider.create",
		audit::Target::new("git_provider", provider.id).named(&provider.name),
		Diff::new()
			.set("name", provider.name.clone())
			.set("kind", provider.kind.clone())
			.set("base_url", audit::scrub_url(&provider.base_url))
			.secret("private_key")
			.secret("webhook_secret"),
	)
	.await;
	view(s, &provider, Vec::new())
}

async fn store(s: &AppState, provider: &GitProvider) -> AppResult<()> {
	super::insert(&s.db, provider).await.map_err(|e| {
		if crate::db::is_unique_violation(&e, "git_providers_team_id_name_key") {
			ServerError::Conflict(format!(
				"A git provider called \"{}\" already exists.",
				provider.name
			))
		} else {
			e.into()
		}
	})
}

/// Asks the provider who the credentials are; needs `update git_provider`.
pub async fn test(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
) -> AppResult<Identity> {
	authorize(&s.db, user, team, Action::Update, Resource::GitProvider).await?;
	let p = scope::git_provider(&s.db, team, id).await?;
	let (kind, credentials) = open(s, &p)?;
	let identity = target(s, &p, kind, &credentials).await?.test().await?;
	audit::log(
		&s.db,
		user,
		team,
		"git_provider.test",
		audit::Target::new("git_provider", p.id).named(&p.name),
		Diff::new(),
	)
	.await;
	Ok(identity)
}

/// The client for a stored provider. Its address is vetted on the way: it was fine when the
/// provider was saved, and what a name points at can change.
async fn target<'a>(
	s: &'a AppState,
	p: &'a GitProvider,
	kind: Kind,
	credentials: &'a Credentials,
) -> AppResult<Target<'a>> {
	vet_base_url(s, &p.base_url).await?;
	Ok(Target {
		kind,
		base_url: &p.base_url,
		credentials,
		// No redirects, resolves through the outbound policy.
		http: s.outbound.vetted_client(),
	})
}

/// Refuses a provider address podkit does not send requests to (`net::outbound`). An address
/// written as an IP is judged here; a name is judged by the resolver the connection uses.
pub(super) async fn vet_base_url(s: &AppState, base_url: &str) -> AppResult<()> {
	let url = url::Url::parse(base_url).map_err(|_| crate::net::outbound::refused())?;
	s.outbound
		.vet(&url)
		.await
		.map_err(|_| crate::net::outbound::refused())
}

/// One page of the repositories a provider offers, matching `query`; needs `read git_provider`.
pub async fn repos(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
	query: &str,
	page: u32,
) -> AppResult<RepoPage> {
	authorize(&s.db, user, team, Action::Read, Resource::GitProvider).await?;
	let p = scope::git_provider(&s.db, team, id).await?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.repos(query, page)
		.await
}

/// The branches of one repository, the default first; needs `read git_provider`.
pub async fn branches(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
	repo: &str,
) -> AppResult<Vec<String>> {
	authorize(&s.db, user, team, Action::Read, Resource::GitProvider).await?;
	if !valid_repo(repo) {
		return Err(ServerError::Validation(
			"That repository name is not valid.".to_owned(),
		));
	}
	let p = scope::git_provider(&s.db, team, id).await?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.branches(repo)
		.await
}

/// Deletes a provider; needs `delete git_provider`. One an application still clones through
/// is a `Conflict` that names the applications.
pub async fn remove(s: &AppState, user: UserId, team: TeamId, id: GitProviderId) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Delete, Resource::GitProvider).await?;
	let p = scope::git_provider(&s.db, team, id).await?;
	let apps_using = super::usage_by_team(&s.db, team)
		.await?
		.remove(&p.id)
		.unwrap_or_default();
	if !apps_using.is_empty() {
		let names: Vec<&str> = apps_using.iter().map(|u| u.name.as_str()).collect();
		return Err(ServerError::Conflict(format!(
			"This provider is in use by {}. Move them to another source first.",
			names.join(", ")
		)));
	}
	super::delete(&s.db, team, p.id).await.map_err(|e| {
		if crate::db::is_reference_violation(&e) {
			ServerError::Conflict("This provider is still in use.".to_owned())
		} else {
			e.into()
		}
	})?;
	audit::log(
		&s.db,
		user,
		team,
		"git_provider.delete",
		audit::Target::new("git_provider", p.id).named(&p.name),
		Diff::new()
			.set("name", p.name.clone())
			.set("kind", p.kind.clone()),
	)
	.await;
	Ok(())
}

/// What a new application takes from a provider.
pub struct Resolved {
	pub provider: GitProviderId,
	pub repo: String,
	/// The URL to clone, without credentials.
	pub repo_url: String,
}

/// Checks that `team` has the provider and that it gives access to `repo`, for an application
/// being created; needs `read git_provider`. A GitHub App must be installed on the repository.
pub async fn resolve_repo(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: GitProviderId,
	repo: &str,
) -> AppResult<Resolved> {
	authorize(&s.db, user, team, Action::Read, Resource::GitProvider).await?;
	let repo = repo.trim();
	if !valid_repo(repo) {
		return Err(ServerError::Validation(
			"Pick a repository from the list.".to_owned(),
		));
	}
	let p = scope::git_provider(&s.db, team, id).await?;
	let (kind, credentials) = open(s, &p)?;
	let access = target(s, &p, kind, &credentials)
		.await?
		.clone_access(repo)
		.await?;
	Ok(Resolved {
		provider: p.id,
		repo: repo.to_owned(),
		repo_url: access.repo_url,
	})
}

/// What the deploy job clones with: the provider's token, or a fresh installation token for a
/// GitHub App. No team check: the caller holds an application row that already passed one.
pub async fn clone_access(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
) -> AppResult<CloneAccess> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.clone_access(repo)
		.await
}

/// What happened to an application's push webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hook {
	/// A webhook was made at the provider.
	Created,
	/// The GitHub App's own webhook delivers pushes.
	ProviderLevel,
	/// podkit has no public address, so no provider can call it.
	NoPublicUrl,
	/// The provider refused; the message says why.
	Failed(String),
}

impl Hook {
	/// A line for the person who created the application, when the webhook is not working.
	#[must_use]
	pub fn notice(&self) -> Option<String> {
		match self {
			Self::Created | Self::ProviderLevel => None,
			Self::NoPublicUrl => Some(
				"Pushes will not deploy on their own: podkit has no public address, so the provider cannot call it. Deploy from the application's page, or set PUBLIC_URL."
					.to_owned(),
			),
			Self::Failed(why) => Some(format!(
				"The push webhook could not be created: {why} Deploy from the application's page, or add the webhook by hand."
			)),
		}
	}
}

/// Makes the push webhook of a GitLab or Gitea application, when podkit has a public address.
/// `secret` is the application's plaintext webhook secret, known only at creation. Never fails
/// the creation: the outcome says what to tell the user.
pub async fn after_app_created(s: &AppState, app: &Application, secret: &str) -> Hook {
	let (Some(provider), Some(repo)) = (app.git_provider_id, app.git_repo.as_deref()) else {
		return Hook::ProviderLevel;
	};
	let result = async {
		let p = super::find_by_id(&s.db, provider)
			.await?
			.ok_or(ServerError::GitProviderNotFound)?;
		let (kind, credentials) = open(s, &p)?;
		if kind == Kind::GithubApp {
			return Ok(Hook::ProviderLevel);
		}
		let Some(public) = s.config.public_url.as_deref() else {
			return Ok(Hook::NoPublicUrl);
		};
		let url = format!("{public}/webhooks/deploy/{}", app.id);
		let id = target(s, &p, kind, &credentials)
			.await?
			.create_hook(repo, &url, secret)
			.await?;
		if let Some(id) = id {
			applications::set_git_hook_id(&s.db, app.id, Some(&id)).await?;
		}
		Ok::<_, ServerError>(Hook::Created)
	}
	.await;
	result.unwrap_or_else(|e| {
		tracing::warn!(application = app.id.0, error = %e, "push webhook was not created");
		Hook::Failed(e.to_string())
	})
}

/// Takes down the webhook [`after_app_created`] made. Best effort: the application goes
/// either way.
pub async fn before_app_removed(s: &AppState, app: &Application) {
	let (Some(provider), Some(repo), Some(hook)) = (
		app.git_provider_id,
		app.git_repo.as_deref(),
		app.git_hook_id.as_deref(),
	) else {
		return;
	};
	let result = async {
		let p = super::find_by_id(&s.db, provider)
			.await?
			.ok_or(ServerError::GitProviderNotFound)?;
		let (kind, credentials) = open(s, &p)?;
		target(s, &p, kind, &credentials)
			.await?
			.delete_hook(repo, hook)
			.await
	}
	.await;
	if let Err(e) = result {
		tracing::warn!(application = app.id.0, error = %e, "push webhook was not removed");
	}
}

/// A pool is enough to look a provider up for a webhook; kept so the route code stays free of
/// queries.
pub(super) async fn for_webhook(db: &PgPool, id: GitProviderId) -> AppResult<GitProvider> {
	super::find_by_id(db, id)
		.await?
		.ok_or(ServerError::GitProviderNotFound)
}

/// What the checks of `sha` in `repo` add up to, asked through the provider. No team check:
/// autodeploy calls it for an application row that already passed one.
pub async fn ci(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	sha: &str,
) -> AppResult<crate::autodeploy::rules::Ci> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials).await?.ci(repo, sha).await
}

/// The files a push changed, asked through the provider. No team check, like [`ci`].
pub async fn changed_files(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	before: Option<&str>,
	after: &str,
	commits: &[String],
) -> AppResult<Vec<String>> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.changed_files(repo, before, after, commits)
		.await
}

/// Posts the deploy's state on a commit through the provider. No team check, like [`ci`].
pub async fn post_status(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	sha: &str,
	post: &super::api::StatusPost<'_>,
) -> AppResult<()> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.post_status(repo, sha, post)
		.await
}

/// Pull request `number` of `repo`, asked through the provider. No team check: previews call
/// it for an application row that already passed one.
pub async fn pull_request(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	number: i64,
) -> AppResult<super::PullRequest> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.pull_request(repo, number)
		.await
}

/// Whether `login` may write to `repo` at the provider. No team check, like [`pull_request`].
pub async fn can_write(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	login: &str,
	user_id: Option<i64>,
) -> AppResult<bool> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.can_write(repo, login, user_id)
		.await
}

/// Writes podkit's comment in a pull request, editing `existing` when it is still there.
/// Returns the comment's id. No team check, like [`pull_request`].
pub async fn upsert_comment(
	s: &AppState,
	provider: GitProviderId,
	repo: &str,
	number: i64,
	existing: Option<&str>,
	body: &str,
) -> AppResult<String> {
	let p = super::find_by_id(&s.db, provider)
		.await?
		.ok_or(ServerError::GitProviderNotFound)?;
	let (kind, credentials) = open(s, &p)?;
	target(s, &p, kind, &credentials)
		.await?
		.upsert_comment(repo, number, existing, body)
		.await
}

/// Makes the application's webhook at the provider again, so that it also sends the pull
/// request events previews follow. A webhook made before previews existed sends pushes only.
/// Best effort, like [`after_app_created`]: the application keeps working either way.
pub async fn refresh_hook(s: &AppState, app: &Application) {
	let (Some(provider), Some(repo), Some(old), Some(sealed)) = (
		app.git_provider_id,
		app.git_repo.as_deref(),
		app.git_hook_id.as_deref(),
		app.webhook_secret.as_deref(),
	) else {
		return;
	};
	let result = async {
		let p = super::find_by_id(&s.db, provider)
			.await?
			.ok_or(ServerError::GitProviderNotFound)?;
		let (kind, credentials) = open(s, &p)?;
		if kind == Kind::GithubApp {
			return Ok(());
		}
		let Some(public) = s.config.public_url.as_deref() else {
			return Ok(());
		};
		let secret = s
			.secrets
			.decrypt(sealed)
			.map_err(|_| ServerError::Internal)?;
		let target = target(s, &p, kind, &credentials).await?;
		let url = format!("{public}/webhooks/deploy/{}", app.id);
		// The new one first: a failure leaves the old one working.
		if let Some(id) = target.create_hook(repo, &url, &secret).await? {
			applications::set_git_hook_id(&s.db, app.id, Some(&id)).await?;
			if let Err(e) = target.delete_hook(repo, old).await {
				tracing::warn!(application = app.id.0, error = %e, "the old push webhook was not removed");
			}
		}
		Ok::<_, ServerError>(())
	}
	.await;
	if let Err(e) = result {
		tracing::warn!(application = app.id.0, error = %e, "the webhook was not refreshed for previews");
	}
}
