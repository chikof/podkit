//! API tests of Quadlet: the units of one resource never carry a secret, the export is a
//! `.tar.gz` with a README per server, and env files with secrets need reveal rights and are
//! audited.

use std::io::Read as _;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::tests::{PgPool, app, call, db_test, member_role, revoke_permission, user_with_team};

/// A value that must never come out of the view or of an export without secrets.
const SECRET: &str = "S3CR3T-quadlet-7c1d2f3a";

struct World {
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	application: i64,
}

impl World {
	/// Alice, her team, a project `shop` and an image application `web` with one secret
	/// variable.
	async fn new(db: PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "{s} {project}");
		let project = project["id"].as_i64().unwrap();
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let server = servers[0]["id"].as_i64().unwrap();
		let (s, created) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(json!({
				"name": "Web", "slug": "web", "server_id": server,
				"image": "ghcr.io/acme/web:1.2", "container_port": 3000,
			})),
		)
		.await;
		assert!(s.is_success(), "{s} {created}");
		let application = created["id"].as_i64().unwrap();
		let (s, body) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications/{application}/env-vars"),
			Some(&token),
			Some(json!({ "key": "API_KEY", "value": SECRET })),
		)
		.await;
		assert!(s.is_success(), "{s} {body}");
		Self {
			app,
			db,
			token,
			team,
			project,
			application,
		}
	}

	fn url(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/{rest}",
			self.team, self.project
		)
	}

	async fn view(&self) -> (StatusCode, Value) {
		call(
			&self.app,
			"GET",
			&self.url(&format!("applications/{}/quadlet", self.application)),
			Some(&self.token),
			None,
		)
		.await
	}

	/// `POST` the export and unpack it: (status, every file as path and text).
	async fn export(&self, query: &str) -> (StatusCode, Vec<(String, String)>) {
		let req = Request::builder()
			.method("POST")
			.uri(format!("{}{query}", self.url("quadlet/export")))
			.header(header::AUTHORIZATION, format!("Bearer {}", self.token))
			.body(Body::empty())
			.unwrap();
		let resp = self.app.clone().oneshot(req).await.unwrap();
		let status = resp.status();
		let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
			.await
			.unwrap();
		if status != StatusCode::OK {
			return (status, Vec::new());
		}
		(status, unpack(&bytes))
	}

	async fn exports_audited(&self) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = 'quadlet.export'")
			.fetch_one(&self.db)
			.await
			.unwrap()
	}
}

fn unpack(gz: &[u8]) -> Vec<(String, String)> {
	let mut tar = Vec::new();
	flate2::read::GzDecoder::new(gz)
		.read_to_end(&mut tar)
		.unwrap();
	let mut archive = tar::Archive::new(tar.as_slice());
	let mut out = Vec::new();
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		let path = entry.path().unwrap().to_string_lossy().into_owned();
		let mut text = String::new();
		entry.read_to_string(&mut text).unwrap();
		out.push((path, text));
	}
	out
}

#[db_test]
async fn the_view_shows_the_units_and_names_the_variables_without_their_values(db: PgPool) {
	let w = World::new(db).await;
	let (s, body) = w.view().await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["kind"], "application");
	assert_eq!(body["directory"], "~/.config/containers/systemd");
	let text = body.to_string();
	assert!(!text.contains(SECRET), "{text}");
	let units = body["units"].as_array().unwrap();
	let container = units
		.iter()
		.find(|u| u["name"] == format!("podkit-app-{}.container", w.application))
		.unwrap_or_else(|| panic!("no container unit in {body}"));
	let content = container["content"].as_str().unwrap();
	assert!(content.contains("Image=ghcr.io/acme/web:1.2"), "{content}");
	assert!(
		content.contains(&format!("EnvironmentFile=podkit-app-{}.env", w.application)),
		"{content}"
	);
	assert!(content.contains("API_KEY"), "{content}");
	assert!(
		content.contains("[Install]\nWantedBy=default.target"),
		"{content}"
	);
	// It lives on its environment's network, which gets a unit of its own.
	assert!(
		units
			.iter()
			.any(|u| u["name"].as_str().unwrap().starts_with("podkit-env-")),
		"{body}"
	);
	assert_eq!(body["env_files"][0]["keys"], json!(["API_KEY"]));
}

