//! API tokens: scope, team and project limits, expiry, revocation, and the secret's
//! handling. `cargo test -p server --all-features -- api_tokens`

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use super::{Grant, Scope};
use crate::auth::permission::{Resource, granted};
use crate::ids::{ProjectId, TeamId, UserId};
use crate::tests::{app, call, join_as_project_reader, user_id, user_with_team};

fn tokens(team: i64) -> String {
	format!("/api/v1/teams/{team}/api-tokens")
}

/// Makes a token through the API as `session`; returns the whole response.
async fn make(app: &Router, session: &str, team: i64, extra: Value) -> Value {
	let mut body = json!({ "name": "ci", "scopes": ["read"] });
	body.as_object_mut()
		.unwrap()
		.extend(extra.as_object().unwrap().clone());
	let (s, created) = call(app, "POST", &tokens(team), Some(session), Some(body)).await;
	assert_eq!(s, StatusCode::OK, "create token: {created}");
	created
}

async fn project(app: &Router, session: &str, team: i64, slug: &str) -> i64 {
	let (s, p) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(session),
		Some(json!({ "name": slug, "slug": slug })),
	)
	.await;
	assert!(s.is_success(), "create project: {s} {p}");
	p["id"].as_i64().unwrap()
}

fn secret(created: &Value) -> &str {
	created["secret"].as_str().unwrap()
}

