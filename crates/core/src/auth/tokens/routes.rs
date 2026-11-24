use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use super::service::{self, CreateInput};
use super::{ApiToken, Scope};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApiTokenId, ProjectId, TeamId};
use crate::state::AppState;

/// Longest a token may live: ten years.
const MAX_DAYS: u32 = 3650;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{token_id}", delete(revoke))
}

#[derive(Deserialize)]
struct CreateTokenRequest {
	name: String,
	/// `read`, `read:sensitive`, `write`, `deploy`, `delete`, `root`.
	scopes: Vec<String>,
	/// Limits the token to these projects. Empty or omitted means every project.
	#[serde(default)]
	project_ids: Vec<ProjectId>,
	/// Days until the token stops working. Omitted means it never expires.
	#[serde(default)]
	expires_in_days: Option<u32>,
}

/// Never carries the secret or its hash; `CreatedResponse` adds the secret once.
#[derive(Serialize)]
struct TokenResponse {
	id: ApiTokenId,
	name: String,
	prefix: String,
	scopes: Vec<&'static str>,
	project_ids: Vec<ProjectId>,
	/// Unix seconds.
	expires_at: Option<i64>,
	last_used_at: Option<i64>,
	created_at: i64,
}

impl From<&ApiToken> for TokenResponse {
	fn from(t: &ApiToken) -> Self {
		Self {
			id: t.id,
			name: t.name.clone(),
			prefix: t.prefix.clone(),
			scopes: t.scopes.iter().map(|s| s.as_str()).collect(),
			project_ids: t.project_ids.clone(),
			expires_at: t.expires_at.map(OffsetDateTime::unix_timestamp),
			last_used_at: t.last_used_at.map(OffsetDateTime::unix_timestamp),
			created_at: t.created_at.unix_timestamp(),
		}
	}
}

#[derive(Serialize)]
struct CreatedResponse {
	#[serde(flatten)]
	token: TokenResponse,
	/// The whole token. This response is the only time it is ever shown.
	secret: String,
}

#[derive(Serialize)]
struct RevokedResponse {
	id: ApiTokenId,
}

pub fn parse_scopes(raw: &[String]) -> AppResult<Vec<Scope>> {
	raw.iter()
		.map(|s| {
			Scope::parse(s).ok_or_else(|| ServerError::Validation(format!("unknown scope {s:?}")))
		})
		.collect()
}

pub fn expiry(days: Option<u32>) -> AppResult<Option<OffsetDateTime>> {
	match days {
		None => Ok(None),
		Some(d) if (1..=MAX_DAYS).contains(&d) => Ok(Some(
			OffsetDateTime::now_utc() + Duration::days(i64::from(d)),
		)),
		Some(_) => Err(ServerError::Validation(format!(
			"The expiry must be 1 to {MAX_DAYS} days."
		))),
	}
}

/// Makes a token for the caller in this team; it is returned once. Needs the `root`
/// scope when called with a token.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateTokenRequest>,
) -> AppResult<Json<CreatedResponse>> {
	let input = CreateInput {
		name: body.name,
		scopes: parse_scopes(&body.scopes)?,
		project_ids: body.project_ids,
		expires_at: expiry(body.expires_in_days)?,
	};
	let created = service::create(&s.db, user.id, team, input).await?;
	Ok(Json(CreatedResponse {
		token: TokenResponse::from(&created.token),
		secret: created.secret,
	}))
}

/// The caller's tokens in this team, without secrets.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<TokenResponse>>> {
	let tokens = service::list(&s.db, user.id, team).await?;
	Ok(Json(tokens.iter().map(TokenResponse::from).collect()))
}

/// Revokes one of the caller's tokens; it stops working at once.
async fn revoke(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ApiTokenId)>,
) -> AppResult<Json<RevokedResponse>> {
	let id = service::revoke(&s.db, user.id, team, id).await?;
	Ok(Json(RevokedResponse { id }))
}
