use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::keygen::Kind;
use super::rotate::{DEPLOY_KEY_NOTE, Outcome, Rotation};
use super::service::{self, KeyView, NewKey, Source};
use super::{Usage, UsedBy};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{SshKeyId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{key_id}", get(show).delete(remove))
		.route("/{key_id}/rotate", post(rotate))
}

#[derive(Deserialize)]
struct CreateKeyRequest {
	name: String,
	/// Generate a key of this kind (`ed25519`, the default, or `rsa`). Not with `private_key`.
	#[serde(default)]
	algorithm: Option<Kind>,
	/// Import this OpenSSH or PEM private key instead. Write-only: no response repeats it.
	#[serde(default)]
	private_key: Option<String>,
}

#[derive(Serialize)]
struct UsedByResponse {
	id: i64,
	name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	project_id: Option<i64>,
}

impl From<&UsedBy> for UsedByResponse {
	fn from(u: &UsedBy) -> Self {
		Self {
			id: u.id,
			name: u.name.clone(),
			project_id: u.project_id.map(|p| p.0),
		}
	}
}

#[derive(Serialize)]
struct UsageResponse {
	servers: Vec<UsedByResponse>,
	applications: Vec<UsedByResponse>,
}

impl From<&Usage> for UsageResponse {
	fn from(u: &Usage) -> Self {
		Self {
			servers: u.servers.iter().map(Into::into).collect(),
			applications: u.applications.iter().map(Into::into).collect(),
		}
	}
}

/// A key as the API shows it: the public half and who uses it, never the private one.
#[derive(Serialize)]
struct KeyResponse {
	id: SshKeyId,
	team_id: TeamId,
	name: String,
	algorithm: String,
	public_key: String,
	fingerprint: String,
	created_at: String,
	last_used_at: Option<String>,
	used_by: UsageResponse,
}

fn rfc3339(t: time::OffsetDateTime) -> String {
	t.format(&time::format_description::well_known::Rfc3339)
		.unwrap_or_default()
}

impl From<&KeyView> for KeyResponse {
	fn from(k: &KeyView) -> Self {
		Self {
			id: k.id,
			team_id: k.team_id,
			name: k.name.clone(),
			algorithm: k.algorithm.clone(),
			public_key: k.public_key.clone(),
			fingerprint: k.fingerprint.clone(),
			created_at: rfc3339(k.created_at),
			last_used_at: k.last_used_at.map(rfc3339),
			used_by: UsageResponse::from(&k.usage),
		}
	}
}

#[derive(Serialize)]
struct ServerOutcomeResponse {
	id: i64,
	name: String,
	/// `rotated`, `warning` (switched, old key left behind) or `failed` (still on the old key).
	status: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	message: Option<String>,
}

#[derive(Serialize)]
struct RotationResponse {
	old_key_id: SshKeyId,
	/// `null` when nothing could be moved and no application needs the new key.
	new_key: Option<KeyResponse>,
	servers: Vec<ServerOutcomeResponse>,
	/// Applications still on the old key: git deploy keys cannot be rotated for you.
	applications: Vec<UsedByResponse>,
	/// Set when `applications` is not empty.
	#[serde(skip_serializing_if = "Option::is_none")]
	note: Option<&'static str>,
}

impl From<&Rotation> for RotationResponse {
	fn from(r: &Rotation) -> Self {
		Self {
			old_key_id: r.old_key,
			new_key: r.new_key.as_ref().map(Into::into),
			servers: r
				.servers
				.iter()
				.map(|s| {
					let (status, message) = match &s.outcome {
						Outcome::Rotated => ("rotated", None),
						Outcome::Warning(m) => ("warning", Some(m.clone())),
						Outcome::Failed(m) => ("failed", Some(m.clone())),
					};
					ServerOutcomeResponse {
						id: s.id.0,
						name: s.name.clone(),
						status,
						message,
					}
				})
				.collect(),
			applications: r.applications.iter().map(Into::into).collect(),
			note: (!r.applications.is_empty()).then_some(DEPLOY_KEY_NOTE),
		}
	}
}

/// Adds a key to the team's vault: generated (`algorithm`, default Ed25519) or imported
/// (`private_key`). A passphrase-protected key is refused.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateKeyRequest>,
) -> AppResult<(StatusCode, Json<KeyResponse>)> {
	let source = match (body.private_key, body.algorithm) {
		(Some(_), Some(_)) => {
			return Err(ServerError::Validation(
				"Give either an algorithm (to generate a key) or a private_key (to import one), not both."
					.to_owned(),
			));
		}
		(Some(text), None) => Source::Import(text),
		(None, kind) => Source::Generate(kind.unwrap_or_default()),
	};
	let key = service::create(
		&s,
		user.id,
		team,
		NewKey {
			name: body.name,
			source,
		},
	)
	.await?;
	Ok((StatusCode::CREATED, Json(KeyResponse::from(&key))))
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<KeyResponse>>> {
	let keys = service::list(&s.db, user.id, team).await?;
	Ok(Json(keys.iter().map(KeyResponse::from).collect()))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, SshKeyId)>,
) -> AppResult<Json<KeyResponse>> {
	let key = service::get(&s.db, user.id, team, id).await?;
	Ok(Json(KeyResponse::from(&key)))
}

/// Deletes a key. One a server or application uses is refused with 409.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, SshKeyId)>,
) -> AppResult<StatusCode> {
	service::remove(&s.db, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}

/// Makes a new Ed25519 key and moves the servers using this one onto it, reporting each.
async fn rotate(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, SshKeyId)>,
) -> AppResult<Json<RotationResponse>> {
	let rotation = service::rotate(&s, user.id, team, id).await?;
	Ok(Json(RotationResponse::from(&rotation)))
}
