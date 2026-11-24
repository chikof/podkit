//! Shared variables: precedence, references, the resolver in every spec builder, secrecy,
//! affected resources and tenancy. Names start with `shared_env` so
//! `cargo test -p server -- shared_env` runs them all.

use std::collections::BTreeMap;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::service::{self, AffectedKind};
use super::{Scope, list_scope, resolve};
use crate::applications::Application;
use crate::error::ServerError;
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::grid::env_id;
use crate::tests::{
	PgPool, application_id, count, db_test, local_server_id, member_role, project_id,
	revoke_permission, state, user_id, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
};
use crate::{deployments, env_vars, scope, stacks};

/// A value that must never come back out, whatever the page.
const SECRET: &str = "S3CR3T-7d1e0c52-3b0f-4c53-8a0f-1f5a6b7c8d9e";

struct Fixture {
	state: AppState,
	app: Router,
	cookie: String,
	team: i64,
	project: i64,
	server: i64,
	user: UserId,
	db: PgPool,
}

impl Fixture {
	/// Alice, her team, a project `shop` (default environment `production`) and a second
	/// environment `staging`.
	async fn new(db: &PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (cookie, team) = ui_session(&app, "alice").await;
		let mut f = Self {
			state,
			app,
			cookie,
			team,
			project: 0,
			server: local_server_id(db, team).await,
			user: UserId(user_id(db, "alice").await),
			db: db.clone(),
		};
		f.post(
			&format!("/teams/{team}/projects"),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		f.project = project_id(db, "shop").await;
		f.post(
			&format!("/teams/{team}/projects/{}/environments", f.project),
			&json!({ "newName": "Staging", "newSlug": "staging" }),
		)
		.await;
		f
	}

	async fn post(&self, uri: &str, signals: &Value) -> String {
		let (_, _, body) = post_signals(&self.app, uri, Some(&self.cookie), signals).await;
		body
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	fn team_id(&self) -> TeamId {
		TeamId(self.team)
	}

	fn project_id(&self) -> ProjectId {
		ProjectId(self.project)
	}

	async fn env(&self, slug: &str) -> EnvironmentId {
		EnvironmentId(env_id(&self.db, self.project, slug).await)
	}

	fn team_scope(&self) -> Scope {
		Scope::Team(self.team_id())
	}

	fn project_scope(&self) -> Scope {
		Scope::Project(self.team_id(), self.project_id())
	}

	async fn env_scope(&self, slug: &str) -> Scope {
		Scope::Environment(self.team_id(), self.project_id(), self.env(slug).await)
	}

	async fn set(&self, scope: Scope, key: &str, value: &str) {
		service::set(&self.state, self.user, scope, key, value, true)
			.await
			.unwrap();
	}

	/// An image application `slug` in `environment` (the default when empty).
	async fn application(&self, slug: &str, environment: &str) -> Application {
		let environment = if environment.is_empty() {
			String::new()
		} else {
			self.env(environment).await.to_string()
		};
		self.post(
			&format!("/teams/{}/projects/{}/apps", self.team, self.project),
			&json!({
				"name": slug, "slug": slug, "server_id": self.server.to_string(),
				"source": "image", "environment": environment, "image": "traefik/whoami:v1.10",
				"registry_username": "", "registry_password": "", "container_port": "80",
				"memory_limit_mb": "", "cpu_limit": "",
			}),
		)
		.await;
		let id = application_id(&self.db, slug).await;
		scope::application(
			&self.db,
			self.team_id(),
			self.project_id(),
			ApplicationId(id),
		)
		.await
		.unwrap()
	}

	async fn own(&self, app: &Application, key: &str, value: &str) {
		env_vars::service::upsert(
			&self.state,
			self.user,
			self.team_id(),
			self.project_id(),
			app.id,
			key,
			value,
		)
		.await
		.unwrap();
	}

	/// What a deploy of `app` would run with.
	async fn deployed_env(&self, app: &Application) -> BTreeMap<String, String> {
		deployments::run::load_env(&self.state, app)
			.await
			.unwrap_or_else(|e| panic!("load_env: {e:?}"))
			.into_iter()
			.collect()
	}
}

#[db_test]
async fn shared_env_precedence_runs_team_project_environment_app(db: PgPool) {
	let f = Fixture::new(&db).await;
	let app = f.application("web", "staging").await;
	let other = f.application("batch", "").await;

	for key in ["A", "B", "C", "D"] {
		f.set(f.team_scope(), key, "team").await;
	}
	for key in ["B", "C", "D"] {
		f.set(f.project_scope(), key, "project").await;
	}
	for key in ["C", "D"] {
		f.set(f.env_scope("staging").await, key, "environment")
			.await;
	}
	// Another environment's variable never leaks into this one.
	f.set(f.env_scope("production").await, "ONLY_PROD", "x")
		.await;
	f.own(&app, "D", "app").await;

	let env = f.deployed_env(&app).await;
	assert_eq!(env["A"], "team");
	assert_eq!(env["B"], "project");
	assert_eq!(env["C"], "environment");
	assert_eq!(env["D"], "app");
	assert!(!env.contains_key("ONLY_PROD"), "{env:?}");

	// The application in the other environment sees project, not staging, values.
	let env = f.deployed_env(&other).await;
	assert_eq!(env["C"], "project");
	assert_eq!(env["ONLY_PROD"], "x");

	// Each variable is listed once, from the level that wins.
	let rows = resolve::shared(
		&db,
		&f.state.secrets,
		f.team_id(),
		f.project_id(),
		app.environment_id,
	)
	.await
	.unwrap();
	let levels: Vec<_> = rows
		.iter()
		.map(|r| (r.key.as_str(), r.level.label()))
		.collect();
	assert_eq!(
		levels,
		[
			("A", "team"),
			("B", "project"),
			("C", "environment"),
			("D", "environment")
		]
	);
}

#[db_test]
async fn shared_env_setting_a_name_again_replaces_it_at_that_scope_only(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.set(f.team_scope(), "KEY", "one").await;
	f.set(f.project_scope(), "KEY", "two").await;
	f.set(f.team_scope(), "KEY", "three").await;
	assert_eq!(list_scope(&db, f.team_scope()).await.unwrap().len(), 1);
	assert_eq!(list_scope(&db, f.project_scope()).await.unwrap().len(), 1);
	assert_eq!(count(&db, "shared_env_vars").await, 2);
}

#[db_test]
async fn shared_env_references_are_replaced_in_app_values(db: PgPool) {
	let f = Fixture::new(&db).await;
	let app = f.application("web", "").await;
	f.set(f.team_scope(), "DB_HOST", "db.internal").await;
	f.own(
		&app,
		"DATABASE_URL",
		"postgres://${{shared.DB_HOST}}:5432/shop",
	)
	.await;
	let env = f.deployed_env(&app).await;
	assert_eq!(env["DATABASE_URL"], "postgres://db.internal:5432/shop");
	// The stored value is the reference, not the resolved text.
	let stored = env_vars::list(&db, app.id).await.unwrap();
	assert_eq!(stored.len(), 1);
	assert_eq!(
		f.state.secrets.decrypt(&stored[0].value).unwrap(),
		"postgres://${{shared.DB_HOST}}:5432/shop"
	);
}

#[db_test]
async fn shared_env_a_missing_reference_is_a_clear_validation_error(db: PgPool) {
	let f = Fixture::new(&db).await;
	let app = f.application("web", "").await;

	let err = env_vars::service::upsert(
		&f.state,
		f.user,
		f.team_id(),
		f.project_id(),
		app.id,
		"URL",
		"https://${{shared.NOPE}}/",
	)
	.await
	.unwrap_err();
	let ServerError::Validation(message) = &err else {
		panic!("{err:?}");
	};
	assert!(message.contains("env var URL"), "{message}");
	assert!(message.contains("${{shared.NOPE}}"), "{message}");
	assert_eq!(count(&db, "env_vars").await, 0);

	// The same through the page: the error comes back as a signal, nothing is saved.
	let body = f
		.post(
			&format!(
				"/teams/{}/projects/{}/apps/{}/env",
				f.team, f.project, app.id
			),
			&json!({ "env_key": "URL", "env_value": "${{shared.NOPE}}" }),
		)
		.await;
	assert_contains(&body, "NOPE");
	assert_eq!(count(&db, "env_vars").await, 0);

	// A reference that stops resolving after the save fails the deploy with the same words,
	// and never deploys an empty string.
	f.set(f.team_scope(), "NOPE", "here").await;
	f.own(&app, "URL", "https://${{shared.NOPE}}/").await;
	let id = list_scope(&db, f.team_scope()).await.unwrap()[0].id;
	service::remove(&f.state, f.user, f.team_scope(), id)
		.await
		.unwrap();
	let failure = deployments::run::load_env(&f.state, &app)
		.await
		.unwrap_err();
	let text = format!("{failure:?}");
	assert!(text.contains("${{shared.NOPE}}"), "{text}");
	assert!(text.contains("env var URL"), "{text}");
}

#[db_test]
async fn shared_env_a_shared_value_cannot_hold_a_reference(db: PgPool) {
	let f = Fixture::new(&db).await;
	let err = service::set(&f.state, f.user, f.team_scope(), "A", "${{shared.B}}", true)
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::Validation(_)), "{err:?}");
	let err = service::set(&f.state, f.user, f.team_scope(), "1BAD", "x", true)
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::Validation(_)), "{err:?}");
}

