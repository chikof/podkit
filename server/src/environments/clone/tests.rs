//! Environment cloning: the plan for a fixture project that has every kind of resource,
//! the rewriting of names, slugs and domains, what is left behind, secrets-blank mode,
//! permissions, tenancy, atomicity and the report of a partial failure. Names start with
//! `clone_env` so `cargo test -p server -- clone_env` runs them all.

use std::collections::BTreeMap;

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::plan::{self, Plan, suffixed};
use super::service::{self, Input};
use super::{CloneStatus, Kind, Options, Secrets, StepStatus, run};
use crate::error::ServerError;
use crate::ids::{
	ApplicationId, BackupTargetId, EnvironmentId, JobId, ModelId, ProjectId, ServerId, StackId,
	TeamId, UserId,
};
use crate::jobs::{Job, JobFailure, JobKind, JobStatus};
use crate::shared_env::{self, Scope};
use crate::stacks::{self, NewStack};
use crate::state::AppState;
use crate::tests::{
	PgPool, app, call, count, db_test, join_as_project_reader, member_role, revoke_permission,
	seed_model, server_id, state, user_id, user_with_team,
};
use crate::{domains, env_vars, links, volumes};

mod live;

/// A value that must never be copied when secrets are blanked.
const SECRET: &str = "S3CR3T-0a9d6c52-9b0f-4c53-8a0f-1f5a6b7c8d9e";

struct Fx {
	state: AppState,
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	user: UserId,
	server: i64,
	source: EnvironmentId,
}

fn input(name: &str, slug: &str) -> Input {
	Input {
		name: name.to_owned(),
		slug: slug.to_owned(),
		options: Options::default(),
	}
}

fn blank(name: &str, slug: &str) -> Input {
	Input {
		options: Options {
			secrets: Secrets::Blank,
			..Options::default()
		},
		..input(name, slug)
	}
}

impl Fx {
	/// Alice with a project `web` whose `production` environment is the source, still empty.
	async fn empty(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (status, body) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(status.is_success(), "{status} {body}");
		let project = body["id"].as_i64().unwrap();
		let source = crate::environments::first(&db, ProjectId(project))
			.await
			.unwrap()
			.unwrap();
		Self {
			server: server_id(&db, team).await,
			user: UserId(user_id(&db, "alice").await),
			state,
			app,
			db,
			token,
			team,
			project,
			source,
		}
	}

	/// An application, a Postgres, a stack with a volume, a model linked to the
	/// application, a shared variable, a custom domain and a user network: every kind.
	async fn new(db: PgPool) -> Self {
		let f = Self::empty(db).await;
		f.seed().await;
		f
	}

	fn enc(&self, value: &str) -> Vec<u8> {
		self.state.secrets.encrypt(value).unwrap()
	}

	fn dec(&self, blob: &[u8]) -> String {
		self.state.secrets.decrypt(blob).unwrap()
	}

	async fn seed(&self) {
		let db = &self.db;
		sqlx::query(
			"INSERT INTO applications (id, project_id, environment_id, server_id, name, slug, repo_url,
				container_port, git_token, webhook_secret, git_hook_id)
			VALUES (5001, $1, $2, $3, 'Storefront', 'storefront', 'https://example.com/r.git', 3000,
				$4, $4, 'hook-1')",
		)
		.bind(self.project)
		.bind(self.source)
		.bind(self.server)
		.bind(self.enc("git-secret"))
		.execute(db)
		.await
		.unwrap();
		let app = ApplicationId(5001);
		env_vars::upsert(db, app, "API_KEY", &self.enc(SECRET))
			.await
			.unwrap();
		env_vars::upsert(db, app, "MODE", &self.enc("production"))
			.await
			.unwrap();
		domains::insert(db, app, "shop.example.com").await.unwrap();
		volumes::insert(db, app, "/data").await.unwrap();

