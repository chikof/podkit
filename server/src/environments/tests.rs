//! API, constraint and migration tests for environments.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::tests::{
	app, call, count, seed_app_and_database, seed_model, server_id, user_with_team,
};

const UP: &str = include_str!("../../migrations/0034_environments.up.sql");
const DOWN: &str = include_str!("../../migrations/0034_environments.down.sql");

struct Setup {
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let project = Self::project_in(&app, &token, team, "web").await;
		Self {
			app,
			db,
			token,
			team,
			project,
		}
	}

	async fn project_in(app: &Router, token: &str, team: i64, slug: &str) -> i64 {
		let (s, body) = call(
			app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(token),
			Some(json!({ "name": slug, "slug": slug })),
		)
		.await;
		assert!(s.is_success(), "create project {slug}: {s} {body}");
		body["id"].as_i64().unwrap()
	}

	fn envs(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/environments",
			self.team, self.project
		)
	}

	fn under(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/{rest}",
			self.team, self.project
		)
	}

	async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, path, Some(&self.token), body).await
	}

	async fn environment(&self, slug: &str) -> i64 {
		let (s, body) = self
			.call(
				"POST",
				&self.envs(),
				Some(json!({ "name": slug, "slug": slug })),
			)
			.await;
		assert!(s.is_success(), "create environment {slug}: {s} {body}");
		body["id"].as_i64().unwrap()
	}

	async fn production(&self) -> i64 {
		let (_, list) = self.call("GET", &self.envs(), None).await;
		list.as_array()
			.unwrap()
			.iter()
			.find(|e| e["slug"] == "production")
			.unwrap()["id"]
			.as_i64()
			.unwrap()
	}

	fn application(slug: &str, server: i64, environment: Option<i64>) -> Value {
		let mut body = json!({
			"name": slug,
			"slug": slug,
			"server_id": server,
			"repo_url": "https://github.com/traefik/whoami",
			"container_port": 80,
		});
		if let Some(e) = environment {
			body["environment_id"] = json!(e);
		}
		body
	}
}

/// Adds carol to alice's team as a viewer (role 2) and returns her token.
async fn viewer(s: &Setup) -> String {
	let (carol, _) = user_with_team(&s.app, "carol").await;
	let id = crate::tests::user_id(&s.db, "carol").await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(id)
		.execute(&s.db)
		.await
		.unwrap();
	carol
}

#[sqlx::test]
async fn environments_new_project_has_production(db: PgPool) {
	let s = Setup::new(db).await;
	let (st, list) = s.call("GET", &s.envs(), None).await;
	assert_eq!(st, StatusCode::OK);
	let list = list.as_array().unwrap();
	assert_eq!(list.len(), 1, "{list:?}");
	assert_eq!(list[0]["slug"], "production");
	assert_eq!(list[0]["name"], "production");
	assert_eq!(list[0]["position"], 0);
	assert_eq!(list[0]["project_id"], s.project);
}