#[db_test]
async fn shared_env_every_spec_builder_goes_through_the_resolver(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.set(f.project_scope(), "STRIPE_KEY", "sk_test_123").await;

	// Applications: the deploy spec's env.
	let app = f.application("web", "").await;
	assert_eq!(f.deployed_env(&app).await["STRIPE_KEY"], "sk_test_123");

	// Stacks: the compose file is interpolated with the shared variables under the stack's own.
	let (stack, _) = stacks::service::create(
		&f.state,
		f.user,
		f.team_id(),
		f.project_id(),
		stacks::service::CreateInput {
			name: "billing".to_owned(),
			compose_yaml: "services:\n  api:\n    image: traefik/whoami:v1.10\n    environment:\n      TOKEN: ${STRIPE_KEY}\n      REGION: ${REGION}\n".to_owned(),
			env: BTreeMap::from([("REGION".to_owned(), "eu-${{shared.STRIPE_KEY}}".to_owned())]),
			files: BTreeMap::new(),
			server_id: crate::ids::ServerId(f.server),
			environment_id: None,
			source: None,
			deploy: false,
			registry: None,
		},
	)
	.await
	.unwrap();
	let inputs = stacks::service::effective_env(&f.state, &stack)
		.await
		.unwrap();
	assert_eq!(inputs["STRIPE_KEY"], "sk_test_123");
	assert_eq!(inputs["REGION"], "eu-sk_test_123");
	let files = BTreeMap::new();
	let compose = stacks::compose::parse(
		&stack.compose_yaml,
		&stacks::compose::Inputs {
			env: &inputs,
			files: &files,
		},
	)
	.unwrap();
	assert!(
		compose.missing_vars.is_empty(),
		"{:?}",
		compose.missing_vars
	);
	assert_eq!(compose.services["api"].env["TOKEN"], "sk_test_123");
	// The stack's stored variables stay its own: nothing shared is written into them.
	let own = stacks::decrypt_env(&f.state.secrets, &stack).unwrap();
	assert_eq!(own.len(), 1);

	// Every builder calls the resolver: the app's load_env and the stack's plan. A new
	// builder (cron jobs, previews) must be added here.
	for (name, source) in [
		("deployments/run.rs", include_str!("../deployments/run.rs")),
		("stacks/run.rs", include_str!("../stacks/run.rs")),
	] {
		assert!(
			source.contains("shared_env::resolve::"),
			"{name} does not use the shared variable resolver"
		);
	}
}