		let credentials = json!({ "username": "podkit", "password": SECRET, "database": "podkit" });
		sqlx::query(
			"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine, version,
				credentials, public_port)
			VALUES (6001, $1, $2, $3, 'orders', 'postgres', '17', $4, 25432)",
		)
		.bind(self.project)
		.bind(self.source)
		.bind(self.server)
		.bind(self.enc(&credentials.to_string()))
		.execute(db)
		.await
		.unwrap();

		let stack_env = BTreeMap::from([("TOKEN".to_owned(), SECRET.to_owned())]);
		stacks::insert(
			db,
			NewStack {
				id: StackId(7001),
				project_id: ProjectId(self.project),
				environment_id: self.source,
				server_id: ServerId(self.server),
				name: "Cache".to_owned(),
				slug: "cache".to_owned(),
				compose_yaml: "services:\n  redis:\n    image: redis:7\n    volumes:\n      - cache:/data\nvolumes:\n  cache: {}\n"
					.to_owned(),
				env: stacks::encrypt_env(&self.state.secrets, &stack_env).unwrap(),
				files: Vec::new(),
				source: "raw".to_owned(),
				registry_id: None,
			},
		)
		.await
		.unwrap();
		let images = BTreeMap::from([("redis".to_owned(), "redis:7".to_owned())]);
		stacks::sync_services(db, StackId(7001), &images)
			.await
			.unwrap();

		let (_, model) =
			seed_model(db, self.team, self.project, "Llama", "running", Some("c1")).await;
		sqlx::query("UPDATE models SET environment_id = $2 WHERE id = $1")
			.bind(model)
			.bind(self.source)
			.execute(db)
			.await
			.unwrap();
		links::add(db, app, ModelId(model), true).await.unwrap();

		let scope = Scope::Environment(TeamId(self.team), ProjectId(self.project), self.source);
		shared_env::upsert(db, scope, "REGION", &self.enc("eu"), false, self.user)
			.await
			.unwrap();
		shared_env::upsert(db, scope, "DB_PASSWORD", &self.enc(SECRET), true, self.user)
			.await
			.unwrap();

		sqlx::query(
			"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name)
			VALUES (9001, $1, $2, 'Backend', 'backend', 'user', 'podkit-user-9001')",
		)
		.bind(self.team)
		.bind(self.server)
		.execute(db)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO resource_networks (network_id, resource_kind, resource_id, aliases)
			VALUES (9001, 'application', 5001, '{api}')",
		)
		.execute(db)
		.await
		.unwrap();
	}

	fn project_id(&self) -> ProjectId {
		ProjectId(self.project)
	}

	fn team_id(&self) -> TeamId {
		TeamId(self.team)
	}

	async fn clone(&self, input: Input) -> Result<super::EnvironmentClone, ServerError> {
		service::clone(
			&self.state,
			self.user,
			self.team_id(),
			self.project_id(),
			self.source,
			input,
		)
		.await
	}

	async fn preview(&self, input: &Input) -> Result<Plan, ServerError> {
		service::preview(
			&self.state,
			self.user,
			self.team_id(),
			self.project_id(),
			self.source,
			input,
		)
		.await
	}

	async fn scalar<T>(&self, sql: &'static str, bind: i64) -> T
	where
		T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
	{
		sqlx::query_scalar(sql)
			.bind(bind)
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	/// The JSON API of Alice's project.
	fn api(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/environments/{rest}",
			self.team, self.project
		)
	}

	/// The application the clone made from the storefront.
	fn cloned_app(c: &super::EnvironmentClone) -> ApplicationId {
		ApplicationId(super::copy::pair_of(&c.pairs.0, Kind::Application, 5001).unwrap())
	}

	async fn backup_target(&self) -> BackupTargetId {
		let id = BackupTargetId(9900);
		sqlx::query(
			"INSERT INTO backup_targets
				(id, team_id, name, endpoint, region, bucket, access_key_id, secret_access_key)
			VALUES ($1, $2, 'garage', 'http://127.0.0.1:3900', 'garage', 'b', 'k', $3)",
		)
		.bind(id)
		.bind(self.team)
		.bind(self.enc("s"))
		.execute(&self.db)
		.await
		.unwrap();
		id
	}
}

// --- the plan ----------------------------------------------------------------------------

#[db_test]
async fn clone_env_plan_lists_every_resource_kind(db: PgPool) {
	let f = Fx::new(db).await;
	let plan = f.preview(&input("Staging", "staging")).await.unwrap();

	let kinds: Vec<Kind> = plan.items.iter().map(|i| i.kind).collect();
	assert_eq!(
		kinds,
		[Kind::Application, Kind::Database, Kind::Stack, Kind::Model]
	);
	assert_eq!(plan.environment_slug, "staging");
	assert_eq!(plan.shared_vars, 2, "the environment's shared variables");
	assert_eq!(plan.links, 1, "the application's link to the model");
	assert_eq!(plan.networks, 1, "the user network");
	assert_eq!(plan.skipped_domains, 1);
	assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
	assert_eq!(
		plan.items[0].notes,
		["1 custom domain not copied", "volumes start empty"]
	);
	assert!(plan.items[1].notes[0].contains("new credentials"));
}

#[db_test]
async fn clone_env_plan_with_data_names_what_is_copied(db: PgPool) {
	let f = Fx::new(db).await;
	let mut i = input("Staging", "staging");
	i.options.data = true;
	let plan = f.preview(&i).await.unwrap();
	assert_eq!(plan.items[0].data, ["volume /data"]);
	assert_eq!(plan.items[1].data, ["dump and restore"]);
	assert_eq!(plan.items[2].data, ["volume cache"]);
	assert!(plan.items[3].data.is_empty(), "models share their files");
}