#[db_test]
async fn another_teams_application_is_not_found(db: PgPool) {
	let w = World::new(db).await;
	let (mallory, _) = user_with_team(&w.app, "mallory").await;
	let (s, _) = call(
		&w.app,
		"GET",
		&w.url(&format!("applications/{}/quadlet", w.application)),
		Some(&mallory),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn an_export_without_secrets_has_units_and_a_readme_but_no_env_file(db: PgPool) {
	let w = World::new(db).await;
	let (s, files) = w.export("").await;
	assert_eq!(s, StatusCode::OK);
	assert!(files.iter().all(|(_, text)| !text.contains(SECRET)));
	assert!(
		files
			.iter()
			.any(|(path, _)| path
				.ends_with(&format!("/units/podkit-app-{}.container", w.application))),
		"{files:?}"
	);
	assert!(!files.iter().any(|(path, _)| {
		std::path::Path::new(path)
			.extension()
			.is_some_and(|ext| ext.eq_ignore_ascii_case("env"))
	}));
	let (_, readme) = files
		.iter()
		.find(|(path, _)| path.ends_with("/README.md"))
		.unwrap();
	assert!(
		readme.contains("systemctl --user daemon-reload"),
		"{readme}"
	);
	assert!(readme.contains("Traefik routing"), "{readme}");
	assert!(readme.contains("Backups"), "{readme}");
	assert_eq!(w.exports_audited().await, 1);
}

#[db_test]
async fn an_export_with_secrets_carries_the_env_file_and_is_audited(db: PgPool) {
	let w = World::new(db).await;
	let (s, files) = w.export("?secrets=true").await;
	assert_eq!(s, StatusCode::OK);
	let (_, env) = files
		.iter()
		.find(|(path, _)| path.ends_with(&format!("/units/podkit-app-{}.env", w.application)))
		.unwrap_or_else(|| panic!("no env file in {files:?}"));
	assert!(env.contains(&format!("API_KEY={SECRET}")), "{env}");
	assert_eq!(w.exports_audited().await, 1);
	// The audit event says secrets went out, without any value.
	let diff: Value = sqlx::query_scalar(
		"SELECT diff FROM audit_events WHERE action = 'quadlet.export' ORDER BY id DESC LIMIT 1",
	)
	.fetch_one(&w.db)
	.await
	.unwrap();
	assert_eq!(diff["secrets"], true, "{diff}");
	assert!(!diff.to_string().contains(SECRET));
}

#[db_test]
async fn secrets_need_update_on_env_vars(db: PgPool) {
	let w = World::new(db).await;
	let role = member_role(&w.db, w.team, "alice").await;
	revoke_permission(&w.db, role, "update", "env_var").await;
	let (s, _) = w.export("?secrets=true").await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// Without secrets the export still works.
	let (s, _) = w.export("").await;
	assert_eq!(s, StatusCode::OK);
}

#[db_test]
async fn an_environment_of_another_project_is_refused(db: PgPool) {
	let w = World::new(db).await;
	let (s, _) = w.export("?environment_id=999999").await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn the_team_export_covers_every_project(db: PgPool) {
	let w = World::new(db).await;
	let req = Request::builder()
		.method("POST")
		.uri(format!("/api/v1/teams/{}/quadlet/export", w.team))
		.header(header::AUTHORIZATION, format!("Bearer {}", w.token))
		.body(Body::empty())
		.unwrap();
	let resp = w.app.clone().oneshot(req).await.unwrap();
	assert_eq!(resp.status(), StatusCode::OK);
	assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/gzip");
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	let files = unpack(&bytes);
	assert!(
		files
			.iter()
			.any(|(path, _)| path.ends_with(&format!("podkit-app-{}.container", w.application))),
		"{files:?}"
	);
}