#[sqlx::test]
async fn environments_create_rename_reorder_delete(db: PgPool) {
	let s = Setup::new(db).await;
	let production = s.production().await;
	let staging = s.environment("staging").await;
	let dev = s.environment("dev").await;

	let slugs = |list: &Value| -> Vec<String> {
		list.as_array()
			.unwrap()
			.iter()
			.map(|e| e["slug"].as_str().unwrap().to_owned())
			.collect()
	};
	let (_, list) = s.call("GET", &s.envs(), None).await;
	assert_eq!(slugs(&list), ["production", "staging", "dev"]);

	let (st, one) = s
		.call("GET", &format!("{}/{staging}", s.envs()), None)
		.await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(one["slug"], "staging");

	// Rename keeps the slug unless one is given.
	let (st, renamed) = s
		.call(
			"PATCH",
			&format!("{}/{staging}", s.envs()),
			Some(json!({ "name": "Staging" })),
		)
		.await;
	assert_eq!(st, StatusCode::OK, "{renamed}");
	assert_eq!(
		(renamed["name"].as_str(), renamed["slug"].as_str()),
		(Some("Staging"), Some("staging"))
	);
	let (st, renamed) = s
		.call(
			"PATCH",
			&format!("{}/{staging}", s.envs()),
			Some(json!({ "name": "Stage", "slug": "stage" })),
		)
		.await;
	assert_eq!(st, StatusCode::OK, "{renamed}");
	assert_eq!(renamed["slug"], "stage");

	// Reorder: the first becomes the default.
	let (st, list) = s
		.call(
			"PUT",
			&format!("{}/order", s.envs()),
			Some(json!({ "ids": [dev, production, staging] })),
		)
		.await;
	assert_eq!(st, StatusCode::OK, "{list}");
	assert_eq!(slugs(&list), ["dev", "production", "stage"]);
	let (_, list) = s.call("GET", &s.envs(), None).await;
	assert_eq!(slugs(&list), ["dev", "production", "stage"]);

	// A partial or foreign order is refused and changes nothing.
	for ids in [
		json!([dev, production]),
		json!([dev, production, staging, 7]),
	] {
		let (st, _) = s
			.call(
				"PUT",
				&format!("{}/order", s.envs()),
				Some(json!({ "ids": ids })),
			)
			.await;
		assert_eq!(st, StatusCode::BAD_REQUEST, "{ids}");
	}
	let (_, list) = s.call("GET", &s.envs(), None).await;
	assert_eq!(slugs(&list), ["dev", "production", "stage"]);

	let (st, _) = s.call("DELETE", &format!("{}/{dev}", s.envs()), None).await;
	assert_eq!(st, StatusCode::OK);
	let (st, _) = s.call("GET", &format!("{}/{dev}", s.envs()), None).await;
	assert_eq!(st, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn environments_reject_bad_input(db: PgPool) {
	let s = Setup::new(db).await;
	for body in [
		json!({ "name": "", "slug": "x" }),
		json!({ "name": "x", "slug": "Not Valid" }),
		json!({ "name": "x", "slug": "-x" }),
	] {
		let (st, _) = s.call("POST", &s.envs(), Some(body.clone())).await;
		assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
	}
	// The slug is unique within a project, not across projects.
	let (st, _) = s
		.call(
			"POST",
			&s.envs(),
			Some(json!({ "name": "again", "slug": "production" })),
		)
		.await;
	assert_eq!(st, StatusCode::CONFLICT);
	let other = Setup::project_in(&s.app, &s.token, s.team, "api").await;
	let (st, _) = s
		.call(
			"POST",
			&format!("/api/v1/teams/{}/projects/{other}/environments", s.team),
			Some(json!({ "name": "staging", "slug": "staging" })),
		)
		.await;
	assert_eq!(st, StatusCode::OK);
}

#[sqlx::test]
async fn environments_resources_default_to_the_first_environment(db: PgPool) {
	let s = Setup::new(db).await;
	let production = s.production().await;
	let staging = s.environment("staging").await;
	let server = server_id(&s.db, s.team).await;

	// No environment_id: production.
	let (st, web) = s
		.call(
			"POST",
			&s.under("applications"),
			Some(Setup::application("web", server, None)),
		)
		.await;
	assert!(st.is_success(), "{st} {web}");
	assert_eq!(web["environment_id"], production);

	let (st, web_staging) = s
		.call(
			"POST",
			&s.under("applications"),
			Some(Setup::application("web-staging", server, Some(staging))),
		)
		.await;
	assert!(st.is_success(), "{st} {web_staging}");
	assert_eq!(web_staging["environment_id"], staging);

	let (st, db_prod) = s
		.call(
			"POST",
			&s.under("databases"),
			Some(json!({ "name": "main", "engine": "postgres", "server_id": server })),
		)
		.await;
	assert!(st.is_success(), "{st} {db_prod}");
	assert_eq!(db_prod["environment_id"], production);
	let (st, db_staging) = s
		.call(
			"POST",
			&s.under("databases"),
			Some(json!({
				"name": "staging-db", "engine": "postgres", "server_id": server,
				"environment_id": staging,
			})),
		)
		.await;
	assert!(st.is_success(), "{st} {db_staging}");
	assert_eq!(db_staging["environment_id"], staging);

	let ids = |list: &Value| -> Vec<i64> {
		list.as_array()
			.unwrap()
			.iter()
			.map(|v| v["id"].as_i64().unwrap())
			.collect()
	};
	for (kind, prod, stage) in [
		("applications", &web, &web_staging),
		("databases", &db_prod, &db_staging),
	] {
		let (_, default) = s.call("GET", &s.under(kind), None).await;
		assert_eq!(ids(&default), [prod["id"].as_i64().unwrap()], "{kind}");
		let (_, only_staging) = s
			.call(
				"GET",
				&format!("{}?environment_id={staging}", s.under(kind)),
				None,
			)
			.await;
		assert_eq!(
			ids(&only_staging),
			[stage["id"].as_i64().unwrap()],
			"{kind}"
		);
		let (_, only_prod) = s
			.call(
				"GET",
				&format!("{}?environment_id={production}", s.under(kind)),
				None,
			)
			.await;
		assert_eq!(ids(&only_prod), [prod["id"].as_i64().unwrap()], "{kind}");
	}

	// Models list the same way.
	let (_, model) = seed_model(&s.db, s.team, s.project, "m", "stopped", None).await;
	let (_, list) = s.call("GET", &s.under("models"), None).await;
	assert_eq!(ids(&list), [model]);
	assert_eq!(list[0]["environment_id"], production);
	let (_, list) = s
		.call(
			"GET",
			&format!("{}?environment_id={staging}", s.under("models")),
			None,
		)
		.await;
	assert!(list.as_array().unwrap().is_empty());

	// Moving the default changes what an unqualified list shows.
	let (st, _) = s
		.call(
			"PUT",
			&format!("{}/order", s.envs()),
			Some(json!({ "ids": [staging, production] })),
		)
		.await;
	assert_eq!(st, StatusCode::OK);
	let (_, default) = s.call("GET", &s.under("applications"), None).await;
	assert_eq!(ids(&default), [web_staging["id"].as_i64().unwrap()]);

	// The item routes don't need an environment.
	let id = web["id"].as_i64().unwrap();
	let (st, _) = s
		.call("DELETE", &s.under(&format!("applications/{id}")), None)
		.await;
	assert_eq!(st, StatusCode::OK);
}

#[sqlx::test]
async fn environments_other_projects_environment_is_404(db: PgPool) {
	let s = Setup::new(db).await;
	let other = Setup::project_in(&s.app, &s.token, s.team, "api").await;
	let (_, theirs) = s
		.call(
			"GET",
			&format!("/api/v1/teams/{}/projects/{other}/environments", s.team),
			None,
		)
		.await;
	let theirs = theirs[0]["id"].as_i64().unwrap();
	let server = server_id(&s.db, s.team).await;

	let before = (
		count(&s.db, "applications").await,
		count(&s.db, "databases").await,
	);
	let (st, body) = s
		.call(
			"POST",
			&s.under("applications"),
			Some(Setup::application("web", server, Some(theirs))),
		)
		.await;
	assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
	assert_eq!(body["error"], "environment not found");
	let (st, body) = s
		.call(
			"POST",
			&s.under("databases"),
			Some(json!({
				"name": "staging-db", "engine": "postgres", "server_id": server,
				"environment_id": theirs,
			})),
		)
		.await;
	assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
	let (st, body) = s
		.call(
			"POST",
			&s.under("models"),
			Some(json!({
				"name": "m", "model_files_id": 1, "engine": "llama_cpu",
				"environment_id": theirs,
			})),
		)
		.await;
	assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
	assert_eq!(body["error"], "environment not found");
	let (st, _) = s
		.call(
			"GET",
			&format!("{}?environment_id={theirs}", s.under("applications")),
			None,
		)
		.await;
	assert_eq!(st, StatusCode::NOT_FOUND);
	// Reaching it through this project's own environment routes fails too.
	let (st, _) = s.call("GET", &format!("{}/{theirs}", s.envs()), None).await;
	assert_eq!(st, StatusCode::NOT_FOUND);
	assert_eq!(
		before,
		(
			count(&s.db, "applications").await,
			count(&s.db, "databases").await
		)
	);
}

#[sqlx::test]
async fn environments_constraint_refuses_another_projects_environment(db: PgPool) {
	let s = Setup::new(db).await;
	let other = Setup::project_in(&s.app, &s.token, s.team, "api").await;
	let server = server_id(&s.db, s.team).await;
	let production = s.production().await;
	let (files, _) = seed_model(&s.db, s.team, s.project, "seed", "stopped", None).await;

	// The composite foreign key holds without the API in front of it.
	for table in ["applications", "databases", "models"] {
		let sql = match table {
			"applications" => {
				"INSERT INTO applications (id, project_id, environment_id, server_id, name, slug,
					repo_url, container_port) VALUES (1, $1, $2, $3, 'x', 'x', 'u', 80)"
					.to_owned()
			}
			"databases" => {
				"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine,
					version, credentials) VALUES (1, $1, $2, $3, 'x', 'postgres', '17', 'x')"
					.to_owned()
			}
			_ => {
				// The model needs files; the constraint fires either way.
				format!(
					"INSERT INTO models (id, project_id, environment_id, server_id, model_files_id, name,
					slug, engine, image, port, served_name, context_size)
				VALUES (1, $1, $2, $3, {files}, 'x', 'x', 'e', 'i', 1, 's', 1)"
				)
			}
		};
		let err = sqlx::query(sqlx::AssertSqlSafe(sql))
			.bind(other)
			.bind(production)
			.bind(server)
			.execute(&s.db)
			.await
			.expect_err(table);
		let code = err
			.as_database_error()
			.and_then(|d| d.code().map(|c| c.to_string()));
		assert_eq!(code.as_deref(), Some("23503"), "{table}: {err}");
		assert_eq!(
			err.as_database_error().and_then(|d| d.constraint()),
			Some(format!("{table}_environment_fk").as_str()),
			"{table}"
		);
	}
}