#[db_test]
async fn clone_env_plan_reports_what_is_taken_and_changes_nothing(db: PgPool) {
	let f = Fx::new(db).await;
	let before = count(&f.db, "environments").await;
	let plan = f
		.preview(&input("Production again", "production"))
		.await
		.unwrap();
	assert!(
		plan.conflicts.iter().any(|c| c.contains("production")),
		"{:?}",
		plan.conflicts
	);
	assert_eq!(count(&f.db, "environments").await, before);
	assert_eq!(count(&f.db, "applications").await, 1);
}

// --- names, slugs, domains ----------------------------------------------------------------

#[test]
fn clone_env_suffix_keeps_the_suffix_and_cuts_the_base() {
	assert_eq!(suffixed("shop", "staging", 60), "shop-staging");
	let long = "a".repeat(60);
	let cut = suffixed(&long, "staging", 60);
	assert_eq!(cut.len(), 60);
	assert!(cut.ends_with("-staging"));
	assert_eq!(suffixed("abc-defg", "xy", 6), "abc-xy");
	assert_eq!(suffixed("abc-defg", "xy", 5), "ab-xy");
	// A cut never leaves a hyphen in front of the suffix.
	assert_eq!(suffixed("ab-cd", "xy", 6), "ab-xy");
	// Nothing left of the base: the slug alone.
	assert_eq!(suffixed("abc", "toolongslug", 8), "toolongs");
}

#[db_test]
async fn clone_env_rewrites_names_slugs_and_default_domains(db: PgPool) {
	let f = Fx::new(db).await;
	let plan = f.preview(&input("Staging", "staging")).await.unwrap();
	assert_eq!(
		plan.items[0].new_slug.as_deref(),
		Some("storefront-staging")
	);
	assert_eq!(
		plan.items[0].url.as_deref(),
		Some("storefront-staging.127-0-0-1.sslip.io"),
		"the default domain follows the slug"
	);
	assert_eq!(plan.items[0].new_name, "Storefront-staging");
	assert_eq!(plan.items[1].new_name, "orders-staging");
	assert_eq!(plan.items[2].new_slug.as_deref(), Some("cache-staging"));
	assert_eq!(plan.items[3].new_name, "Llama-staging");

	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap();
	let slugs: Vec<String> = sqlx::query_scalar(
		"SELECT slug FROM applications WHERE environment_id = $1
		UNION ALL SELECT slug FROM stacks WHERE environment_id = $1
		UNION ALL SELECT slug FROM models WHERE environment_id = $1 ORDER BY 1",
	)
	.bind(env)
	.fetch_all(&f.db)
	.await
	.unwrap();
	assert_eq!(slugs.len(), 3);
	assert!(slugs.iter().all(|s| s.ends_with("-staging")), "{slugs:?}");
	let db_name: String = f
		.scalar(
			"SELECT name FROM databases WHERE environment_id = $1",
			env.0,
		)
		.await;
	assert_eq!(db_name, "orders-staging");
	// The source is untouched.
	let source_slug: String = f
		.scalar("SELECT slug FROM applications WHERE id = $1", 5001)
		.await;
	assert_eq!(source_slug, "storefront");
}

#[db_test]
async fn clone_env_does_not_copy_custom_domains_webhooks_or_public_ports(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap().0;
	let domains: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM custom_domains d JOIN applications a ON a.id = d.application_id
			WHERE a.environment_id = $1",
			env,
		)
		.await;
	assert_eq!(domains, 0, "a hostname belongs to one application");
	assert_eq!(
		count(&f.db, "custom_domains").await,
		1,
		"the source keeps its domain"
	);
	let hooks: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM applications WHERE environment_id = $1
				AND (webhook_secret IS NOT NULL OR git_hook_id IS NOT NULL)",
			env,
		)
		.await;
	assert_eq!(hooks, 0);
	let ports: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM databases WHERE environment_id = $1 AND public_port IS NOT NULL",
			env,
		)
		.await;
	assert_eq!(ports, 0);
}

// --- what is copied ------------------------------------------------------------------------

