use axum::Router;
use axum::http::{Extensions, HeaderMap, StatusCode, Version, header};
use axum::routing::get;
use tower_cookies::CookieManagerLayer;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{Predicate, SizeAbove};

use crate::error::AppResult;
use crate::state::AppState;
use crate::{
	applications, auth, avatars, backups, catalog, cron, databases, deployments, domains, env_vars,
	environments, git_providers, iac, links, logos, members, metrics, model_files, models,
	networks, notifications, projects, registries, scanning, servers, ssh_keys, stacks, teams,
	templates, ui, volumes, webhooks,
};

const APP: &str = "/teams/{team_id}/projects/{project_id}/applications/{application_id}";
const DB: &str = "/teams/{team_id}/projects/{project_id}/databases";
const MODELS: &str = "/teams/{team_id}/projects/{project_id}/models";
const STACKS: &str = "/teams/{team_id}/projects/{project_id}/stacks";

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
		.merge(avatars::api_router())
		.nest("/teams", teams::router())
		.nest("/teams/{team_id}", members::router())
		.nest("/teams/{team_id}/projects", projects::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/environments",
			environments::router(),
		)
		.nest("/teams/{team_id}/projects/{project_id}", iac::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/cron",
			cron::router(),
		)
		.nest("/teams/{team_id}/import", iac::import_router())
		.nest("/teams/{team_id}/servers", servers::router())
		.nest("/teams/{team_id}/ssh-keys", ssh_keys::router())
		.nest("/teams/{team_id}/git-providers", git_providers::router())
		.nest(
			"/teams/{team_id}/servers/{server_id}/networks",
			networks::router(),
		)
		.nest("/teams/{team_id}/networks", networks::network_router())
		.nest("/teams/{team_id}/registries", registries::router())
		.nest("/teams/{team_id}/api-tokens", auth::tokens::router())
		.nest("/teams/{team_id}/notifications", notifications::router())
		.nest(
			"/teams/{team_id}/servers/{server_id}/metrics",
			metrics::router(),
		)
		.nest(&format!("{APP}/metrics"), metrics::application_router())
		.nest(
			&format!("{DB}/{{database_id}}/metrics"),
			metrics::database_router(),
		)
		.nest(
			&format!("{MODELS}/{{model_id}}/metrics"),
			metrics::model_router(),
		)
		.nest(
			&format!("{STACKS}/{{stack_id}}/metrics"),
			metrics::stack_router(),
		)
		.nest(
			"/teams/{team_id}/projects/{project_id}/applications",
			applications::router(),
		)
		.nest(&format!("{APP}/deployments"), deployments::router())
		.nest(&format!("{APP}/env-vars"), env_vars::router())
		.nest(&format!("{APP}/domains"), domains::router())
		.nest(&format!("{APP}/volumes"), volumes::router())
		.nest(&format!("{APP}/models"), links::router())
		.nest(
			"/teams/{team_id}/backup-targets",
			backups::targets::router(),
		)
		.nest(DB, databases::router())
		.nest(MODELS, models::router())
		.nest(STACKS, stacks::router())
		.nest("/templates", templates::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/templates",
			templates::deploy_router(),
		)
		.nest("/catalog", catalog::router())
		.nest("/huggingface", catalog::hf_router())
		.nest("/teams/{team_id}/model-files", model_files::router())
		.nest(
			"/teams/{team_id}/huggingface-token",
			model_files::token::router(),
		)
		.nest(&format!("{DB}/{{database_id}}/backups"), backups::router())
		.nest("/teams/{team_id}/security", scanning::routes::router())
		.nest("/webhooks", webhooks::router())
		// Last, so it wraps every route above: a `pk_` token is resolved and limited here.
		.layer(axum::middleware::from_fn_with_state(
			state.clone(),
			auth::tokens::gate,
		));
	Ok(Router::new()
		.merge(ui::router())
		.merge(logos::router())
		.merge(git_providers::public_router())
		.merge(avatars::public_router())
		.route("/health", get(health))
		.nest("/api/v1", api)
		.nest("/webhooks", webhooks::router())
		.fallback(ui::not_found)
		.layer(CookieManagerLayer::new())
		.layer(compression())
		.with_state(state))
}

/// Compress the text a browser downloads: HTML, CSS, JS and SVG, from 256 bytes up. JSON from
/// the API, event streams and the font and image files are left as they are: the fonts are
/// already compressed, and a stream has to flush each event.
fn compression() -> CompressionLayer<impl Predicate> {
	CompressionLayer::new().compress_when(SizeAbove::new(256).and(
		|_: StatusCode, _: Version, headers: &HeaderMap, _: &Extensions| {
			headers
				.get(header::CONTENT_TYPE)
				.and_then(|v| v.to_str().ok())
				.is_some_and(|ct| {
					[
						"text/html",
						"text/css",
						"text/javascript",
						"application/javascript",
						"image/svg+xml",
					]
					.iter()
					.any(|t| ct.starts_with(t))
				})
		},
	))
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;
	use sqlx::PgPool;

	use crate::tests::{app, call, user_with_team};

	#[sqlx::test]
	async fn text_assets_are_gzipped_and_json_and_fonts_are_not(db: PgPool) {
		use axum::body::Body;
		use axum::http::Request;
		use tower::ServiceExt;

		let app = app(db).await;
		let get = |uri: &'static str| {
			let app = app.clone();
			async move {
				app.oneshot(
					Request::builder()
						.uri(uri)
						.header("accept-encoding", "gzip")
						.body(Body::empty())
						.unwrap(),
				)
				.await
				.unwrap()
			}
		};
		let r = get("/static/app.css").await;
		assert_eq!(r.status(), StatusCode::OK);
		assert_eq!(r.headers()["content-encoding"], "gzip", "css");
		let r = get("/static/page.js").await;
		assert_eq!(r.headers()["content-encoding"], "gzip", "js");
		let r = get("/login").await;
		assert_eq!(r.headers()["content-encoding"], "gzip", "html");
		let r = get("/api/v1/catalog").await;
		assert!(
			r.headers().get("content-encoding").is_none(),
			"json stays plain"
		);
		let r = get("/static/fonts/hanken-grotesk-var.woff2").await;
		assert!(
			r.headers().get("content-encoding").is_none(),
			"fonts stay plain"
		);
	}

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
