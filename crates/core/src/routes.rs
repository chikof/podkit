use axum::Router;
use axum::http::{Extensions, HeaderMap, StatusCode, Version, header};
use axum::routing::get;
use tower_cookies::CookieManagerLayer;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{Predicate, SizeAbove};

#[cfg(feature = "cloud")]
use crate::billing;
use crate::error::AppResult;
use crate::state::AppState;
use crate::{
	applications, audit, auth, auto_update, avatars, backups, catalog, cron, databases,
	deployments, domains, env_vars, environments, git_providers, iac, importers, links, log_search,
	logos, mcp, mcp_servers, members, metrics, model_files, models, moves, networks, notifications,
	projects, quadlet, quotas, registries, scanning, servers, ssh_keys, stacks, tailscale, teams,
	templates, terminal, uptime, volume_backups, volumes, webhooks,
};

const APP: &str = "/teams/{team_id}/projects/{project_id}/applications/{application_id}";
const DB: &str = "/teams/{team_id}/projects/{project_id}/databases";
const MODELS: &str = "/teams/{team_id}/projects/{project_id}/models";
const STACKS: &str = "/teams/{team_id}/projects/{project_id}/stacks";

async fn health() -> &'static str {
	"ok"
}

/// Everything this crate serves and nothing else: the JSON API, the MCP endpoint, the
/// webhooks and the public files. It is [`routes_with`] with no web UI; the `server` binary
/// serves `podkit_ui::routes`, which adds the pages.
pub async fn routes(state: AppState) -> AppResult<Router> {
	routes_with(state, |public| public).await
}

/// The JSON API lives under `/api/v1`; the root belongs to the web UI. The
/// webhook is also mounted at `/webhooks/deploy/{id}` because git providers
/// are already configured with that URL. The API and the UI share one origin,
/// so there is no CORS layer: browsers can only call the API from podkit's
/// own pages.
///
/// `web` gets the public router before the layers that wrap every route go on, and gives
/// it back with the web UI's routes and its fallback (the 404 page) added: the UI is a layer
/// above this crate, so it is handed in rather than named here.
#[allow(
	clippy::too_many_lines,
	reason = "one route table; each resource nests its router here"
)]
#[allow(
	clippy::unused_async,
	reason = "every caller awaits it, as they awaited `routes` before it took the web layer"
)]
pub async fn routes_with(
	state: AppState,
	web: impl FnOnce(Router<AppState>) -> Router<AppState>,
) -> AppResult<Router> {
	let api = Router::new()
		.nest("/auth", auth::router())
		.merge(avatars::api_router())
		.merge(auth::me_router())
		.nest("/teams", teams::router())
		.nest("/teams/{team_id}", members::router())
		.nest("/teams/{team_id}/audit", audit::router())
		.nest("/teams/{team_id}/projects", projects::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/environments",
			environments::router(),
		)
		.nest(
			"/teams/{team_id}/projects/{project_id}",
			iac::router().merge(quadlet::routes::router()),
		)
		.nest("/teams/{team_id}/quadlet", quadlet::routes::team_router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/cron",
			cron::router(),
		)
		.nest("/teams/{team_id}/import", iac::import_router())
		.nest("/teams/{team_id}/importers", importers::router())
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
		.nest("/teams/{team_id}/uptime", uptime::router())
		.nest("/teams/{team_id}/terminal", terminal::router())
		.nest("/teams/{team_id}/logs", log_search::router())
		.nest("/teams/{team_id}/tailscale", tailscale::router())
		.nest(&format!("{APP}/tailscale"), tailscale::application_router())
		.nest("/teams/{team_id}/usage", quotas::router())
		.nest(
			"/teams/{team_id}/servers/{server_id}/metrics",
			metrics::router(),
		)
		.nest(&format!("{APP}/metrics"), metrics::application_router())
		.nest(&format!("{APP}/move"), moves::app_router())
		.nest(&format!("{APP}/moves"), moves::history_router())
		.nest(
			"/teams/{team_id}/servers/{server_id}/drain",
			moves::server_router(),
		)
		.nest(
			"/teams/{team_id}/servers/{server_id}/undrain",
			moves::undrain_router(),
		)
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
		.nest(&format!("{APP}/auto-update"), auto_update::router())
		.nest(&format!("{APP}/env-vars"), env_vars::router())
		.nest(&format!("{APP}/domains"), domains::router())
		.nest(&format!("{APP}/volumes"), volumes::router())
		.nest(
			&format!("{APP}/volumes/{{volume_id}}/backups"),
			volume_backups::app_router(),
		)
		.nest(
			&format!("{STACKS}/{{stack_id}}/volumes/{{volume_key}}/backups"),
			volume_backups::stack_router(),
		)
		.nest(&format!("{APP}/models"), links::router())
		.nest(&format!("{APP}/mcp"), mcp_servers::app_router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/mcp-servers",
			mcp_servers::router(),
		)
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
		.nest(
			&format!("{DB}/{{database_id}}/drills"),
			backups::drills::router(),
		)
		.nest("/teams/{team_id}/security", scanning::routes::router())
		.nest("/webhooks", webhooks::router())
		// Inside the token gate, so a `pk_` token is already resolved: a team that is not the
		// caller's is a 404 before any handler or extractor runs.
		.layer(axum::middleware::from_fn_with_state(
			state.clone(),
			auth::team_gate::gate,
		))
		// Last, so it wraps every route above: a `pk_` token is resolved and limited here.
		.layer(axum::middleware::from_fn_with_state(
			state.clone(),
			auth::tokens::gate,
		));
	// The MCP endpoint sends its tool calls through this same router, so the token gate
	// and RBAC decide them.
	let mcp = mcp::router(api.clone().with_state(state.clone()));
	let public = Router::new()
		.merge(mcp)
		.merge(terminal::ws_router())
		.merge(logos::router())
		.merge(git_providers::public_router())
		.merge(avatars::public_router())
		.route("/health", get(health))
		.nest("/api/v1", api)
		.nest("/webhooks", webhooks::router());
	// The billing provider's callback: public, the provider checks its own signature.
	#[cfg(feature = "cloud")]
	let public = public.merge(billing::router());
	Ok(web(public)
		.layer(CookieManagerLayer::new())
		.layer(axum::middleware::from_fn(audit::layer))
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
	async fn api_is_served_under_api_v1(db: PgPool) {
		let app = app(db).await;
		let (token, _) = user_with_team(&app, "alice").await;
		let (s, body) = call(&app, "GET", "/api/v1/teams", Some(&token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert!(body.is_array());
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