#[sqlx::test]
async fn environments_delete_refused_while_resources_remain(db: PgPool) {
	let s = Setup::new(db).await;
	let production = s.production().await;
	let staging = s.environment("staging").await;
	let server = server_id(&s.db, s.team).await;
	let (st, _) = s
		.call(
			"POST",
			&s.under("applications"),
			Some(Setup::application("api", server, Some(staging))),
		)
		.await;
	assert!(st.is_success());
	let (st, _) = s
		.call(
			"POST",
			&s.under("databases"),
			Some(json!({
				"name": "orders", "engine": "postgres", "server_id": server,
				"environment_id": staging,
			})),
		)
		.await;
	assert!(st.is_success());
	let (_, model) = seed_model(&s.db, s.team, s.project, "llama", "stopped", None).await;
	sqlx::query("UPDATE models SET environment_id = $1 WHERE id = $2")
		.bind(staging)
		.bind(model)
		.execute(&s.db)
		.await
		.unwrap();

	let path = format!("{}/{staging}", s.envs());
	let (st, body) = s.call("DELETE", &path, None).await;
	assert_eq!(st, StatusCode::CONFLICT, "{body}");
	let message = body["error"].as_str().unwrap();
	for name in ["application api", "database orders", "model llama"] {
		assert!(message.contains(name), "{message}");
	}
	let (st, _) = s.call("GET", &path, None).await;
	assert_eq!(st, StatusCode::OK, "still there");
	assert_eq!(count(&s.db, "applications").await, 1);

	// Empty it and the delete goes through.
	sqlx::query("DELETE FROM applications")
		.execute(&s.db)
		.await
		.unwrap();
	sqlx::query("DELETE FROM databases")
		.execute(&s.db)
		.await
		.unwrap();
	sqlx::query("DELETE FROM models")
		.execute(&s.db)
		.await
		.unwrap();
	let (st, body) = s.call("DELETE", &path, None).await;
	assert_eq!(st, StatusCode::OK, "{body}");

	// And the last one stays.
	let (st, body) = s
		.call("DELETE", &format!("{}/{production}", s.envs()), None)
		.await;
	assert_eq!(st, StatusCode::CONFLICT, "{body}");
	assert!(body["error"].as_str().unwrap().contains("last"));
	assert_eq!(count(&s.db, "environments").await, 1);
}

