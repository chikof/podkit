use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::{ProjectId, TeamId};
use serde::Serialize;
use std::collections::HashMap;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct ApplicationResponse {
	pub id: i64,
	pub project_id: i64,
	pub server_id: i64,
	pub name: String,
	pub slug: String,
	pub repo_url: String,
	pub git_ref: String,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	pub url: String,
}

/// Lists applications under a project; caller must hold `read application`
/// on the team. The project must belong to that team.
pub async fn list_applications(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id)): Path<(i64, i64)>,
) -> Result<Json<Vec<ApplicationResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"application",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	let servers_by_id: HashMap<i64, _> = state
		.servers
		.list_by_team(TeamId(team_id))
		.await?
		.into_iter()
		.map(|s| (s.id.0, s))
		.collect();

	let applications = state
		.applications
		.list_by_project(ProjectId(project_id))
		.await?
		.into_iter()
		.map(|app| {
			let url = servers_by_id
				.get(&app.server_id.0)
				.map(|server| runtime::public_hostname(&app.slug, server))
				.unwrap_or_default();
			ApplicationResponse {
				id: app.id.0,
				project_id: app.project_id.0,
				server_id: app.server_id.0,
				name: app.name,
				slug: app.slug,
				repo_url: app.repo_url,
				git_ref: app.git_ref,
				build_strategy: app.build_strategy.as_str().to_string(),
				dockerfile_path: app.dockerfile_path,
				container_port: app.container_port,
				memory_limit_mb: app.memory_limit_mb,
				cpu_limit: app.cpu_limit,
				url,
			}
		})
		.collect();

	Ok(Json(applications))
}
