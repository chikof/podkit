use axum::Router;
use axum::routing::get;
use tower_cookies::CookieManagerLayer;

use crate::error::AppResult;
use crate::state::AppState;
use crate::{
	applications, auth, backups, catalog, databases, deployments, domains, env_vars, members,
	model_files, models, projects, servers, teams, ui, volumes, webhooks,
};

const APP: &str = "/teams/{team_id}/projects/{project_id}/applications/{application_id}";
const DB: &str = "/teams/{team_id}/projects/{project_id}/databases";
const MODELS: &str = "/teams/{team_id}/projects/{project_id}/models";

async fn health() -> &'static str {
	"ok"
}

/// The JSON API lives under `/api/v1`; the root belongs to the web UI. The
/// webhook is also mounted at `/webhooks/deploy/{id}` because git providers
/// are already configured with that URL. The API and the UI share one origin,
/// so there is no CORS layer: browsers can only call the API from podkit's
/// own pages.
pub async fn routes(state: AppState) -> AppResult<Router> {
	let api = Router::new()
		.nest("/auth", auth::router())
		.nest("/teams", teams::router())
		.nest("/teams/{team_id}", members::router())
		.nest("/teams/{team_id}/projects", projects::router())
		.nest("/teams/{team_id}/servers", servers::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/applications",
			applications::router(),
		)
		.nest(&format!("{APP}/deployments"), deployments::router())
		.nest(&format!("{APP}/env-vars"), env_vars::router())
		.nest(&format!("{APP}/domains"), domains::router())
		.nest(&format!("{APP}/volumes"), volumes::router())
		.nest(
			"/teams/{team_id}/backup-targets",
			backups::targets::router(),
		)
		.nest(DB, databases::router())
		.nest(MODELS, models::router())
		.nest("/catalog", catalog::router())
		.nest("/huggingface", catalog::hf_router())
		.nest("/teams/{team_id}/model-files", model_files::router())
		.nest(
			"/teams/{team_id}/huggingface-token",
			model_files::token::router(),
		)
		.nest(&format!("{DB}/{{database_id}}/backups"), backups::router())
		.nest("/webhooks", webhooks::router());
	Ok(Router::new()
		.merge(ui::router())
		.route("/health", get(health))
		.nest("/api/v1", api)
		.nest("/webhooks", webhooks::router())
		.layer(CookieManagerLayer::new())
		.with_state(state))
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;
	use sqlx::PgPool;

	use crate::tests::{app, call, user_with_team};

	#[sqlx::test]
	async fn api_is_served_under_api_v1(db: PgPool) {
		let app = app(db).await;
		let (token, _) = user_with_team(&app, "alice").await;
		let (s, body) = call(&app, "GET", "/api/v1/teams", Some(&token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert!(body.is_array());
	}

	#[sqlx::test]
	async fn old_api_paths_are_not_json(db: PgPool) {
		let app = app(db).await;
		let (token, team) = user_with_team(&app, "alice").await;
		for path in ["/teams".to_owned(), "/auth/login".to_owned()] {
			let (s, body) = call(&app, "GET", &path, Some(&token), None).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
			assert!(body.is_null(), "{path} returned JSON: {body}");
		}
		// The web UI owns this path now: it answers with a page, never JSON.
		let path = format!("/teams/{team}/projects");
		let (_, body) = call(&app, "GET", &path, Some(&token), None).await;
		assert!(body.is_null(), "{path} returned JSON: {body}");
	}

	#[sqlx::test]
	async fn webhook_root_and_api_paths_both_route(db: PgPool) {
		let app = app(db).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let t = Some(token.as_str());
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			t,
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s}");
		let project = project["id"].as_i64().unwrap();
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			t,
			None,
		)
		.await;
		let (s, application) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			t,
			Some(json!({
				"name": "whoami",
				"slug": "whoami",
				"server_id": servers[0]["id"].as_i64().unwrap(),
				"repo_url": "https://github.com/traefik/whoami",
				"git_ref": "master",
				"container_port": 80,
			})),
		)
		.await;
		assert!(s.is_success(), "create application: {s} {application}");
		let id = application["id"].as_i64().unwrap();

		// No credentials: both mounts reach the handler and give the same error.
		let root = call(
			&app,
			"POST",
			&format!("/webhooks/deploy/{id}"),
			None,
			Some(json!({})),
		)
		.await;
		let api = call(
			&app,
			"POST",
			&format!("/api/v1/webhooks/deploy/{id}"),
			None,
			Some(json!({})),
		)
		.await;
		assert_eq!(root, api);
		assert_eq!(root.0, StatusCode::FORBIDDEN, "{root:?}");
	}
}