#[db_test]
async fn shared_env_values_are_never_rendered(db: PgPool) {
	let f = Fixture::new(&db).await;
	let app = f.application("web", "").await;
	let prod = f.env("production").await;
	let (team, project) = (f.team, f.project);

	let mut seen = Vec::new();
	let team_url = format!("/teams/{team}/settings/shared-variables");
	let project_url = format!("/teams/{team}/projects/{project}/shared-variables");
	let env_url = format!("/teams/{team}/projects/{project}/environments/{prod}/shared-variables");
	for (url, key) in [
		(&team_url, "TEAM_SECRET"),
		(&project_url, "PROJECT_SECRET"),
		(&env_url, "ENV_SECRET"),
	] {
		seen.push(
			f.post(
				url,
				&json!({ "sv_key": key, "sv_value": SECRET, "sv_secret": true }),
			)
			.await,
		);
	}
	// A plain variable's value shows on its own scope's page, and only there.
	seen.push(
		f.post(
			&team_url,
			&json!({ "sv_key": "REGION", "sv_value": "eu-west-1", "sv_secret": false }),
		)
		.await,
	);
	assert_contains(seen.last().unwrap(), "REGION");

	for url in [&team_url, &project_url, &env_url] {
		let (s, body) = f.get(url).await;
		assert_eq!(s, StatusCode::OK, "{url}");
		seen.push(body);
	}
	let (_, own_page) = f.get(&team_url).await;
	assert_contains(&own_page, "eu-west-1");
	assert_contains(&own_page, "Value hidden");

	// The application's page lists the names, where they come from, and no value at all.
	let env_tab = format!("/teams/{team}/projects/{project}/apps/{}/env", app.id);
	let (s, tab) = f.get(&env_tab).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&tab, "Inherited variables");
	for key in ["TEAM_SECRET", "PROJECT_SECRET", "ENV_SECRET", "REGION"] {
		assert_contains(&tab, key);
	}
	assert_not_contains(&tab, "eu-west-1");
	seen.push(tab);
	// Saving a variable of its own on that page does not bring them out either.
	seen.push(
		f.post(&env_tab, &json!({ "env_key": "OWN", "env_value": SECRET }))
			.await,
	);
	for body in &seen {
		assert_not_contains(body, SECRET);
	}

	// At rest it is ciphertext, and the flag is stored.
	for row in list_scope(&db, f.team_scope()).await.unwrap() {
		assert!(
			!String::from_utf8_lossy(&row.value).contains(SECRET),
			"{} stored in clear",
			row.key
		);
	}
	let secret: bool =
		sqlx::query_scalar("SELECT is_secret FROM shared_env_vars WHERE key = 'REGION'")
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(!secret);
}

