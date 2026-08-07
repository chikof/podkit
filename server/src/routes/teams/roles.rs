use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::TeamId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct RoleResponse {
	pub id: i64,
	pub name: String,
	/// `true` for the built-in roles every team gets (seeded by
	/// `0009_seed_rbac_defaults`), shared across all teams rather than
	/// owned by this one.
	pub is_builtin: bool,
}

/// Lists the roles assignable within a team: the built-in global roles
/// plus any this team has defined. Lets the dashboard populate a role
/// picker for `POST/PATCH .../members`. Caller must hold `read role` on
/// the team.
pub async fn list_roles(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
) -> Result<Json<Vec<RoleResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"role",
	)
	.await?;

	let roles = state
		.roles
		.list_for_team(TeamId(team_id))
		.await?
		.into_iter()
		.map(|r| RoleResponse {
			id: r.id.0,
			is_builtin: r.is_builtin(),
			name: r.name,
		})
		.collect();

	Ok(Json(roles))
}
