//! The team's Hugging Face token. Write-only: stored age-encrypted, and the
//! API only ever says whether one is configured.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use crypto::age::SecretBox;
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route("/", get(status).put(set).delete(remove))
}

#[derive(Deserialize)]
struct SetTokenRequest {
	token: String,
}

#[derive(Serialize)]
struct TokenStatus {
	configured: bool,
}

/// The decrypted token of `team`, if one is set.
///
/// # Errors
/// `Internal` when the stored blob cannot be decrypted.
pub async fn fetch(
	db: impl PgExecutor<'_>,
	secrets: &SecretBox,
	team: TeamId,
) -> Result<Option<String>, ServerError> {
	let blob: Option<Vec<u8>> =
		sqlx::query_scalar("SELECT token FROM huggingface_tokens WHERE team_id = $1")
			.bind(team)
			.fetch_optional(db)
			.await?;
	blob.map(|blob| {
		secrets.decrypt(&blob).map_err(|e| {
			tracing::error!(
				team = team.0,
				"failed to decrypt the Hugging Face token: {e}"
			);
			ServerError::Internal
		})
	})
	.transpose()
}

async fn configured(db: impl PgExecutor<'_>, team: TeamId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM huggingface_tokens WHERE team_id = $1)")
		.bind(team)
		.fetch_one(db)
		.await
}

/// Whether the team has a token.
///
/// # Errors
/// `Forbidden` without `read model_file`.
pub async fn is_configured(s: &AppState, user: UserId, team: TeamId) -> AppResult<bool> {
	authorize(&s.db, user, team, Action::Read, Resource::ModelFile).await?;
	Ok(configured(&s.db, team).await?)
}

/// Stores or replaces the token. The token is never read back out.
///
/// # Errors
/// `Forbidden` without `update model_file`; `Validation` for a malformed token.
pub async fn store(s: &AppState, user: UserId, team: TeamId, token: &str) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::ModelFile).await?;
	let token = token.trim();
	if token.is_empty()
		|| token.len() > 512
		|| token.chars().any(|c| c.is_control() || c.is_whitespace())
	{
		return Err(ServerError::Validation("invalid token".to_owned()));
	}
	let blob = s.secrets.encrypt(token).map_err(|e| {
		tracing::error!("failed to encrypt the Hugging Face token: {e}");
		ServerError::Internal
	})?;
	sqlx::query(
		"INSERT INTO huggingface_tokens (team_id, token) VALUES ($1, $2)
		ON CONFLICT (team_id) DO UPDATE SET token = EXCLUDED.token, updated_at = NOW()",
	)
	.bind(team)
	.bind(blob)
	.execute(&s.db)
	.await?;
	Ok(())
}

/// Removes the token. Removing a token that is not set is fine.
///
/// # Errors
/// `Forbidden` without `update model_file`.
pub async fn clear(s: &AppState, user: UserId, team: TeamId) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::ModelFile).await?;
	sqlx::query("DELETE FROM huggingface_tokens WHERE team_id = $1")
		.bind(team)
		.execute(&s.db)
		.await?;
	Ok(())
}

/// Whether the team has a token; caller must hold `read model_file`.
async fn status(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<TokenStatus>> {
	Ok(Json(TokenStatus {
		configured: is_configured(&s, user.id, team).await?,
	}))
}

/// Stores or replaces the token; caller must hold `update model_file`.
async fn set(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<SetTokenRequest>,
) -> AppResult<Json<TokenStatus>> {
	store(&s, user.id, team, &body.token).await?;
	Ok(Json(TokenStatus { configured: true }))
}

/// Removes the token; caller must hold `update model_file`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<TokenStatus>> {
	clear(&s, user.id, team).await?;
	Ok(Json(TokenStatus { configured: false }))
}
