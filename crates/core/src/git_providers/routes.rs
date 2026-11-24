use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::flow::{self, StartInput};
use super::service::{self, NewTokenProvider, ProviderView};
use super::{Kind, UsedBy, hooks};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{GitProviderId, TeamId};
use crate::state::AppState;

/// The JSON API, under `/teams/{team_id}/git-providers`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/github/manifest", post(manifest))
		.route("/{provider_id}", get(show).delete(remove))
		.route("/{provider_id}/test", post(test))
		.route("/{provider_id}/repos", get(repos))
		.route("/{provider_id}/branches", get(branches))
}

/// What providers call, mounted at the root: no session, authenticated by signature.
pub fn public_router() -> Router<AppState> {
	Router::new().route(
		"/git-providers/github/webhook/{provider_id}",
		post(hooks::github),
	)
}

#[derive(Deserialize)]
struct CreateRequest {
	kind: Kind,
	name: String,
	/// Required for Gitea; GitLab defaults to gitlab.com.
	#[serde(default)]
	base_url: Option<String>,
	/// Write-only: no response repeats it.
	token: String,
}

#[derive(Serialize)]
struct UsedByResponse {
	id: i64,
	name: String,
	project_id: i64,
	repo: String,
}

impl From<&UsedBy> for UsedByResponse {
	fn from(u: &UsedBy) -> Self {
		Self {
			id: u.id,
			name: u.name.clone(),
			project_id: u.project_id.0,
			repo: u.repo.clone(),
		}
	}
}

/// A provider as the API shows it: never a token, a private key or a secret.
#[derive(Serialize)]
struct ProviderResponse {
	id: GitProviderId,
	team_id: TeamId,
	kind: Kind,
	name: String,
	base_url: String,
	created_at: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	install_url: Option<String>,
	/// Whether pushes can reach podkit: it has a public address and a webhook secret.
	push_webhooks: bool,
	used_by: Vec<UsedByResponse>,
}

fn rfc3339(t: time::OffsetDateTime) -> String {
	t.format(&time::format_description::well_known::Rfc3339)
		.unwrap_or_default()
}

impl From<&ProviderView> for ProviderResponse {
	fn from(p: &ProviderView) -> Self {
		Self {
			id: p.id,
			team_id: p.team_id,
			kind: p.kind,
			name: p.name.clone(),
			base_url: p.base_url.clone(),
			created_at: rfc3339(p.created_at),
			install_url: p.install_url(),
			push_webhooks: p.push_webhooks,
			used_by: p.used_by.iter().map(Into::into).collect(),
		}
	}
}

/// Connects a GitLab or Gitea. The token is tried against the provider first.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateRequest>,
) -> AppResult<(StatusCode, Json<ProviderResponse>)> {
	let p = service::create_token(
		&s,
		user.id,
		team,
		NewTokenProvider {
			kind: body.kind,
			name: body.name,
			base_url: body.base_url,
			token: body.token,
		},
	)
	.await?;
	Ok((StatusCode::CREATED, Json(ProviderResponse::from(&p))))
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ProviderResponse>>> {
	let all = service::list(&s, user.id, team).await?;
	Ok(Json(all.iter().map(ProviderResponse::from).collect()))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, GitProviderId)>,
) -> AppResult<Json<ProviderResponse>> {
	Ok(Json(ProviderResponse::from(
		&service::get(&s, user.id, team, id).await?,
	)))
}

/// Deletes a provider. One an application still clones through is refused with 409.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, GitProviderId)>,
) -> AppResult<StatusCode> {
	service::remove(&s, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct TestResponse {
	login: String,
	detail: String,
}

async fn test(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, GitProviderId)>,
) -> AppResult<Json<TestResponse>> {
	let who = service::test(&s, user.id, team, id).await?;
	Ok(Json(TestResponse {
		login: who.login,
		detail: who.detail,
	}))
}

#[derive(Deserialize)]
struct RepoQuery {
	#[serde(default)]
	q: String,
	#[serde(default = "first_page")]
	page: u32,
}

fn first_page() -> u32 {
	1
}

#[derive(Serialize)]
struct RepoResponse {
	full_name: String,
	default_branch: String,
	private: bool,
}

#[derive(Serialize)]
struct RepoPageResponse {
	repos: Vec<RepoResponse>,
	next_page: Option<u32>,
}

/// One page of the provider's repositories, searched server-side by `q`.
async fn repos(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, GitProviderId)>,
	Query(q): Query<RepoQuery>,
) -> AppResult<Json<RepoPageResponse>> {
	let page = service::repos(&s, user.id, team, id, &q.q, q.page).await?;
	Ok(Json(RepoPageResponse {
		repos: page
			.repos
			.into_iter()
			.map(|r| RepoResponse {
				full_name: r.full_name,
				default_branch: r.default_branch,
				private: r.private,
			})
			.collect(),
		next_page: page.next_page,
	}))
}

#[derive(Deserialize)]
struct BranchQuery {
	repo: String,
}

async fn branches(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, GitProviderId)>,
	Query(q): Query<BranchQuery>,
) -> AppResult<Json<Vec<String>>> {
	Ok(Json(
		service::branches(&s, user.id, team, id, &q.repo).await?,
	))
}

#[derive(Deserialize)]
struct ManifestRequest {
	name: String,
	#[serde(default)]
	organization: String,
	/// A GitHub Enterprise Server address; github.com when empty.
	#[serde(default)]
	base_url: String,
}

#[derive(Serialize)]
struct ManifestResponse {
	/// Post the form to this address, as the user's browser.
	action: String,
	/// The value of the form's `manifest` field.
	manifest: String,
}

/// Starts the GitHub App manifest flow: returns the form the user's browser posts to GitHub.
/// GitHub then redirects the browser to podkit's callback, which needs a web session.
async fn manifest(
	State(s): State<AppState>,
	user: AuthUser,
	headers: HeaderMap,
	Path(team): Path<TeamId>,
	Json(body): Json<ManifestRequest>,
) -> AppResult<Json<ManifestResponse>> {
	let start = flow::start(
		&s,
		user.id,
		team,
		&flow::origin(&headers),
		StartInput {
			name: body.name,
			organization: body.organization,
			base_url: body.base_url,
		},
	)
	.await?;
	Ok(Json(ManifestResponse {
		action: start.action,
		manifest: start.manifest,
	}))
}