#[sqlx::test]
async fn environments_deleting_a_project_removes_them(db: PgPool) {
	let s = Setup::new(db).await;
	s.environment("staging").await;
	let server = server_id(&s.db, s.team).await;
	let (st, _) = s
		.call(
			"POST",
			&s.under("applications"),
			Some(Setup::application("web", server, None)),
		)
		.await;
	assert!(st.is_success());
	let (st, _) = s
		.call(
			"DELETE",
			&format!("/api/v1/teams/{}/projects/{}", s.team, s.project),
			None,
		)
		.await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(count(&s.db, "environments").await, 0);
	assert_eq!(count(&s.db, "applications").await, 0);
}

#[sqlx::test]
async fn environments_other_teams_get_404(db: PgPool) {
	let s = Setup::new(db).await;
	let staging = s.environment("staging").await;
	let (bob, bob_team) = user_with_team(&s.app, "bob").await;
	// Bob names his own team with alice's project and environment ids.
	let envs = format!(
		"/api/v1/teams/{bob_team}/projects/{}/environments",
		s.project
	);
	let path = format!("{envs}/{staging}");
	let calls = [
		("GET", envs.clone(), None),
		(
			"POST",
			envs.clone(),
			Some(json!({ "name": "x", "slug": "x" })),
		),
		("GET", path.clone(), None),
		("PATCH", path.clone(), Some(json!({ "name": "x" }))),
		("DELETE", path.clone(), None),
		(
			"PUT",
			format!("{envs}/order"),
			Some(json!({ "ids": [staging] })),
		),
	];
	for (method, uri, body) in calls {
		let (st, _) = call(&s.app, method, &uri, Some(&bob), body).await;
		assert_eq!(st, StatusCode::NOT_FOUND, "{method} {uri}");
	}
	// Naming alice's team outright is refused, not leaked.
	let (st, _) = call(&s.app, "GET", &s.envs(), Some(&bob), None).await;
	assert_eq!(st, StatusCode::FORBIDDEN);
	assert_eq!(count(&s.db, "environments").await, 2);
}