#[sqlx::test]
async fn api_tokens_scope_allows_reads_and_refuses_the_rest_with_403(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({ "scopes": ["read"] })).await;
	let t = Some(secret(&token));

	let (s, teams) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert!(teams.is_array());
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);

	// write: refused, and nothing was created
	let (s, body) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	assert!(body["error"].as_str().unwrap().contains("write"), "{body}");
	assert_eq!(crate::tests::count(&db, "projects").await, 0);

	// delete: refused
	let id = project(&app, &session, team, "web").await;
	let (s, body) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/projects/{id}"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	assert_eq!(crate::tests::count(&db, "projects").await, 1);

	// the same call with the scope works
	let writer = make(
		&app,
		&session,
		team,
		json!({ "name": "w", "scopes": ["write", "delete"] }),
	)
	.await;
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(secret(&writer)),
		Some(json!({ "name": "api", "slug": "api" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/projects/{id}"),
		Some(secret(&writer)),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	// write alone does not read
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		Some(secret(&writer)),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn api_tokens_root_does_everything_and_only_root_manages_tokens(db: PgPool) {
	let app = app(db).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let all_but_root = make(
		&app,
		&session,
		team,
		json!({ "scopes": ["read", "read:sensitive", "write", "deploy", "delete"] }),
	)
	.await;
	let t = Some(secret(&all_but_root));
	let (s, _) = call(&app, "GET", &tokens(team), t, None).await;
	assert_eq!(s, StatusCode::FORBIDDEN, "listing tokens needs root");
	let (s, body) = call(
		&app,
		"POST",
		&tokens(team),
		t,
		Some(json!({ "name": "more", "scopes": ["read"] })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/teams",
		t,
		Some(json!({ "name": "x", "slug": "x" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "making teams needs root");

	let root = make(
		&app,
		&session,
		team,
		json!({ "name": "r", "scopes": ["root"] }),
	)
	.await;
	let t = Some(secret(&root));
	let (s, made) = call(
		&app,
		"POST",
		&tokens(team),
		t,
		Some(json!({ "name": "child", "scopes": ["read"] })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{made}");
	let (s, listed) = call(&app, "GET", &tokens(team), t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(listed.as_array().unwrap().len(), 3);
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "p", "slug": "p" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
}

#[sqlx::test]
async fn api_tokens_project_limit_gives_404_on_other_projects(db: PgPool) {
	let app = app(db).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let mine = project(&app, &session, team, "mine").await;
	let other = project(&app, &session, team, "other").await;
	let token = make(
		&app,
		&session,
		team,
		json!({ "scopes": ["read", "write", "delete"], "project_ids": [mine] }),
	)
	.await;
	let t = Some(secret(&token));

	for (method, path) in [
		(
			"GET",
			format!("/api/v1/teams/{team}/projects/{other}/applications"),
		),
		(
			"GET",
			format!("/api/v1/teams/{team}/projects/{other}/databases"),
		),
		(
			"GET",
			format!("/api/v1/teams/{team}/projects/{other}/environments"),
		),
		("DELETE", format!("/api/v1/teams/{team}/projects/{other}")),
	] {
		let (s, body) = call(&app, method, &path, t, None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {body}");
	}
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects/{mine}/applications"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);

	// The project list shows only its own, and a limited token cannot make projects.
	let (s, list) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let ids: Vec<_> = list
		.as_array()
		.unwrap()
		.iter()
		.map(|p| p["id"].as_i64().unwrap())
		.collect();
	assert_eq!(ids, [mine]);
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "new", "slug": "new" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// ...nor reach team-wide resources that are not project lists.
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/backup-targets"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// A project of another team in the limit is a 404 at creation.
	let (bob, bob_team) = user_with_team(&app, "bob").await;
	let foreign = project(&app, &bob, bob_team, "bobs").await;
	let (s, _) = call(
		&app,
		"POST",
		&tokens(team),
		Some(&session),
		Some(json!({ "name": "x", "scopes": ["read"], "project_ids": [foreign] })),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn api_tokens_cross_team_requests_are_404(db: PgPool) {
	let app = app(db).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (_, b_team) = user_with_team(&app, "bob").await;
	let token = make(&app, &alice, a_team, json!({ "scopes": ["root"] })).await;
	let t = Some(secret(&token));
	for path in [
		format!("/api/v1/teams/{b_team}/projects"),
		format!("/api/v1/teams/{b_team}/registries"),
		format!("/api/v1/teams/{b_team}/api-tokens"),
	] {
		let (s, _) = call(&app, "GET", &path, t, None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
	}
	// ...and it lists only its own team.
	let (_, teams) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(teams.as_array().unwrap().len(), 1);
	assert_eq!(teams[0]["id"], a_team);
}

#[sqlx::test]
async fn api_tokens_expired_is_401_and_a_future_expiry_works(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({ "expires_in_days": 30 })).await;
	let t = Some(secret(&token));
	assert!(token["expires_at"].as_i64().is_some());
	let (s, _) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(s, StatusCode::OK);

	sqlx::query("UPDATE api_tokens SET expires_at = NOW() - INTERVAL '1 second'")
		.execute(&db)
		.await
		.unwrap();
	let (s, body) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(s, StatusCode::UNAUTHORIZED, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("expired"),
		"{body}"
	);

	let (s, _) = call(
		&app,
		"POST",
		&tokens(team),
		Some(&session),
		Some(json!({ "name": "past", "scopes": ["read"], "expires_in_days": 0 })),
	)
	.await;
	assert_eq!(
		s,
		StatusCode::BAD_REQUEST,
		"an expiry of zero days is refused"
	);
}

#[sqlx::test]
async fn api_tokens_revocation_takes_effect_immediately(db: PgPool) {
	let app = app(db).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({})).await;
	let t = Some(secret(&token));
	let (s, _) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(s, StatusCode::OK);

	let id = token["id"].as_i64().unwrap();
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{id}", tokens(team)),
		Some(&session),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(s, StatusCode::UNAUTHORIZED);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{id}", tokens(team)),
		Some(&session),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "already gone");
}

#[sqlx::test]
async fn api_tokens_secret_is_shown_once_and_never_stored_in_plain_text(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let created = make(&app, &session, team, json!({})).await;
	let secret = secret(&created).to_owned();
	assert!(secret.starts_with("pk_"), "{secret}");
	let (prefix, rest) = secret[3..].split_once('_').unwrap();
	assert_eq!(prefix, created["prefix"].as_str().unwrap());
	assert_eq!(rest.len(), 64, "32 random bytes, hex");

	// Nowhere in the database, in any column.
	let dump: String = sqlx::query_scalar("SELECT api_tokens::text FROM api_tokens")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!dump.contains(rest), "the secret is stored: {dump}");
	let hash: Vec<u8> = sqlx::query_scalar("SELECT hash FROM api_tokens")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(hash.len(), 32, "a SHA-256, not the secret");
	assert_ne!(hash, rest.as_bytes());

	// Not in any later response.
	let (_, listed) = call(&app, "GET", &tokens(team), Some(&session), None).await;
	assert!(!listed.to_string().contains(rest), "{listed}");
	assert!(listed[0].get("secret").is_none() && listed[0].get("hash").is_none());
	assert_eq!(listed[0]["prefix"], prefix);

	// A wrong secret under the right prefix, and a garbled token, are both 401.
	for bad in [
		format!("pk_{prefix}_{}", "0".repeat(64)),
		format!("pk_{prefix}_short"),
		"pk_nonsense".to_owned(),
	] {
		let (s, _) = call(&app, "GET", "/api/v1/teams", Some(&bad), None).await;
		assert_eq!(s, StatusCode::UNAUTHORIZED, "{bad}");
	}
}

#[sqlx::test]
async fn api_tokens_follow_the_users_rbac_even_with_root(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (bob, _) = user_with_team(&app, "bob").await;
	let bob_id = user_id(&db, "bob").await;
	join_as_project_reader(&db, team, bob_id, 9000).await;

	// Bob is a project reader in alice's team. His token carries `root`, but his role
	// cannot create projects, so neither can the token.
	let token = make(&app, &bob, team, json!({ "scopes": ["root"] })).await;
	let t = Some(secret(&token));
	let (s, list) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{list}");
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "p", "slug": "p" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// ...and what bob's role may not read stays unread.
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// Alice's token in the same team keeps alice's power.
	let a = make(&app, &alice, team, json!({ "scopes": ["write"] })).await;
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(secret(&a)),
		Some(json!({ "name": "p", "slug": "p" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	// A user who left the team takes the token's power with them.
	sqlx::query("DELETE FROM team_members WHERE user_id = $1 AND team_id = $2")
		.bind(bob_id)
		.bind(team)
		.execute(&db)
		.await
		.unwrap();
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn api_tokens_cannot_use_the_ui_or_the_auth_routes(db: PgPool) {
	let app = app(db).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({ "scopes": ["root"] })).await;
	let t = secret(&token);

	for path in [
		"/".to_owned(),
		format!("/teams/{team}/projects"),
		format!("/teams/{team}/settings"),
		format!("/teams/{team}/settings/api-tokens"),
	] {
		let resp = app
			.clone()
			.oneshot(
				Request::builder()
					.uri(&path)
					.header(header::AUTHORIZATION, format!("Bearer {t}"))
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
		assert_ne!(resp.status(), StatusCode::OK, "{path} answered a token");
	}
	let (s, _) = call(&app, "POST", "/api/v1/auth/logout", Some(t), None).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// The token is not a session, so the UI-only cookie path cannot take it either.
	let resp = app
		.clone()
		.oneshot(
			Request::builder()
				.uri(format!("/teams/{team}/projects"))
				.header(header::COOKIE, format!("session={t}"))
				.body(Body::empty())
				.unwrap(),
		)
		.await
		.unwrap();
	assert_ne!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn api_tokens_last_used_is_written_at_most_once_a_minute(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({})).await;
	let t = Some(secret(&token));
	let used = || async {
		sqlx::query_scalar::<_, Option<time::OffsetDateTime>>("SELECT last_used_at FROM api_tokens")
			.fetch_one(&db)
			.await
			.unwrap()
	};
	assert_eq!(used().await, None);
	call(&app, "GET", "/api/v1/teams", t, None).await;
	let first = used().await.expect("first use is recorded");
	call(&app, "GET", "/api/v1/teams", t, None).await;
	assert_eq!(used().await, Some(first), "a second call within a minute");
	sqlx::query("UPDATE api_tokens SET last_used_at = NOW() - INTERVAL '2 minutes'")
		.execute(&db)
		.await
		.unwrap();
	call(&app, "GET", "/api/v1/teams", t, None).await;
	assert!(used().await.unwrap() > time::OffsetDateTime::now_utc() - time::Duration::minutes(1));
}

#[sqlx::test]
async fn api_tokens_creation_is_validated_and_tokens_are_private_to_their_user(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	for body in [
		json!({ "name": "", "scopes": ["read"] }),
		json!({ "name": "x", "scopes": [] }),
		json!({ "name": "x", "scopes": ["admin"] }),
		json!({ "name": "x".repeat(61), "scopes": ["read"] }),
		json!({ "name": "x", "scopes": ["read"], "expires_in_days": 4000 }),
	] {
		let (s, resp) = call(
			&app,
			"POST",
			&tokens(team),
			Some(&alice),
			Some(body.clone()),
		)
		.await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{body}: {resp}");
	}
	assert_eq!(crate::tests::count(&db, "api_tokens").await, 0);

	let made = make(&app, &alice, team, json!({})).await;
	let (s, _) = call(
		&app,
		"POST",
		&tokens(team),
		Some(&alice),
		Some(json!({ "name": "ci", "scopes": ["read"] })),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT, "names are unique per user");

	// Someone outside the team can neither list, make nor revoke.
	let (bob, _) = user_with_team(&app, "bob").await;
	let (s, _) = call(&app, "GET", &tokens(team), Some(&bob), None).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{}", tokens(team), made["id"]),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// A teammate does not see, and cannot revoke, someone else's token.
	let bob_id = user_id(&db, "bob").await;
	join_as_project_reader(&db, team, bob_id, 9001).await;
	let (_, theirs) = call(&app, "GET", &tokens(team), Some(&bob), None).await;
	assert_eq!(theirs.as_array().unwrap().len(), 0);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{}", tokens(team), made["id"]),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_eq!(crate::tests::count(&db, "api_tokens").await, 1);
}

fn grant(scopes: &[Scope], projects: &[i64]) -> Grant {
	Grant {
		token: crate::ids::ApiTokenId(1),
		team: TeamId(7),
		user: UserId(1),
		scopes: scopes.to_vec(),
		projects: projects.iter().copied().map(ProjectId).collect(),
	}
}

#[test]
fn api_tokens_scope_algebra() {
	let read = grant(&[Scope::Read], &[]);
	assert!(read.allows(Scope::Read));
	assert!(!read.allows(Scope::ReadSensitive) && !read.allows(Scope::Write));
	let sensitive = grant(&[Scope::ReadSensitive], &[]);
	assert!(sensitive.allows(Scope::Read) && sensitive.allows(Scope::ReadSensitive));
	assert!(!sensitive.allows(Scope::Write));
	let root = grant(&[Scope::Root], &[]);
	assert!(Scope::ALL.into_iter().all(|s| root.allows(s)));
	let limited = grant(&[Scope::Read], &[3]);
	assert!(limited.sees(ProjectId(3)) && !limited.sees(ProjectId(4)));
	assert!(grant(&[], &[]).sees(ProjectId(4)));
	for scope in Scope::ALL {
		assert_eq!(Scope::parse(scope.as_str()), Some(scope));
	}
	assert_eq!(Scope::parse("admin"), None);
}

#[test]
fn api_tokens_gate_table() {
	use axum::http::Method as M;
	let all = grant(&Scope::ALL, &[]);
	let read = grant(&[Scope::Read], &[]);
	let deployer = grant(&[Scope::Read, Scope::Deploy], &[]);
	let p = "/teams/7/projects/3";
	let app = format!("{p}/applications/9");
	// (grant, method, path, wanted: None = allowed, else the status)
	let cases: Vec<(&Grant, M, String, Option<u16>)> = vec![
		(&read, M::GET, "/teams".into(), None),
		(&read, M::GET, format!("{app}/logs"), None),
		(&read, M::POST, format!("{app}/deployments"), Some(403)),
		(&deployer, M::POST, format!("{app}/deployments"), None),
		(&deployer, M::POST, format!("{app}/restart"), None),
		(&deployer, M::POST, format!("{app}/stop"), None),
		(&deployer, M::POST, format!("{p}/stacks/1/deploy"), None),
		(&deployer, M::POST, format!("{p}/templates/x/deploy"), None),
		(&deployer, M::POST, format!("{p}/databases"), Some(403)),
		(&deployer, M::PATCH, app.clone(), Some(403)),
		(&deployer, M::DELETE, app.clone(), Some(403)),
		(&all, M::DELETE, app, None),
		(&all, M::PUT, format!("{p}/databases/2/settings"), None),
		(&all, M::GET, "/teams/8/projects".into(), Some(404)),
		(&all, M::GET, "/teams/x/projects".into(), Some(404)),
		(&all, M::GET, "/auth/login".into(), Some(403)),
		(&all, M::POST, "/teams".into(), None),
		(&read, M::POST, "/teams/7/members".into(), Some(403)),
		(&all, M::POST, "/teams/7/members".into(), None),
	];
	for (grant, method, path, want) in cases {
		let got = super::gate::check(grant, &method, &path)
			.err()
			.map(|e| e.status().as_u16());
		// `all` has every scope including root, so only the team and auth rules can refuse.
		assert_eq!(got, want, "{method} {path}");
	}
	let limited = grant(&[Scope::Root], &[3]);
	for (method, path, want) in [
		(M::GET, "/teams/7/projects/3/applications", None),
		(M::GET, "/teams/7/projects/4/applications", Some(404)),
		(M::GET, "/teams/7/projects", None),
		(M::POST, "/teams/7/projects", Some(403)),
		(M::GET, "/teams/7/servers", None),
		(M::GET, "/teams/7/backup-targets", Some(403)),
		(M::GET, "/teams", None),
		(M::GET, "/templates", None),
	] {
		let got = super::gate::check(&limited, &method, path)
			.err()
			.map(|e| e.status().as_u16());
		assert_eq!(got, want, "limited {method} {path}");
	}
}

#[sqlx::test]
async fn api_tokens_new_resources_know_the_registry_permission(db: PgPool) {
	// `Resource::Registry` is in the permission set a page loads.
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let alice = user_id(&db, "alice").await;
	let perms = granted(&db, UserId(alice), TeamId(team)).await.unwrap();
	assert!(perms.can(crate::auth::permission::Action::Create, Resource::Registry));
}