#[db_test]
async fn shared_env_the_affected_list_skips_what_overrides_the_variable(db: PgPool) {
	let f = Fixture::new(&db).await;
	let plain = f.application("plain", "").await;
	let own = f.application("own", "").await;
	let staged = f.application("staged", "staging").await;
	f.own(&own, "TOKEN", "mine").await;
	f.set(f.team_scope(), "TOKEN", "team").await;
	f.set(f.env_scope("staging").await, "TOKEN", "staging")
		.await;

	let names = |list: Vec<service::Affected>| -> Vec<String> {
		list.into_iter().map(|a| a.name).collect()
	};
	// Team level: not the app with its own TOKEN, not the environment that sets its own.
	let list = service::affected(&db, f.user, f.team_scope(), "TOKEN")
		.await
		.unwrap();
	assert_eq!(names(list), [plain.name.as_str()]);
	// Environment level: just what is in that environment.
	let list = service::affected(&db, f.user, f.env_scope("staging").await, "TOKEN")
		.await
		.unwrap();
	assert_eq!(names(list), [staged.name.as_str()]);
	// A name nothing overrides reaches all three, at team level.
	let list = service::affected(&db, f.user, f.team_scope(), "OTHER")
		.await
		.unwrap();
	assert_eq!(list.len(), 3);
	assert!(list.iter().all(|a| a.kind == AffectedKind::Application));

	// The inherited view says where each comes from and what is overridden.
	let inherited = service::inherited(&f.state, f.user, f.team_id(), f.project_id(), own.id)
		.await
		.unwrap();
	assert_eq!(inherited.len(), 1);
	assert_eq!(inherited[0].key, "TOKEN");
	assert_eq!(inherited[0].level.label(), "team");
	assert!(inherited[0].overridden);
	let inherited = service::inherited(&f.state, f.user, f.team_id(), f.project_id(), staged.id)
		.await
		.unwrap();
	assert_eq!(inherited[0].level.label(), "environment");
	assert!(!inherited[0].overridden);

	// The page lists them after a change, with the button that redeploys them.
	let body = f
		.post(
			&format!("/teams/{}/settings/shared-variables", f.team),
			&json!({ "sv_key": "TOKEN", "sv_value": "v2", "sv_secret": true }),
		)
		.await;
	assert_contains(&body, "plain");
	assert_contains(&body, "Redeploy affected");
	assert_not_contains(&body, ">own<");
}