#[db_test]
async fn clone_env_copies_configuration_links_networks_and_shared_variables(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap();
	let app = Fx::cloned_app(&clone);

	// Variables as written.
	let vars = env_vars::list(&f.db, app).await.unwrap();
	let keys: Vec<&str> = vars.iter().map(|v| v.key.as_str()).collect();
	assert_eq!(keys, ["API_KEY", "MODE"]);
	assert_eq!(f.dec(&vars[0].value), SECRET);
	// A fresh volume of its own.
	let new_volumes = volumes::list(&f.db, app).await.unwrap();
	let old_volumes = volumes::list(&f.db, ApplicationId(5001)).await.unwrap();
	assert_eq!(new_volumes.len(), 1);
	assert_eq!(new_volumes[0].mount_path, "/data");
	assert_ne!(new_volumes[0].name, old_volumes[0].name);
	// The link goes to the cloned model, as the default.
	let linked = links::list(&f.db, app).await.unwrap();
	assert_eq!(linked.len(), 1);
	assert!(linked[0].is_default);
	assert!(linked[0].slug.ends_with("-staging"), "{}", linked[0].slug);
	// The user network, without the source's aliases (they would answer for both).
	let aliases: Vec<String> = sqlx::query_scalar(
		"SELECT aliases FROM resource_networks WHERE network_id = 9001 AND resource_id = $1",
	)
	.bind(app)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert!(aliases.is_empty(), "{aliases:?}");
	// Shared variables of the environment's own scope.
	let shared =
		shared_env::list_scope(&f.db, Scope::Environment(f.team_id(), f.project_id(), env))
			.await
			.unwrap();
	let keys: Vec<&str> = shared.iter().map(|v| v.key.as_str()).collect();
	assert_eq!(keys, ["DB_PASSWORD", "REGION"]);
	assert_eq!(f.dec(&shared[0].value), SECRET);
	// Stack variables and compose file, and its services for the next deploy.
	let stack = stacks::list_by_environment(&f.db, env)
		.await
		.unwrap()
		.remove(0);
	assert_eq!(
		stacks::decrypt_env(&f.state.secrets, &stack).unwrap()["TOKEN"],
		SECRET
	);
	assert!(stack.compose_yaml.contains("redis:7"));
	let services = stacks::list_services(&f.db, stack.id).await.unwrap();
	assert_eq!(services.len(), 1);
	assert!(services[0].domain.is_none());
}

#[db_test]
async fn clone_env_gives_the_database_new_credentials(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap();
	let new = crate::databases::list_by_environment(&f.db, env)
		.await
		.unwrap()
		.remove(0);
	let creds = crate::databases::decrypt_credentials(&f.state.secrets, &new).unwrap();
	assert_ne!(
		creds.password, SECRET,
		"a clone never shares the source's password"
	);
	assert_eq!(new.version, "17");
}

#[db_test]
async fn clone_env_models_wait_stopped_unless_deployed(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap();
	let model = crate::models::list_by_environment(&f.db, env)
		.await
		.unwrap()
		.remove(0);
	assert_eq!(model.status, crate::models::ModelStatus::Stopped);
}

// --- secrets blank -------------------------------------------------------------------------

#[db_test]
async fn clone_env_secrets_blank_keeps_names_and_drops_values(db: PgPool) {
	let f = Fx::new(db).await;
	let plan = f.preview(&blank("Staging", "staging")).await.unwrap();
	// Two application variables, one stack variable and the secret shared variable.
	assert_eq!(plan.blanked, 4);
	assert!(!serde_json::to_string(&plan).unwrap().contains(SECRET));

	let clone = f.clone(blank("Staging", "staging")).await.unwrap();
	let env = clone.environment_id.unwrap();
	let app = Fx::cloned_app(&clone);

	let vars = env_vars::list(&f.db, app).await.unwrap();
	assert_eq!(vars.len(), 2, "the names stay");
	assert!(vars.iter().all(|v| f.dec(&v.value).is_empty()));

	let stack = stacks::list_by_environment(&f.db, env)
		.await
		.unwrap()
		.remove(0);
	let stack_env = stacks::decrypt_env(&f.state.secrets, &stack).unwrap();
	assert_eq!(stack_env.keys().collect::<Vec<_>>(), ["TOKEN"]);
	assert_eq!(stack_env["TOKEN"], "");

	let shared =
		shared_env::list_scope(&f.db, Scope::Environment(f.team_id(), f.project_id(), env))
			.await
			.unwrap();
	let by_key = |k: &str| shared.iter().find(|v| v.key == k).unwrap();
	assert_eq!(
		f.dec(&by_key("DB_PASSWORD").value),
		"",
		"a secret is emptied"
	);
	assert_eq!(
		f.dec(&by_key("REGION").value),
		"eu",
		"a plain value is kept"
	);

	// Stored logins to the git host are not copied either.
	let creds: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM applications WHERE environment_id = $1 AND git_token IS NOT NULL",
			env.0,
		)
		.await;
	assert_eq!(creds, 0);
	let source_creds: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM applications WHERE id = $1 AND git_token IS NOT NULL",
			5001,
		)
		.await;
	assert_eq!(source_creds, 1, "the source keeps its login");
}