#[sqlx::test]
async fn environments_members_without_permission_are_refused(db: PgPool) {
	let s = Setup::new(db).await;
	let staging = s.environment("staging").await;
	let carol = viewer(&s).await;
	let path = format!("{}/{staging}", s.envs());

	// A viewer can read.
	for uri in [s.envs(), path.clone()] {
		let (st, _) = call(&s.app, "GET", &uri, Some(&carol), None).await;
		assert_eq!(st, StatusCode::OK, "{uri}");
	}
	// And can't change anything.
	let calls = [
		("POST", s.envs(), Some(json!({ "name": "x", "slug": "x" }))),
		("PATCH", path.clone(), Some(json!({ "name": "x" }))),
		("DELETE", path.clone(), None),
		(
			"PUT",
			format!("{}/order", s.envs()),
			Some(json!({ "ids": [staging] })),
		),
	];
	for (method, uri, body) in calls {
		let (st, _) = call(&s.app, method, &uri, Some(&carol), body).await;
		assert_eq!(st, StatusCode::FORBIDDEN, "{method} {uri}");
	}
	assert_eq!(count(&s.db, "environments").await, 2);
	let name: String = sqlx::query_scalar("SELECT name FROM environments WHERE id = $1")
		.bind(staging)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(name, "staging");
}