#[db_test]
async fn shared_env_redeploy_affected_queues_a_deploy_for_each(db: PgPool) {
	let f = Fixture::new(&db).await;
	let a = f.application("one", "").await;
	let b = f.application("two", "staging").await;
	f.application("three", "").await;
	f.own(&a, "KEY", "mine").await;
	f.set(f.team_scope(), "KEY", "v").await;

	assert_eq!(count(&db, "deployments").await, 0);
	let done = service::redeploy_affected(&f.state, f.user, f.team_scope(), "KEY")
		.await
		.unwrap();
	assert_eq!((done.queued, done.skipped), (2, 0));
	assert_eq!(count(&db, "deployments").await, 2);
	let deployed: Vec<i64> =
		sqlx::query_scalar("SELECT application_id FROM deployments ORDER BY application_id")
			.fetch_all(&db)
			.await
			.unwrap();
	assert!(!deployed.contains(&a.id.0), "{deployed:?}");
	assert!(deployed.contains(&b.id.0), "{deployed:?}");
}

#[db_test]
async fn shared_env_another_teams_scope_is_a_404(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.set(f.project_scope(), "KEY", SECRET).await;
	let (_, bob_team) = user_with_team(&f.app, "bob").await;
	let bob_cookie = ui_login(&f.app, "bob").await;
	let bob = UserId(user_id(&db, "bob").await);

	// Bob's own team, alice's project: the project does not exist for him.
	let project_url = format!("/teams/{bob_team}/projects/{}/shared-variables", f.project);
	let (s, _, body) = get_html(&f.app, &project_url, Some(&bob_cookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, SECRET);
	let env = f.env("production").await;
	let (s, _, _) = get_html(
		&f.app,
		&format!(
			"/teams/{bob_team}/projects/{}/environments/{env}/shared-variables",
			f.project
		),
		Some(&bob_cookie),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	// Alice's team, which bob is not in.
	for url in [
		format!("/teams/{}/settings/shared-variables", f.team),
		format!("/teams/{}/projects/{}/shared-variables", f.team, f.project),
	] {
		let (s, _, _) = get_html(&f.app, &url, Some(&bob_cookie)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{url}");
	}

	// Forged writes change nothing.
	let forged = json!({ "sv_key": "EVIL", "sv_value": "x", "sv_secret": true });
	for url in [
		format!("/teams/{bob_team}/projects/{}/shared-variables", f.project),
		format!("/teams/{}/settings/shared-variables", f.team),
	] {
		post_signals(&f.app, &url, Some(&bob_cookie), &forged).await;
	}
	assert_eq!(count(&db, "shared_env_vars").await, 1);

	// The service layer says the same.
	let scope = Scope::Project(TeamId(bob_team), f.project_id());
	assert!(matches!(
		service::list(&f.state, bob, scope).await.unwrap_err(),
		ServerError::ProjectNotFound
	));
	assert!(matches!(
		service::set(&f.state, bob, scope, "EVIL", "x", true)
			.await
			.unwrap_err(),
		ServerError::ProjectNotFound
	));
	let id = list_scope(&db, f.project_scope()).await.unwrap()[0].id;
	assert!(matches!(
		service::remove(&f.state, bob, scope, id).await.unwrap_err(),
		ServerError::ProjectNotFound
	));
	// Removing through the right team but the wrong scope finds nothing either.
	assert!(matches!(
		service::remove(&f.state, f.user, f.team_scope(), id)
			.await
			.unwrap_err(),
		ServerError::EnvVarNotFound
	));
	assert_eq!(count(&db, "shared_env_vars").await, 1);
}

#[db_test]
async fn shared_env_writes_need_the_env_var_permissions(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.set(f.team_scope(), "KEEP", "v").await;
	let role = member_role(&db, f.team, "alice").await;

	revoke_permission(&db, role, "create", "env_var").await;
	let err = service::set(&f.state, f.user, f.team_scope(), "NEW", "v", true)
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	let body = f
		.post(
			&format!("/teams/{}/settings/shared-variables", f.team),
			&json!({ "sv_key": "NEW", "sv_value": "v", "sv_secret": true }),
		)
		.await;
	assert_contains(&body, "permission");
	assert_eq!(count(&db, "shared_env_vars").await, 1);

	revoke_permission(&db, role, "delete", "env_var").await;
	let id = list_scope(&db, f.team_scope()).await.unwrap()[0].id;
	let body = f
		.post(
			&format!("/teams/{}/settings/shared-variables/{id}/delete", f.team),
			&json!({}),
		)
		.await;
	assert_contains(&body, "permission");
	assert_eq!(count(&db, "shared_env_vars").await, 1);

	revoke_permission(&db, role, "read", "env_var").await;
	let (s, _) = f
		.get(&format!("/teams/{}/settings/shared-variables", f.team))
		.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[db_test]
async fn shared_env_pages_escape_names_and_have_one_h1(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.application("web", "").await;
	f.set(f.team_scope(), "KEY", "v").await;
	for url in [
		format!("/teams/{}/settings/shared-variables", f.team),
		format!("/teams/{}/projects/{}/shared-variables", f.team, f.project),
	] {
		let (s, body) = f.get(&url).await;
		assert_eq!(s, StatusCode::OK, "{url}");
		assert_eq!(body.matches("<h1").count(), 1, "{url}");
	}
	// The sidebar's settings list names the page.
	let (_, body) = f.get(&format!("/teams/{}/settings", f.team)).await;
	assert_contains(&body, "Shared variables");
}

// --- F5: every builder of a deploy spec uses the resolver -------------------------------------

/// A running job, as the builders of a spec want one.
fn job(f: &Fixture) -> crate::jobs::Job {
	crate::jobs::Job {
		id: crate::ids::JobId(41),
		kind: crate::jobs::JobKind::DatabaseProvision,
		server_id: crate::ids::ServerId(f.server),
		target_id: 0,
		lock_key: String::new(),
		status: crate::jobs::JobStatus::Running,
		error: None,
	}
}

fn env_map(env: Vec<(String, String)>) -> BTreeMap<String, String> {
	env.into_iter().collect()
}

#[db_test]
async fn followups_shared_env_a_database_gets_the_shared_variables_and_keeps_its_own(db: PgPool) {
	use crate::databases::{self, Credentials, EngineKind, NewDatabase, engines};
	let f = Fixture::new(&db).await;
	f.set(f.project_scope(), "STRIPE_KEY", "sk_test_123").await;
	f.set(f.team_scope(), "TZ", "Europe/Berlin").await;

	let credentials = Credentials::generate();
	let own = engines::spec(EngineKind::Postgres.into()).env(&credentials);
	assert!(!own.is_empty());
	// A shared variable of the same name as one the engine needs must not win.
	f.set(f.env_scope("production").await, &own[0].0, "hijacked")
		.await;

	let environment = f.env("production").await;
	let database = databases::insert(
		&db,
		NewDatabase {
			id: crate::ids::DatabaseId::generate(),
			project_id: f.project_id(),
			environment_id: environment,
			server_id: crate::ids::ServerId(f.server),
			name: "orders".to_owned(),
			engine: EngineKind::Postgres,
			version: "17".to_owned(),
			credentials: f
				.state
				.secrets
				.encrypt(&serde_json::to_string(&credentials).unwrap())
				.unwrap(),
			public_port: None,
			memory_limit_mb: None,
			cpu_limit: None,
		},
	)
	.await
	.unwrap();

	let spec = databases::run::deploy_spec(&f.state, &job(&f), &database)
		.await
		.unwrap_or_else(|e| panic!("deploy_spec: {e:?}"));
	let env = env_map(spec.env);
	assert_eq!(env["STRIPE_KEY"], "sk_test_123");
	assert_eq!(env["TZ"], "Europe/Berlin");
	for (key, value) in &own {
		assert_eq!(&env[key], value, "the engine's own {key} wins");
	}
	assert_ne!(env[&own[0].0], "hijacked");
}

#[db_test]
async fn followups_shared_env_a_model_engine_gets_the_shared_variables_and_keeps_its_own(
	db: PgPool,
) {
	let f = Fixture::new(&db).await;
	let (_, model) =
		crate::tests::seed_model(&db, f.team, f.project, "Llama", "running", Some("c1")).await;
	let model = crate::models::find_by_id(&db, crate::ids::ModelId(model))
		.await
		.unwrap()
		.unwrap();
	let ingress = || proto::Ingress {
		http_port: 80,
		https_port: 443,
		acme_email: None,
	};
	let spec = |f: &Fixture| {
		let (state, model) = (f.state.clone(), model.clone());
		async move {
			crate::models::run::deploy_spec(&state, &model, "files", None, ingress(), 7)
				.await
				.unwrap_or_else(|e| panic!("deploy_spec: {e:?}"))
		}
	};

	let before = env_map(spec(&f).await.env);
	f.set(f.project_scope(), "HF_TOKEN_FOR_CACHE", "hf_abc")
		.await;
	// The engine's own variables, set shared too, still come from the engine.
	for key in before.keys() {
		f.set(f.team_scope(), key, "hijacked").await;
	}
	let after = env_map(spec(&f).await.env);
	assert_eq!(after["HF_TOKEN_FOR_CACHE"], "hf_abc");
	for (key, value) in &before {
		assert_eq!(&after[key], value, "the engine's own {key} wins");
	}
	assert_eq!(after.len(), before.len() + 1);
}

#[db_test]
async fn followups_shared_env_a_standalone_cron_container_gets_the_shared_variables(db: PgPool) {
	use crate::cron::store::{NewTask, insert_task};
	use crate::cron::{Concurrency, Kind};
	let f = Fixture::new(&db).await;
	f.set(f.project_scope(), "STRIPE_KEY", "sk_test_123").await;
	let task = insert_task(
		&db,
		&NewTask {
			project_id: f.project_id(),
			environment_id: f.env("production").await,
			server_id: crate::ids::ServerId(f.server),
			application_id: None,
			name: "sweep".to_owned(),
			kind: Kind::Container,
			image: Some("docker.io/library/busybox:latest".to_owned()),
			command: "env".to_owned(),
			schedule: "0 3 * * *".to_owned(),
			timezone: "UTC".to_owned(),
			timeout_s: 60,
			concurrency: Concurrency::Skip,
			max_retries: 0,
			retry_backoff_s: 30,
			enabled: true,
			next_run_at: None,
		},
	)
	.await
	.unwrap();
	let request = crate::cron::run::build_request(&f.state, &task, crate::ids::CronRunId(1))
		.await
		.unwrap_or_else(|e| panic!("build_request: {e:?}"));
	let proto::CronTarget::Container { env, .. } = request.target else {
		panic!("a container task asks for a container");
	};
	assert_eq!(env_map(env)["STRIPE_KEY"], "sk_test_123");
}

#[test]
fn followups_shared_env_every_builder_of_a_spec_calls_the_resolver() {
	// The ones that make a `DeploySpec` for a user's workload. The Tailscale sidecar's is
	// podkit's own (its auth key and state path), so it has nothing to inherit. Previews and
	// clones deploy through the application builder, which is listed.
	for (name, source) in [
		("deployments/run.rs", include_str!("../deployments/run.rs")),
		("stacks/run.rs", include_str!("../stacks/run.rs")),
		("databases/run.rs", include_str!("../databases/run.rs")),
		("models/run.rs", include_str!("../models/run.rs")),
		("cron/run.rs", include_str!("../cron/run.rs")),
	] {
		assert!(
			source.contains("shared_env::resolve::"),
			"{name} does not use the shared variable resolver"
		);
	}
	// A builder of a `DeploySpec` that is not listed above is a new one: add it, or say why.
	let mut builders = Vec::new();
	for (name, source) in [
		("deployments/run.rs", include_str!("../deployments/run.rs")),
		(
			"stacks/translate.rs",
			include_str!("../stacks/translate.rs"),
		),
		("databases/run.rs", include_str!("../databases/run.rs")),
		("models/run.rs", include_str!("../models/run.rs")),
		("tailscale/spec.rs", include_str!("../tailscale/spec.rs")),
	] {
		if source.contains("DeploySpec {") {
			builders.push(name);
		}
	}
	assert_eq!(
		builders,
		[
			"deployments/run.rs",
			"stacks/translate.rs",
			"databases/run.rs",
			"models/run.rs",
			"tailscale/spec.rs"
		]
	);
}