#[db_test]
async fn clone_env_copy_mode_keeps_the_git_login(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let creds: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM applications WHERE environment_id = $1 AND git_token IS NOT NULL",
			clone.environment_id.unwrap().0,
		)
		.await;
	assert_eq!(creds, 1);
}

// --- the clone as a job ------------------------------------------------------------------------

#[db_test]
async fn clone_env_queues_a_job_with_the_steps_pending(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	assert_eq!(clone.status, CloneStatus::Queued);
	let labels: Vec<&str> = clone.report.0.iter().map(|s| s.label.as_str()).collect();
	assert_eq!(labels, ["Start database orders-staging"]);
	assert!(
		clone
			.report
			.0
			.iter()
			.all(|s| s.status == StepStatus::Pending)
	);
	let job: (String, i64, String) =
		sqlx::query_as("SELECT kind, target_id, lock_key FROM jobs WHERE id = $1")
			.bind(clone.job_id.unwrap())
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert_eq!(job.0, "environment_clone");
	assert_eq!(job.1, clone.id.0);
	assert!(job.2.starts_with("env-clone:"));
}

#[db_test]
async fn clone_env_with_deploy_adds_the_deploy_steps(db: PgPool) {
	let f = Fx::new(db).await;
	let mut i = input("Staging", "staging");
	i.options.deploy = true;
	let clone = f.clone(i).await.unwrap();
	let labels: Vec<&str> = clone.report.0.iter().map(|s| s.label.as_str()).collect();
	assert_eq!(
		labels,
		[
			"Start database orders-staging",
			"Deploy Storefront-staging",
			"Deploy stack Cache-staging",
			"Start model Llama-staging",
		]
	);
	let model = crate::models::list_by_environment(&f.db, clone.environment_id.unwrap())
		.await
		.unwrap()
		.remove(0);
	assert_eq!(model.status, crate::models::ModelStatus::Starting);
}

#[db_test]
async fn clone_env_with_data_adds_the_copy_steps_after_the_databases_start(db: PgPool) {
	let f = Fx::new(db).await;
	let mut i = input("Staging", "staging");
	i.options.data = true;
	i.options.backup_target_id = Some(f.backup_target().await);
	let clone = f.clone(i).await.unwrap();
	let labels: Vec<&str> = clone.report.0.iter().map(|s| s.label.as_str()).collect();
	assert_eq!(
		labels,
		[
			"Start database orders-staging",
			"Copy the data of orders-staging",
			"Copy volume /data of Storefront-staging",
			"Copy volume cache of Cache-staging",
		]
	);
}

#[db_test]
async fn clone_env_data_needs_a_destination_of_the_team(db: PgPool) {
	let f = Fx::new(db).await;
	let mut i = input("Staging", "staging");
	i.options.data = true;
	let err = f.clone(i.clone()).await.unwrap_err();
	assert!(matches!(&err, ServerError::Validation(m) if m.contains("backup destination")));

	// Someone else's destination is not found.
	let other = app(f.db.clone()).await;
	let (_, bob_team) = user_with_team(&other, "bob").await;
	sqlx::query(
		"INSERT INTO backup_targets
			(id, team_id, name, endpoint, region, bucket, access_key_id, secret_access_key)
		VALUES (9901, $1, 'theirs', 'http://x', 'r', 'b', 'k', $2)",
	)
	.bind(bob_team)
	.bind(f.enc("s"))
	.execute(&f.db)
	.await
	.unwrap();
	i.options.backup_target_id = Some(BackupTargetId(9901));
	let before = count(&f.db, "environments").await;
	let err = f.clone(i).await.unwrap_err();
	assert!(matches!(err, ServerError::BackupTargetNotFound), "{err:?}");
	assert_eq!(count(&f.db, "environments").await, before);
}

