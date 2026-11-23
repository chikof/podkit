//! Saved registries: the API, tenancy, write-only passwords, the login test through the
//! agent, and the move of per-application logins into saved registries.
//! `cargo test -p server --all-features -- registries`

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::service;
use crate::tests::{call, count, join_as_project_reader, state, user_id, user_with_team};

const PASSWORD: &str = "hunter2-registry-secret";

fn url(team: i64) -> String {
	format!("/api/v1/teams/{team}/registries")
}

async fn save(
	app: &Router,
	token: &str,
	team: i64,
	name: &str,
	extra: Value,
) -> (StatusCode, Value) {
	let mut body =
		json!({ "name": name, "url": "ghcr.io", "username": "robot", "password": PASSWORD });
	body.as_object_mut()
		.unwrap()
		.extend(extra.as_object().unwrap().clone());
	call(app, "POST", &url(team), Some(token), Some(body)).await
}

#[sqlx::test]
async fn registries_passwords_are_never_returned_and_are_stored_encrypted(db: PgPool) {
	let app = crate::tests::app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let (s, created) = save(
		&app,
		&token,
		team,
		"GitHub",
		json!({ "url": "https://GHCR.io/" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{created}");
	assert_eq!(
		created["url"], "ghcr.io",
		"the address is reduced to a host"
	);
	let (_, listed) = call(&app, "GET", &url(team), Some(&token), None).await;
	for body in [&created, &listed] {
		assert!(!body.to_string().contains(PASSWORD), "{body}");
		assert!(!body.to_string().contains("password"), "{body}");
	}
	let stored: Vec<u8> = sqlx::query_scalar("SELECT password FROM registries")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains(PASSWORD));
	// An update that leaves the password empty keeps it; a duplicate login is a conflict.
	let id = created["id"].as_i64().unwrap();
	let (s, _) = call(
		&app,
		"PATCH",
		&format!("{}/{id}", url(team)),
		Some(&token),
		Some(json!({ "name": "GitHub 2", "username": "robot" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let after: Vec<u8> = sqlx::query_scalar("SELECT password FROM registries")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(stored, after);
	let (s, _) = save(&app, &token, team, "Again", json!({})).await;
	assert_eq!(s, StatusCode::CONFLICT);
	for bad in [
		json!({ "url": "" }),
		json!({ "password": "" }),
		json!({ "name": "" }),
		json!({ "url": "a b" }),
	] {
		let (s, _) = save(&app, &token, team, "x", bad).await;
		assert_eq!(s, StatusCode::BAD_REQUEST);
	}
}

#[sqlx::test]
async fn registries_of_another_team_are_404_and_permissions_apply(db: PgPool) {
	let app = crate::tests::app(db.clone()).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (bob, b_team) = user_with_team(&app, "bob").await;
	let (_, made) = save(&app, &alice, a_team, "Mine", json!({})).await;
	let id = made["id"].as_i64().unwrap();
	for (method, path) in [
		("PATCH", format!("{}/{id}", url(b_team))),
		("DELETE", format!("{}/{id}", url(b_team))),
		("POST", format!("{}/{id}/test", url(b_team))),
	] {
		let (s, _) = call(
			&app,
			method,
			&path,
			Some(&bob),
			Some(json!({ "name": "x", "username": "u" })),
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	let (_, theirs) = call(&app, "GET", &url(b_team), Some(&bob), None).await;
	assert_eq!(theirs.as_array().unwrap().len(), 0);
	// Not a member at all: refused. A project reader may not read or save registries.
	let (s, _) = call(&app, "GET", &url(a_team), Some(&bob), None).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	join_as_project_reader(&db, a_team, user_id(&db, "bob").await, 9100).await;
	let (s, _) = call(&app, "GET", &url(a_team), Some(&bob), None).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = save(&app, &bob, a_team, "Nope", json!({})).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "registries").await, 1);
}

#[sqlx::test]
async fn registries_in_use_cannot_be_deleted_and_apps_must_use_their_own_host(db: PgPool) {
	let app = crate::tests::app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let (_, reg) = save(&app, &token, team, "GitHub", json!({})).await;
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	let project = project["id"].as_i64().unwrap();
	let (_, servers) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		Some(&token),
		None,
	)
	.await;
	let apps = format!("/api/v1/teams/{team}/projects/{project}/applications");
	let body = |image: &str, slug: &str| {
		json!({ "name": slug, "slug": slug, "server_id": servers[0]["id"],
		"image": image, "container_port": 80, "registry_id": reg["id"] })
	};
	// A login is only for the registry it belongs to.
	let (s, err) = call(
		&app,
		"POST",
		&apps,
		Some(&token),
		Some(body("docker.io/library/nginx", "other")),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");
	let (s, created) = call(
		&app,
		"POST",
		&apps,
		Some(&token),
		Some(body("ghcr.io/acme/app:1", "ok")),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{created}");
	assert_eq!(created["registry_id"], reg["id"]);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{}", url(team), reg["id"]),
		Some(&token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT);
	// Another team's registry is a 404 when naming it on an application.
	let (bob, b_team) = user_with_team(&app, "bob").await;
	let (_, foreign) = save(&app, &bob, b_team, "Bobs", json!({})).await;
	let mut b = body("ghcr.io/acme/app:1", "stolen");
	b["registry_id"] = foreign["id"].clone();
	let (s, _) = call(&app, "POST", &apps, Some(&token), Some(b)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn registries_backfill_dedups_by_url_and_username(db: PgPool) {
	let st = state(db.clone());
	let app = crate::routes::routes(st.clone()).await.unwrap();
	let (token, team) = user_with_team(&app, "alice").await;
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	let project = project["id"].as_i64().unwrap();
	let server = crate::tests::server_id(&db, team).await;
	let login = |user: &str| {
		st.secrets
			.encrypt(&json!({ "username": user, "password": PASSWORD }).to_string())
			.unwrap()
	};
	let rows = [
		("a1", "ghcr.io/acme/one:1", "robot"),
		("a2", "ghcr.io/acme/two:1", "robot"),
		("a3", "ghcr.io/acme/three:1", "other"),
		("a4", "registry.example.com:5000/x:1", "robot"),
	];
	for (i, (slug, image, user)) in rows.iter().enumerate() {
		sqlx::query("INSERT INTO applications (id, project_id, server_id, name, slug, source_kind, image, registry_auth, container_port)
			VALUES ($1, $2, $3, $4, $4, 'image', $5, $6, 80)")
			.bind(7000 + i64::try_from(i).unwrap()).bind(project).bind(server).bind(slug).bind(image).bind(login(user))
			.execute(&db).await.unwrap();
	}
	assert_eq!(service::backfill(&st).await.unwrap(), 4);
	assert_eq!(
		count(&db, "registries").await,
		3,
		"one per url and username"
	);
	let shared: i64 = sqlx::query_scalar(
		"SELECT COUNT(DISTINCT registry_id) FROM applications WHERE slug IN ('a1', 'a2')",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(shared, 1);
	let left: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM applications WHERE registry_auth IS NOT NULL OR registry_id IS NULL",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(left, 0, "every login moved and the private copy is gone");
	// The moved password still decrypts, and a second run finds nothing to do.
	let first: i64 = sqlx::query_scalar("SELECT registry_id FROM applications WHERE slug = 'a1'")
		.fetch_one(&db)
		.await
		.unwrap();
	let auth = service::auth_for(&st, crate::ids::RegistryId(first))
		.await
		.unwrap();
	assert_eq!(
		(
			auth.username.as_str(),
			auth.password.as_str(),
			auth.server.as_deref()
		),
		("robot", PASSWORD, Some("ghcr.io"))
	);
	assert_eq!(service::backfill(&st).await.unwrap(), 0);
	assert_eq!(count(&db, "registries").await, 3);
}

#[sqlx::test]
async fn registries_login_test_goes_through_the_agent_and_never_leaks_the_password(db: PgPool) {
	let st = state(db.clone());
	let app = crate::routes::routes(st.clone()).await.unwrap();
	let (token, team) = user_with_team(&app, "alice").await;
	let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
	let recorder = seen.clone();
	let agent = Router::new().route("/v1/registry-login", post(move |body: Bytes| async move {
		let req: Value = serde_json::from_slice(&body).unwrap();
		let ok = req["password"] == PASSWORD;
		recorder.lock().unwrap().push(req);
		axum::Json(json!({ "ok": ok, "message": if ok { "The registry accepted the login." } else { "The registry refused the username or password." } }))
	}));
	let listener = tokio::net::UnixListener::bind(&st.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, agent).await.unwrap() });

	let (_, reg) = save(&app, &token, team, "GitHub", json!({})).await;
	let (s, out) = call(
		&app,
		"POST",
		&format!("{}/{}/test", url(team), reg["id"]),
		Some(&token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{out}");
	assert_eq!(out["ok"], true);
	assert!(!out.to_string().contains(PASSWORD));
	let (_, out) = call(
		&app,
		"POST",
		&format!("{}/test", url(team)),
		Some(&token),
		Some(json!({ "name": "x", "url": "ghcr.io", "username": "robot", "password": "wrong" })),
	)
	.await;
	assert_eq!(out["ok"], false);
	assert_eq!(
		count(&db, "registries").await,
		1,
		"testing a new login stores nothing"
	);
	let seen = seen.lock().unwrap();
	assert_eq!(seen[0]["server"], "ghcr.io");
	assert_eq!(seen[0]["username"], "robot");
	assert_eq!(seen[1]["password"], "wrong");
}
