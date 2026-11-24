//! Fixtures for the Infrastructure page tests: a team with two environments, resources on
//! them and networks between them. The UI test files hold no SQL, so it lives here.

use super::PgPool;

#[allow(dead_code)]
pub struct Estate {
	pub server: i64,
	pub project: i64,
	pub staging: i64,
	pub storefront: i64,
	pub shared: i64,
}

async fn exec(db: &PgPool, sql: &'static str, args: &[i64]) {
	let mut q = sqlx::query(sql);
	for a in args {
		q = q.bind(*a);
	}
	q.execute(db).await.unwrap();
}

/// Project `Shop <i>x</i>` with `production` and `staging`; an application and a database
/// in production and one application in staging, each on its environment's network, and a
/// shared network joining the two applications. `Storefront` is running, the staging one
/// stopped. Names carry markup on purpose: every page must escape them.
pub async fn seed_estate(db: &PgPool, team: i64) -> Estate {
	let server = super::server_id(db, team).await;
	let (project, staging, storefront, shared) = (9100, 9101, 9110, 9120);
	sqlx::query(
		"INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, 'Shop <i>x</i>', 'shop')",
	)
	.bind(project)
	.bind(team)
	.execute(db)
	.await
	.unwrap();
	exec(
		db,
		"INSERT INTO environments (id, project_id, name, slug, position) VALUES ($1, $2, 'staging', 'staging', 1)",
		&[staging, project],
	)
	.await;
	for (id, name, slug, env) in [
		(storefront, "Storefront", "storefront", project),
		(9111, "<script>alert(1)</script>", "staging-web", staging),
	] {
		sqlx::query(
			"INSERT INTO applications (id, project_id, environment_id, server_id, name, slug, repo_url, container_port)
			 VALUES ($1, $2, $3, $4, $5, $6, 'https://example.com/r.git', 3000)",
		)
		.bind(id)
		.bind(project)
		.bind(env)
		.bind(server)
		.bind(name)
		.bind(slug)
		.execute(db)
		.await
		.unwrap();
	}
	super::insert_deployment(db, storefront, 9150, "running", Some("img"), Some("c1")).await;
	super::insert_deployment(db, 9111, 9151, "stopped", Some("img"), Some("c2")).await;
	sqlx::query(
		"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine, version, credentials, status)
		 VALUES (9130, $1, $1, $2, 'orders', 'postgres', '17', 'x', 'running')",
	)
	.bind(project)
	.bind(server)
	.execute(db)
	.await
	.unwrap();

	for (id, name, env) in [
		(9140, "production net", project),
		(9141, "staging net", staging),
	] {
		sqlx::query(
			"INSERT INTO networks (id, team_id, server_id, name, slug, kind, environment_id, podman_name)
			 VALUES ($1, $2, $3, $4, $5, 'environment', $6, $7)",
		)
		.bind(id)
		.bind(team)
		.bind(server)
		.bind(name)
		.bind(format!("env-{id}"))
		.bind(env)
		.bind(format!("podkit-env-{env}"))
		.execute(db)
		.await
		.unwrap();
	}
	sqlx::query(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name)
		 VALUES ($1, $2, $3, 'backplane', 'backplane', 'user', 'podkit-net-9120')",
	)
	.bind(shared)
	.bind(team)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
	for (network, kind, id) in [
		(9140, "application", storefront),
		(9140, "database", 9130),
		(9141, "application", 9111),
		(shared, "application", storefront),
		(shared, "application", 9111),
	] {
		sqlx::query(
			"INSERT INTO resource_networks (network_id, resource_kind, resource_id) VALUES ($1, $2, $3)",
		)
		.bind(network)
		.bind(kind)
		.bind(id)
		.execute(db)
		.await
		.unwrap();
	}
	Estate {
		server,
		project,
		staging,
		storefront,
		shared,
	}
}

/// A saved registry login of `team`, with placeholder ciphertext.
pub async fn seed_registry(db: &PgPool, team: i64) {
	sqlx::query(
		"INSERT INTO registries (id, team_id, name, url, username, password)
		 VALUES (9200, $1, 'ghcr', 'ghcr.io', 'bot', decode('00', 'hex'))",
	)
	.bind(team)
	.execute(db)
	.await
	.unwrap();
}

/// A connected git provider of `team`, with placeholder credentials.
pub async fn seed_git_provider(db: &PgPool, team: i64) {
	sqlx::query(
		"INSERT INTO git_providers (id, team_id, kind, name, base_url, credentials)
		 VALUES (9210, $1, 'gitea', 'forge', 'https://git.example.com', decode('00', 'hex'))",
	)
	.bind(team)
	.execute(db)
	.await
	.unwrap();
}

/// A finished scan on the team's server, so the vulnerability database has been downloaded.
pub async fn seed_scan_with_database(db: &PgPool, team: i64, server: i64) {
	sqlx::query(
		"INSERT INTO image_scans (id, team_id, server_id, image_ref, status, db_built)
		 VALUES (9220, $1, $2, 'img', 'done', '2026-11-19')",
	)
	.bind(team)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
}

/// Turns scanning on or off for every server of `team`.
pub async fn set_scanning(db: &PgPool, team: i64, enabled: bool) {
	sqlx::query("UPDATE servers SET scan_enabled = $2 WHERE team_id = $1")
		.bind(team)
		.bind(enabled)
		.execute(db)
		.await
		.unwrap();
}

/// What is stored for the team's setup: dismissed, and the skipped parts.
pub async fn stored_setup(db: &PgPool, team: i64) -> Option<(bool, Vec<String>)> {
	sqlx::query_as::<_, (bool, Vec<String>)>(
		"SELECT dismissed_at IS NOT NULL, skipped FROM infrastructure_setup WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await
	.unwrap()
}

/// Makes `user`'s role in `team` able to do `action` on `resource`, besides what it can.
pub async fn grant(db: &PgPool, role: i64, action: &str, resource: &str) {
	sqlx::query(
		"INSERT INTO role_permissions (role_id, permission_id)
		 SELECT $1, id FROM permissions WHERE action = $2 AND resource = $3 ON CONFLICT DO NOTHING",
	)
	.bind(role)
	.bind(action)
	.bind(resource)
	.execute(db)
	.await
	.unwrap();
}

/// Sets the status of every server of `team`: `active`, `pending` or `unreachable`.
pub async fn set_server_status(db: &PgPool, team: i64, status: &str) {
	sqlx::query("UPDATE servers SET status = $2 WHERE team_id = $1")
		.bind(team)
		.bind(status)
		.execute(db)
		.await
		.unwrap();
}