#[db_test]
async fn clone_env_of_an_empty_environment_makes_only_the_environment(db: PgPool) {
	let f = Fx::empty(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	assert_eq!(clone.status, CloneStatus::Succeeded);
	assert!(clone.job_id.is_none());
	assert!(clone.report.0.is_empty());
	assert!(clone.environment_id.is_some());
}

// --- atomicity and conflicts ------------------------------------------------------------------

#[db_test]
async fn clone_env_refuses_a_taken_name_and_leaves_nothing_behind(db: PgPool) {
	let f = Fx::new(db).await;
	// A database of the project already has the name the clone would give.
	sqlx::query(
		"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine, version, credentials)
		VALUES (6002, $1, $2, $3, 'orders-staging', 'redis', '7', 'x')",
	)
	.bind(f.project)
	.bind(f.source)
	.bind(f.server)
	.execute(&f.db)
	.await
	.unwrap();
	let environments = count(&f.db, "environments").await;
	let apps = count(&f.db, "applications").await;
	let err = f.clone(input("Staging", "staging")).await.unwrap_err();
	assert!(
		matches!(&err, ServerError::Conflict(m) if m.contains("orders-staging")),
		"{err:?}"
	);
	assert_eq!(count(&f.db, "environments").await, environments);
	assert_eq!(count(&f.db, "applications").await, apps);
	assert_eq!(count(&f.db, "environment_clones").await, 0);
}

#[db_test]
async fn clone_env_twice_with_the_same_slug_is_a_conflict(db: PgPool) {
	let f = Fx::new(db).await;
	f.clone(input("Staging", "staging")).await.unwrap();
	let err = f.clone(input("Staging", "staging")).await.unwrap_err();
	assert!(matches!(err, ServerError::Conflict(_)), "{err:?}");
}

#[db_test]
async fn clone_env_rejects_a_bad_slug(db: PgPool) {
	let f = Fx::new(db).await;
	for slug in ["", "Bad Slug", "apps"] {
		let err = f.clone(input("Staging", slug)).await.unwrap_err();
		assert!(matches!(err, ServerError::Validation(_)), "{slug}: {err:?}");
	}
}

// --- permissions and tenancy ------------------------------------------------------------------------

#[db_test]
async fn clone_env_needs_create_on_every_kind_it_copies(db: PgPool) {
	let f = Fx::new(db).await;
	let role = member_role(&f.db, f.team, "alice").await;
	revoke_permission(&f.db, role, "create", "database").await;
	let before = count(&f.db, "environments").await;
	let err = f.clone(input("Staging", "staging")).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	assert_eq!(count(&f.db, "environments").await, before);
}

#[db_test]
async fn clone_env_needs_create_project(db: PgPool) {
	let f = Fx::new(db).await;
	let role = member_role(&f.db, f.team, "alice").await;
	revoke_permission(&f.db, role, "create", "project").await;
	let err = f.clone(input("Staging", "staging")).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
}

#[db_test]
async fn clone_env_data_and_deploy_need_their_own_permissions(db: PgPool) {
	let f = Fx::new(db).await;
	let role = member_role(&f.db, f.team, "alice").await;
	let target = f.backup_target().await;

	revoke_permission(&f.db, role, "create", "backup").await;
	let mut data = input("Staging", "staging");
	data.options.data = true;
	data.options.backup_target_id = Some(target);
	let err = f.clone(data).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");

	revoke_permission(&f.db, role, "create", "deployment").await;
	let mut deploy = input("Staging", "staging");
	deploy.options.deploy = true;
	let err = f.clone(deploy).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	// Without the options the clone is allowed.
	f.clone(input("Staging", "staging")).await.unwrap();
}

#[db_test]
async fn clone_env_a_project_reader_may_preview_but_not_clone(db: PgPool) {
	let f = Fx::new(db).await;
	let _ = user_with_team(&f.app, "bob").await;
	let bob = user_id(&f.db, "bob").await;
	join_as_project_reader(&f.db, f.team, bob, 8800).await;
	let bob = UserId(bob);

	service::preview(
		&f.state,
		bob,
		f.team_id(),
		f.project_id(),
		f.source,
		&input("Staging", "staging"),
	)
	.await
	.unwrap();
	let err = service::clone(
		&f.state,
		bob,
		f.team_id(),
		f.project_id(),
		f.source,
		input("Staging", "staging"),
	)
	.await
	.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
}

#[db_test]
async fn clone_env_forged_api_request_without_permission_is_refused(db: PgPool) {
	let f = Fx::new(db).await;
	let role = member_role(&f.db, f.team, "alice").await;
	revoke_permission(&f.db, role, "create", "application").await;
	let (status, _) = call(
		&f.app,
		"POST",
		&f.api(&format!("{}/clone", f.source)),
		Some(&f.token),
		Some(json!({ "name": "Staging", "slug": "staging" })),
	)
	.await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	assert_eq!(count(&f.db, "environment_clones").await, 0);
}

#[db_test]
async fn clone_env_of_another_teams_environment_is_not_found(db: PgPool) {
	let f = Fx::new(db).await;
	let (bob_token, bob_team) = user_with_team(&f.app, "bob").await;
	let body = json!({ "name": "Staging", "slug": "staging" });
	let environments = count(&f.db, "environments").await;

	// Bob uses his own team in the path with Alice's project and environment.
	for rest in [
		format!("{}/clone", f.source),
		format!("{}/clone/preview", f.source),
	] {
		let (status, _) = call(
			&f.app,
			"POST",
			&format!(
				"/api/v1/teams/{bob_team}/projects/{}/environments/{rest}",
				f.project
			),
			Some(&bob_token),
			Some(body.clone()),
		)
		.await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{rest}");
	}
	assert_eq!(count(&f.db, "environments").await, environments);

	// A clone of Alice's, read through Bob's team, is not found either.
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let (status, _) = call(
		&f.app,
		"GET",
		&format!(
			"/api/v1/teams/{bob_team}/projects/{}/environments/clones/{}",
			f.project, clone.id
		),
		Some(&bob_token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	// And through Alice's team but a project that is not hers.
	let err = service::get(&f.db, f.user, f.team_id(), ProjectId(1), clone.id)
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::ProjectNotFound), "{err:?}");
}

#[db_test]
async fn clone_env_api_round_trip_hides_secrets(db: PgPool) {
	let f = Fx::new(db).await;
	let (status, body) = call(
		&f.app,
		"POST",
		&f.api(&format!("{}/clone", f.source)),
		Some(&f.token),
		Some(json!({ "name": "Staging", "slug": "staging", "secrets": "blank" })),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["status"], "queued");
	assert_eq!(body["options"]["secrets"], "blank");
	assert!(!body.to_string().contains(SECRET));

	let id = body["id"].as_i64().unwrap();
	let (status, shown) = call(
		&f.app,
		"GET",
		&f.api(&format!("clones/{id}")),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(shown["name"], "Staging");
	assert_eq!(shown["report"][0]["status"], "pending");

	let (status, plan) = call(
		&f.app,
		"POST",
		&f.api(&format!("{}/clone/preview", f.source)),
		Some(&f.token),
		Some(json!({ "name": "QA", "slug": "qa" })),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{plan}");
	assert_eq!(plan["items"].as_array().unwrap().len(), 4);
	assert_eq!(plan["items"][0]["new_slug"], "storefront-qa");
}

// --- the job and its report -------------------------------------------------------------------------

fn job_of(clone: &super::EnvironmentClone) -> Job {
	Job {
		id: JobId(clone.job_id.map_or(1, |j| j.0)),
		kind: JobKind::EnvironmentClone,
		server_id: ServerId(1),
		target_id: clone.id.0,
		lock_key: format!("env-clone:{}", clone.id),
		status: JobStatus::Running,
		error: None,
	}
}

async fn run_job(
	f: &Fx,
	clone: &super::EnvironmentClone,
	cancel: &CancellationToken,
) -> Result<(), JobFailure> {
	let job = job_of(clone);
	let mut log = f.state.logs.open(job.id).await.unwrap();
	let result = run::run(&f.state, &job, cancel, &mut log).await;
	log.finish().await;
	result
}

#[test]
fn clone_env_status_of_a_report() {
	use StepStatus::{Failed, Ok, Skipped};
	let step = |status| super::Step {
		label: "x".to_owned(),
		status,
		detail: None,
	};
	assert_eq!(
		run::status_of(&[step(Ok), step(Skipped)]),
		CloneStatus::Succeeded
	);
	assert_eq!(
		run::status_of(&[step(Ok), step(Failed)]),
		CloneStatus::Partial
	);
	assert_eq!(
		run::status_of(&[step(Failed), step(Failed)]),
		CloneStatus::Failed
	);
	assert_eq!(run::status_of(&[]), CloneStatus::Succeeded);
}

#[db_test]
async fn clone_env_partial_failure_keeps_the_environment_and_reports_each_step(db: PgPool) {
	let f = Fx::new(db).await;
	// A second database, so one can start and the other not.
	sqlx::query(
		"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine, version, credentials)
		VALUES (6002, $1, $2, $3, 'sessions', 'redis', '7', 'x')",
	)
	.bind(f.project)
	.bind(f.source)
	.bind(f.server)
	.execute(&f.db)
	.await
	.unwrap();
	let mut i = input("Staging", "staging");
	i.options.deploy = true;
	let clone = f.clone(i).await.unwrap();
	let env = clone.environment_id.unwrap();

	// No agent answers here, so the databases are put where the provision jobs would leave
	// them: one started, one not.
	sqlx::query(
		"UPDATE databases SET status = 'running'
		WHERE environment_id = $1 AND name = 'orders-staging'",
	)
	.bind(env)
	.execute(&f.db)
	.await
	.unwrap();
	sqlx::query(
		"UPDATE databases SET status = 'failed', status_message = 'image pull failed'
		WHERE environment_id = $1 AND name = 'sessions-staging'",
	)
	.bind(env)
	.execute(&f.db)
	.await
	.unwrap();

	let result = run_job(&f, &clone, &CancellationToken::new()).await;
	assert!(
		matches!(&result, Err(JobFailure::Failed(m)) if m.contains("1 of 5 steps failed")),
		"{result:?}"
	);

	let after = super::find_by_id(&f.db, clone.id).await.unwrap().unwrap();
	assert_eq!(after.status, CloneStatus::Partial);
	let by_label = |label: &str| {
		after
			.report
			.0
			.iter()
			.find(|s| s.label == label)
			.unwrap_or_else(|| panic!("no step {label}: {:?}", after.report.0))
	};
	assert_eq!(
		by_label("Start database orders-staging").status,
		StepStatus::Ok
	);
	let failed = by_label("Start database sessions-staging");
	assert_eq!(failed.status, StepStatus::Failed);
	assert_eq!(failed.detail.as_deref(), Some("image pull failed"));
	// What does not depend on the failed database still ran.
	assert_eq!(by_label("Deploy Storefront-staging").status, StepStatus::Ok);
	assert_eq!(
		by_label("Deploy stack Cache-staging").status,
		StepStatus::Ok
	);
	assert_eq!(by_label("Start model Llama-staging").status, StepStatus::Ok);
	assert!(after.finished_at.is_some());

	// The environment is consistent: every resource exists.
	let apps: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM applications WHERE environment_id = $1",
			env.0,
		)
		.await;
	let databases: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM databases WHERE environment_id = $1",
			env.0,
		)
		.await;
	assert_eq!((apps, databases), (1, 2));
	// The deploy was queued.
	let deploys: i64 = f
		.scalar(
			"SELECT COUNT(*) FROM deployments WHERE application_id = $1",
			Fx::cloned_app(&after).0,
		)
		.await;
	assert_eq!(deploys, 1);
}

#[db_test]
async fn clone_env_a_cancelled_job_skips_what_is_left(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	let cancel = CancellationToken::new();
	cancel.cancel();
	let result = run_job(&f, &clone, &cancel).await;
	assert!(matches!(result, Err(JobFailure::Cancelled)));
	let after = super::find_by_id(&f.db, clone.id).await.unwrap().unwrap();
	assert_eq!(after.status, CloneStatus::Failed);
	assert!(
		after
			.report
			.0
			.iter()
			.all(|s| s.status == StepStatus::Skipped)
	);
}

#[db_test]
async fn clone_env_failed_job_marks_the_unfinished_clone(db: PgPool) {
	let f = Fx::new(db).await;
	let clone = f.clone(input("Staging", "staging")).await.unwrap();
	crate::jobs::fail_target(
		&f.state,
		JobKind::EnvironmentClone,
		clone.id.0,
		&JobFailure::Failed("podkit restarted".to_owned()),
	)
	.await;
	let after = super::find_by_id(&f.db, clone.id).await.unwrap().unwrap();
	assert_eq!(after.status, CloneStatus::Failed);
	assert_eq!(after.report.0[0].status, StepStatus::Skipped);
	assert_eq!(
		after.report.0[0].detail.as_deref(),
		Some("podkit restarted")
	);

	// A clone that finished keeps its report.
	super::save_report(&f.db, clone.id, CloneStatus::Partial, &after.report.0)
		.await
		.unwrap();
	crate::jobs::fail_target(
		&f.state,
		JobKind::EnvironmentClone,
		clone.id.0,
		&JobFailure::Failed("late".to_owned()),
	)
	.await;
	let kept = super::find_by_id(&f.db, clone.id).await.unwrap().unwrap();
	assert_eq!(kept.status, CloneStatus::Partial);
}

#[db_test]
async fn clone_env_migration_down_removes_the_table(db: PgPool) {
	let f = Fx::new(db).await;
	f.clone(input("Staging", "staging")).await.unwrap();
	sqlx::raw_sql(include_str!(
		"../../../migrations/0055_environment_clones.down.sql"
	))
	.execute(&f.db)
	.await
	.unwrap();
	let left: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'environment_clones'",
	)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert_eq!(left, 0);
}

#[test]
fn clone_env_plan_of_an_empty_environment_is_just_the_environment() {
	let snap = plan::Snapshot {
		environment: crate::environments::Environment {
			id: EnvironmentId(1),
			project_id: ProjectId(1),
			name: "Production".to_owned(),
			slug: "production".to_owned(),
			position: 0,
			created_at: time::OffsetDateTime::now_utc(),
		},
		servers: std::collections::HashMap::new(),
		apps: Vec::new(),
		databases: Vec::new(),
		stacks: Vec::new(),
		models: Vec::new(),
		shared: Vec::new(),
		env_networks: Vec::new(),
	};
	let plan = Plan::build(&snap, "Staging", "staging", &Options::default());
	assert!(plan.items.is_empty());
	assert_eq!(
		plan.notes,
		["The environment is empty, so only the environment is made."]
	);
}
