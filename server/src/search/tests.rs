//! Search is team-scoped and permission-aware: these are the tests the command palette
//! relies on.

use sqlx::PgPool;

use super::service::search;
use super::{Hit, Kind};
use crate::ids::{TeamId, UserId};
use crate::tests::{app, call, project_id, seed_app_and_database, user_id, user_with_team};

async fn project(app: &axum::Router, token: &str, team: i64, slug: &str) -> i64 {
	let (s, body) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(token),
		Some(serde_json::json!({ "name": slug, "slug": slug })),
	)
	.await;
	assert!(s.is_success(), "create project {slug}: {s} {body}");
	body["id"].as_i64().unwrap()
}

fn names(hits: &[Hit], kind: Kind) -> Vec<&str> {
	hits.iter()
		.filter(|h| h.kind == kind)
		.map(|h| h.name.as_str())
		.collect()
}

#[crate::tests::db_test]
async fn finds_each_kind_in_the_callers_team(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let web = project(&app, &token, team, "web-shop").await;
	seed_app_and_database(&db, team, web).await;
	let alice = UserId(user_id(&db, "alice").await);

	let hits = search(&db, alice, TeamId(team), "web").await.unwrap();
	assert_eq!(names(&hits, Kind::Project), ["web-shop"]);
	// The project's own `production` environment is not named like the query.
	assert!(names(&hits, Kind::Environment).is_empty());

	let hits = search(&db, alice, TeamId(team), "PROD").await.unwrap();
	assert_eq!(names(&hits, Kind::Environment), ["production"]);
	let env = hits.iter().find(|h| h.kind == Kind::Environment).unwrap();
	assert_eq!(env.project.as_ref().unwrap().1, "web-shop");

	let hits = search(&db, alice, TeamId(team), "store").await.unwrap();
	let app_hit = hits.iter().find(|h| h.kind == Kind::Application).unwrap();
	assert_eq!(app_hit.name, "Storefront");
	assert_eq!(app_hit.project.as_ref().unwrap().1, "web-shop");
	assert_eq!(app_hit.environment.as_ref().unwrap().1, "production");

	let hits = search(&db, alice, TeamId(team), "order").await.unwrap();
	assert_eq!(names(&hits, Kind::Database), ["orders"]);

	let hits = search(&db, alice, TeamId(team), "local").await.unwrap();
	assert_eq!(names(&hits, Kind::Server).len(), 1, "{hits:?}");
}

#[crate::tests::db_test]
async fn another_teams_resources_are_never_returned(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice_token, alice_team) = user_with_team(&app, "alice").await;
	let (bob_token, bob_team) = user_with_team(&app, "bob").await;
	let mine = project(&app, &alice_token, alice_team, "shared-name").await;
	let theirs = project(&app, &bob_token, bob_team, "shared-name-bob").await;
	seed_app_and_database(&db, alice_team, mine).await;
	// Bob's resources carry the same words as Alice's.
	let bob_server = crate::tests::server_id(&db, bob_team).await;
	sqlx::query(
		"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port)
		 VALUES (5101, $1, $2, 'Storefront Bob', 'storefront-bob', 'https://example.com/r.git', 3000)",
	)
	.bind(theirs)
	.bind(bob_server)
	.execute(&db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
		 VALUES (6101, $1, $2, 'orders-bob', 'postgres', '17', 'x')",
	)
	.bind(theirs)
	.bind(bob_server)
	.execute(&db)
	.await
	.unwrap();

	let alice = UserId(user_id(&db, "alice").await);
	for q in ["shared", "store", "orders", "prod", "bob", "local"] {
		let hits = search(&db, alice, TeamId(alice_team), q).await.unwrap();
		for hit in &hits {
			assert!(
				!hit.name.to_lowercase().contains("bob"),
				"{q}: another team's {hit:?}"
			);
			if let Some((p, _)) = &hit.project {
				assert_eq!(p.0, mine, "{q}: {hit:?} is not in Alice's project");
			}
		}
	}
	assert_eq!(
		names(
			&search(&db, alice, TeamId(alice_team), "shared")
				.await
				.unwrap(),
			Kind::Project
		),
		["shared-name"]
	);

	// Asking about Bob's team is the same as asking about a team that doesn't exist.
	let err = search(&db, alice, TeamId(bob_team), "shared")
		.await
		.unwrap_err();
	assert!(
		matches!(err, crate::error::ServerError::NotFound),
		"{err:?}"
	);
	let err = search(&db, alice, TeamId(1), "shared").await.unwrap_err();
	assert!(
		matches!(err, crate::error::ServerError::NotFound),
		"{err:?}"
	);
}

#[crate::tests::db_test]
async fn a_blank_query_or_a_bare_wildcard_matches_nothing(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let web = project(&app, &token, team, "web").await;
	seed_app_and_database(&db, team, web).await;
	let alice = UserId(user_id(&db, "alice").await);

	for q in ["", "   ", "%", "_", "\\"] {
		let hits = search(&db, alice, TeamId(team), q).await.unwrap();
		assert!(hits.is_empty(), "{q:?}: {hits:?}");
	}
}

#[crate::tests::db_test]
async fn a_kind_the_role_cannot_read_is_left_out(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let web = project(&app, &token, team, "orders-web").await;
	seed_app_and_database(&db, team, web).await;
	let _ = project_id(&db, "orders-web").await;

	// Carol joins with a role that may read projects and nothing else.
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(serde_json::json!({
			"name": "carol", "email": "carol@example.com", "password": "correct horse battery"
		})),
	)
	.await;
	assert!(s.is_success());
	let carol = user_id(&db, "carol").await;
	crate::tests::team_role(&db, 9001, team, "viewer").await;
	sqlx::query(
		"INSERT INTO role_permissions (role_id, permission_id)
		 SELECT 9001, id FROM permissions WHERE action = 'read' AND resource = 'project'",
	)
	.execute(&db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES (9002, $1, $2, 9001)",
	)
	.bind(team)
	.bind(carol)
	.execute(&db)
	.await
	.unwrap();

	let hits = search(&db, UserId(carol), TeamId(team), "orders")
		.await
		.unwrap();
	assert_eq!(names(&hits, Kind::Project), ["orders-web"]);
	assert!(names(&hits, Kind::Database).is_empty(), "{hits:?}");
	assert!(names(&hits, Kind::Application).is_empty(), "{hits:?}");
	assert!(names(&hits, Kind::Server).is_empty(), "{hits:?}");
}