/// Every resource of a database made before 0034 ends up in `production`, and
/// `down` then `up` leaves it that way.
#[sqlx::test]
async fn environments_migration_backfills_and_round_trips(db: PgPool) {
	let app = app(db.clone()).await;
	let (_token, team) = user_with_team(&app, "alice").await;
	let server = server_id(&db, team).await;

	// Back to the shape before 0034 (undoing 0059, 0055, 0054, 0052, 0050, 0048, 0040 and 0036, which
	// depend on it, first), then seed it the way it used to be filled.
	for down in [
		include_str!("../../migrations/0059_previews.down.sql"),
		include_str!("../../migrations/0055_environment_clones.down.sql"),
		include_str!("../../migrations/0054_volume_backups.down.sql"),
		include_str!("../../migrations/0052_cron.down.sql"),
		include_str!("../../migrations/0050_shared_env_vars.down.sql"),
		include_str!("../../migrations/0048_notifications.down.sql"),
		include_str!("../../migrations/0040_networks.down.sql"),
		include_str!("../../migrations/0036_stacks.down.sql"),
	] {
		sqlx::raw_sql(down).execute(&db).await.unwrap();
	}
	sqlx::raw_sql(DOWN).execute(&db).await.unwrap();
	for (id, slug) in [(7001_i64, "a"), (7002, "b")] {
		sqlx::query("INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, $3, $3)")
			.bind(id)
			.bind(team)
			.bind(slug)
			.execute(&db)
			.await
			.unwrap();
	}
	seed_app_and_database(&db, team, 7001).await;
	let (_, model) = seed_model(&db, team, 7002, "m", "stopped", None).await;
	assert!(model > 0 && server > 0);

	let check = |db: PgPool| async move {
		let rows: Vec<(String, i64, String)> = sqlx::query_as(
			"SELECT 'applications', COUNT(*), MIN(e.slug) || '/' || MAX(e.slug)
				FROM applications r JOIN environments e
				ON e.id = r.environment_id AND e.project_id = r.project_id
			UNION ALL
			SELECT 'databases', COUNT(*), MIN(e.slug) || '/' || MAX(e.slug)
				FROM databases r JOIN environments e
				ON e.id = r.environment_id AND e.project_id = r.project_id
			UNION ALL
			SELECT 'models', COUNT(*), MIN(e.slug) || '/' || MAX(e.slug)
				FROM models r JOIN environments e
				ON e.id = r.environment_id AND e.project_id = r.project_id",
		)
		.fetch_all(&db)
		.await
		.unwrap();
		assert_eq!(
			rows,
			[
				(
					"applications".to_owned(),
					1,
					"production/production".to_owned()
				),
				(
					"databases".to_owned(),
					1,
					"production/production".to_owned()
				),
				("models".to_owned(), 1, "production/production".to_owned()),
			]
		);
		let envs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments")
			.fetch_one(&db)
			.await
			.unwrap();
		let projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects")
			.fetch_one(&db)
			.await
			.unwrap();
		assert_eq!(envs, projects, "one production per project");
	};

	sqlx::raw_sql(UP).execute(&db).await.unwrap();
	check(db.clone()).await;
	// The columns really are NOT NULL, and nothing was lost.
	let nullable: Vec<String> = sqlx::query_scalar(
		"SELECT table_name FROM information_schema.columns
		 WHERE column_name = 'environment_id' AND is_nullable = 'YES'",
	)
	.fetch_all(&db)
	.await
	.unwrap();
	assert!(nullable.is_empty(), "{nullable:?}");

	sqlx::raw_sql(DOWN).execute(&db).await.unwrap();
	let left: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM information_schema.columns WHERE column_name = 'environment_id'",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(left, 0, "down removes the columns");
	assert_eq!(count(&db, "applications").await, 1, "down keeps the data");
	sqlx::raw_sql(UP).execute(&db).await.unwrap();
	check(db).await;
}
