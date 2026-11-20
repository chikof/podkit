//! Tests of the application pages: the create flows, the header and its actions, and every
//! tab. They run the real router against a database; nothing here touches Podman or the
//! network, so a deployment stays queued and a build log comes from a fake job.

use std::cell::RefCell;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::ids::JobId;
use crate::state::AppState;
use crate::tests::apps::{app_environment, app_source, webhook_secret_blob};
use crate::tests::grid::env_id;
use crate::tests::health;
use crate::tests::{
	PgPool, add_pending_server, application_id, call, count, db_test, deploy_job_id,
	deployment_image, deployment_status, env_var_ciphertext, env_var_id, insert_deployment,
	local_server_id, newest_deployment, only_id, project_id, rename_applications,
	revoke_permission, seed_deployment, set_deployment_error, set_deployment_status, state,
	stored_secrets, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};

const KEY: &str = crate::tests::SSH_KEY;
const TOKEN: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
/// A value that must never come back out, whatever the page.
const SECRET: &str = "S3CR3T-0b9a1c5e-6f0e-4d0e-9a52-7c1d2f3a4b5c";

struct Fixture {
	state: AppState,
	app: Router,
	cookie: String,
	token: String,
	team: i64,
	project: i64,
	server: i64,
	/// The application `with_web` made, else 0.
	application: i64,
	db: PgPool,
	/// Every response body this fixture received.
	seen: RefCell<Vec<String>>,
}

impl Fixture {
	/// Alice, her team, a project `shop` and the local server; no application yet.
	async fn new(db: &PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (cookie, team) = ui_session(&app, "alice").await;
		post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		let project = project_id(db, "shop").await;
		let server = local_server_id(db, team).await;
		let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
		let (_, login) = call(&app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
		Self {
			state,
			app,
			cookie,
			token: login["token"].as_str().unwrap().to_owned(),
			team,
			project,
			server,
			application: 0,
			db: db.clone(),
			seen: RefCell::new(Vec::new()),
		}
	}

	/// As `new`, with a public git application `web`.
	async fn with_web(db: &PgPool) -> Self {
		let mut f = Self::new(db).await;
		f.create("web", json!({})).await;
		f.application = application_id(db, "web").await;
		f
	}

	/// `/teams/{t}/projects/{p}/apps`
	fn base(&self) -> String {
		format!("/teams/{}/projects/{}/apps", self.team, self.project)
	}

	/// The application's own page, and `tab` under it.
	fn url(&self, tab: &str) -> String {
		format!("{}/{}/{tab}", self.base(), self.application)
			.trim_end_matches('/')
			.to_owned()
	}

	fn api(&self, tab: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/{tab}",
			self.team, self.project, self.application
		)
	}

	/// Signals of a valid public-repo create form, with `extra` merged in.
	fn form(&self, slug: &str, extra: Value) -> Value {
		let mut form = json!({
			"name": "Web", "slug": slug, "server_id": self.server.to_string(),
			"source": "git", "environment": "",
			"repo_url": "https://example.com/web.git", "git_ref": "main",
			"deploy_key": "", "git_token": "", "build_strategy": "dockerfile",
			"dockerfile_path": "Dockerfile", "container_port": "3000",
			"memory_limit_mb": "", "cpu_limit": "",
		});
		merge(&mut form, extra);
		form
	}

	/// Signals of a valid image create form, with `extra` merged in.
	fn image_form(&self, slug: &str, extra: Value) -> Value {
		let mut form = json!({
			"name": "Whoami", "slug": slug, "server_id": self.server.to_string(),
			"source": "image", "environment": "", "image": "traefik/whoami:v1.10",
			"registry_username": "", "registry_password": "", "container_port": "80",
			"memory_limit_mb": "", "cpu_limit": "",
		});
		merge(&mut form, extra);
		form
	}

	async fn create(&self, slug: &str, extra: Value) -> String {
		let (s, _, body) = post_signals(
			&self.app,
			&self.base(),
			Some(&self.cookie),
			&self.form(slug, extra),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		self.seen.borrow_mut().push(body.clone());
		body
	}

	async fn create_image(&self, slug: &str, extra: Value) -> String {
		let (s, _, body) = post_signals(
			&self.app,
			&self.base(),
			Some(&self.cookie),
			&self.image_form(slug, extra),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		self.seen.borrow_mut().push(body.clone());
		body
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		self.seen.borrow_mut().push(body.clone());
		(s, body)
	}

	/// A tab of the application, which must load.
	async fn page(&self, tab: &str) -> String {
		let (s, body) = self.get(&self.url(tab)).await;
		assert_eq!(s, StatusCode::OK, "{tab}: {body}");
		body
	}

	async fn post(&self, uri: &str, signals: &Value) -> String {
		let (_, _, body) = post_signals(&self.app, uri, Some(&self.cookie), signals).await;
		self.seen.borrow_mut().push(body.clone());
		body
	}

	async fn post_empty(&self, uri: &str) -> String {
		self.post(uri, &json!({})).await
	}

	async fn set_env(&self, key: &str, value: &str) -> String {
		self.post(
			&self.url("env"),
			&json!({ "env_key": key, "env_value": value }),
		)
		.await
	}

	/// Inserts a deployment of the app straight into `status`.
	async fn seed(&self, id: i64, status: &str, image: Option<&str>, container: Option<&str>) {
		insert_deployment(&self.db, self.application, id, status, image, container).await;
	}

	async fn api_keys(&self) -> Vec<String> {
		let (s, list) = call(
			&self.app,
			"GET",
			&self.api("env-vars"),
			Some(&self.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		list.as_array()
			.unwrap()
			.iter()
			.map(|v| v["key"].as_str().unwrap().to_owned())
			.collect()
	}

	/// Adds `name` to the team with the built-in read-only Member role and signs them in.
	async fn member(&self, name: &str) -> String {
		user_with_team(&self.app, name).await;
		let (s, _) = call(
			&self.app,
			"POST",
			&format!("/api/v1/teams/{}/members", self.team),
			Some(&self.token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
		ui_login(&self.app, name).await
	}
}

fn merge(form: &mut Value, extra: Value) {
	if let Value::Object(map) = extra {
		for (k, v) in map {
			form[k] = v;
		}
	}
}

/// The value of the input that follows `label` in a create response.
fn shown_value(body: &str, label: &str) -> String {
	let after = body
		.split(label)
		.nth(1)
		.unwrap_or_else(|| panic!("no {label}: {body}"));
	let value = after.split("value=\"").nth(1).unwrap();
	value.split('"').next().unwrap().to_owned()
}

// --- create ------------------------------------------------------------------------------------

/// Was `the_create_form_offers_servers_and_shows_fields_by_signal`.
#[db_test]
async fn the_create_form_offers_servers_and_shows_fields_by_signal(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_pending_server(&db, f.team).await;
	let (s, body) = f.get(&format!("{}/new", f.base())).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "New application");
	assert_contains(&body, &format!("value=\"{}\"", f.server));
	assert_contains(&body, "builder (Pending)");
	// The pending server can be seen but not chosen.
	let pending = body.split("value=\"7001\"").nth(1).unwrap();
	assert!(
		pending.split('>').next().unwrap().contains("disabled"),
		"{body}"
	);
	let active = body
		.split(&format!("value=\"{}\"", f.server))
		.nth(1)
		.unwrap();
	assert!(!active.split('>').next().unwrap().contains("disabled"));
	// It is preselected: the first active server.
	assert_contains(
		&body,
		&format!("&quot;server_id&quot;:&quot;{}&quot;", f.server),
	);
	assert_contains(&body, "$build_strategy === 'dockerfile'");
	assert_contains(&body, "$repo_url.startsWith('https://')");
	assert_contains(&body, "!$repo_url.startsWith('https://')");
	assert_contains(&body, "Nixpacks");
	// Both sources are one click apart, and a git form has no image or registry field.
	assert_contains(&body, "Git repository");
	assert_contains(&body, "Docker image");
	assert_contains(&body, "source=image");
	assert_not_contains(&body, "data-bind:registry_username");
}

#[db_test]
async fn the_image_form_asks_for_an_image_and_a_registry_login_not_a_repository(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, body) = f.get(&format!("{}/new?source=image", f.base())).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "data-bind:image");
	assert_contains(&body, "data-bind:registry_username");
	assert_contains(&body, "data-bind:registry_password");
	assert_contains(&body, "&quot;source&quot;:&quot;image&quot;");
	for git_only in [
		"data-bind:repo_url",
		"data-bind:deploy_key",
		"data-bind:build_strategy",
		"Nixpacks",
	] {
		assert_not_contains(&body, git_only);
	}
	// The password field is a password field, and nothing is prefilled.
	assert_contains(&body, "type=\"password\"");
}

/// Was `creating_shows_the_webhook_secret_once_and_never_again`.
#[db_test]
async fn creating_shows_the_webhook_secret_once_and_never_again(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f.create("web", json!({})).await;
	assert_contains(&body, "Application created");
	let url = shown_value(&body, "Webhook URL");
	let id = application_id(&db, "web").await;
	assert_eq!(url, format!("http://podkit.test/webhooks/deploy/{id}"));
	let secret = shown_value(&body, "Webhook secret");
	assert_eq!(secret.len(), 48, "{secret}");
	assert_contains(&body, "Copy");
	assert_contains(&body, "only this once");
	// The secret is not a signal: nothing persisted client-side.
	assert_not_contains(&body.replace(&format!("value=\"{secret}\""), ""), &secret);
	assert_eq!(stored_secrets(&db, "web").await, (false, false, true));

	let app_uri = format!("{}/{id}", f.base());
	for uri in [
		f.base() + "/new",
		app_uri.clone(),
		format!("{app_uri}/settings"),
		format!("{app_uri}/monitoring"),
		format!("{app_uri}/webhooks"),
		format!("{app_uri}/advanced"),
		format!("{app_uri}/deployments"),
		format!("/teams/{}/projects/{}", f.team, f.project),
		format!("/teams/{}/projects", f.team),
	] {
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&page, &secret);
		assert_not_contains(&page, "Webhook secret");
	}
	// The webhook address is on the Webhooks tab, with the instructions.
	let (_, page) = f.get(&format!("{app_uri}/webhooks")).await;
	assert_contains(&page, &url);
	let (_, page) = f.get(&app_uri).await;
	assert_contains(&page, "Web");
	// The application's own page names the ref it follows; the repository itself is on Settings.
	assert_contains(&page, "main");
	assert_contains(&page, "web.127-0-0-1.sslip.io");
	let (_, page) = f.get(&format!("{app_uri}/settings")).await;
	assert_contains(&page, "https://example.com/web.git");
}

/// Was `credentials_are_stored_but_never_rendered`.
#[db_test]
async fn credentials_are_stored_but_never_rendered(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create(
		"keyed",
		json!({ "repo_url": "git@github.com:me/r.git", "deploy_key": KEY }),
	)
	.await;
	f.create("tokened", json!({ "git_token": TOKEN })).await;
	f.create_image(
		"regged",
		json!({ "registry_username": "me", "registry_password": SECRET }),
	)
	.await;
	assert_eq!(stored_secrets(&db, "keyed").await, (true, false, true));
	assert_eq!(stored_secrets(&db, "tokened").await, (false, true, true));
	assert_eq!(
		app_source(&db, "regged").await,
		(
			"image".to_owned(),
			Some("traefik/whoami:v1.10".to_owned()),
			true
		)
	);

	for slug in ["keyed", "tokened", "regged"] {
		let id = application_id(&db, slug).await;
		for uri in [
			format!("{}/{id}", f.base()),
			format!("{}/{id}/advanced", f.base()),
			format!("/teams/{}/projects/{}", f.team, f.project),
		] {
			let (s, page) = f.get(&uri).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_not_contains(&page, "BEGIN");
			assert_not_contains(&page, "b3BlbnNzaC1rZXk");
			assert_not_contains(&page, TOKEN);
			assert_not_contains(&page, SECRET);
		}
	}
	let id = application_id(&db, "keyed").await;
	let (_, page) = f.get(&format!("{}/{id}/settings", f.base())).await;
	assert_contains(&page, "Uses the key");
	assert_contains(&page, "ssh-ed25519 ");
	let id = application_id(&db, "regged").await;
	let (_, page) = f.get(&format!("{}/{id}/settings", f.base())).await;
	assert_contains(&page, "Registry credentials are stored.");

	// A reader sees that a credential exists, in words, and never what it is.
	let carol = f.member("carol").await;
	let id = application_id(&db, "keyed").await;
	let (_, _, page) = get_html(&f.app, &format!("{}/{id}/settings", f.base()), Some(&carol)).await;
	assert_contains(&page, "Deploy key stored");
}

/// Was `blank_credentials_on_save_keep_the_stored_ones`.
#[db_test]
async fn blank_credentials_on_save_keep_the_stored_ones(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create(
		"keyed",
		json!({ "repo_url": "git@github.com:me/r.git", "deploy_key": KEY }),
	)
	.await;
	f.create("tokened", json!({ "git_token": TOKEN })).await;
	f.create_image(
		"regged",
		json!({ "registry_username": "me", "registry_password": SECRET }),
	)
	.await;
	for (slug, repo) in [
		("keyed", "git@github.com:me/r.git"),
		("tokened", "https://example.com/web.git"),
	] {
		let id = application_id(&db, slug).await;
		let before = stored_secrets(&db, slug).await;
		let body = f
			.post(
				&format!("{}/{id}/settings", f.base()),
				&f.form(slug, json!({ "repo_url": repo, "name": "Renamed" })),
			)
			.await;
		assert_contains(&body, "Settings saved");
		assert_eq!(stored_secrets(&db, slug).await, before, "{slug}");
		assert_not_contains(&body, TOKEN);
		assert_not_contains(&body, "BEGIN");
	}
	let id = application_id(&db, "keyed").await;
	let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
	assert_contains(&page, "Renamed");

	// A new value replaces it; the response never carries it back.
	let id = application_id(&db, "tokened").await;
	let body = f
		.post(
			&format!("{}/{id}/settings", f.base()),
			&f.form(
				"tokened",
				json!({ "git_token": "ghp_replacement-token-value" }),
			),
		)
		.await;
	assert_not_contains(&body, "ghp_replacement-token-value");
	assert_eq!(stored_secrets(&db, "tokened").await, (false, true, true));

	// The same for a registry login: blank keeps it, the page never shows it.
	let id = application_id(&db, "regged").await;
	let body = f
		.post(
			&format!("{}/{id}/settings", f.base()),
			&f.image_form("regged", json!({ "name": "Renamed image" })),
		)
		.await;
	assert_contains(&body, "Settings saved");
	assert_not_contains(&body, SECRET);
	assert!(app_source(&db, "regged").await.2, "the login was dropped");
}

/// Was `bad_input_is_an_inline_message_and_creates_nothing`.
#[db_test]
async fn bad_input_is_an_inline_message_and_creates_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let cases = [
		(
			json!({ "repo_url": "ftp://nope" }),
			"That repository URL isn't valid.",
		),
		(json!({ "repo_url": "" }), "Repository URL is required."),
		(
			json!({ "git_token": TOKEN, "repo_url": "git@github.com:me/r.git" }),
			"An access token needs an https://",
		),
		(json!({ "deploy_key": KEY }), "A deploy key needs an ssh://"),
		(
			json!({ "container_port": "abc" }),
			"Container port must be a number.",
		),
		(
			json!({ "container_port": "" }),
			"Container port is required.",
		),
		(json!({ "container_port": "70000" }), "between 1 and 65535"),
		(
			json!({ "memory_limit_mb": "lots" }),
			"Memory limit must be a number.",
		),
		(
			json!({ "cpu_limit": "0" }),
			"CPU limit must be greater than 0.",
		),
		(
			json!({ "dockerfile_path": "../etc/passwd" }),
			"That Dockerfile path isn't valid.",
		),
		(json!({ "name": "" }), "Name must be"),
		(json!({ "slug": "Not A Slug" }), "Slug must be"),
		(json!({ "server_id": "" }), "Pick a server."),
	];
	for (extra, message) in cases {
		let body = f.create("web", extra.clone()).await;
		assert_contains(&body, message);
		assert_contains(&body, "_error");
		assert_not_contains(&body, "Application created");
		assert_eq!(count(&db, "applications").await, 0, "{extra}");
	}

	f.create("web", json!({})).await;
	let body = f.create("web", json!({})).await;
	assert_contains(&body, "already used by another application");
	assert_eq!(count(&db, "applications").await, 1);

	// The same rules on save, and a slug conflict shows inline too.
	f.create("other", json!({})).await;
	let id = application_id(&db, "other").await;
	let uri = format!("{}/{id}/settings", f.base());
	let body = f.post(&uri, &f.form("web", json!({}))).await;
	assert_contains(&body, "already used by another application");
	let body = f
		.post(&uri, &f.form("other", json!({ "repo_url": "nope" })))
		.await;
	assert_contains(&body, "That repository URL isn't valid.");
	// The person reads a sentence, never the name of an API field.
	assert_not_contains(&body, "repo_url");
}

#[db_test]
async fn image_input_is_checked_and_a_source_cannot_be_mixed_or_switched(db: PgPool) {
	let f = Fixture::new(&db).await;
	let cases = [
		(json!({ "image": "" }), "Image is required."),
		(
			json!({ "image": "not a ref" }),
			"That image reference isn't valid.",
		),
		(
			json!({ "registry_username": "me" }),
			"Give both a registry username and password",
		),
		(
			json!({ "container_port": "" }),
			"Container port is required.",
		),
	];
	for (extra, message) in cases {
		let body = f.create_image("img", extra.clone()).await;
		assert_contains(&body, message);
		assert_not_contains(&body, "Application created");
		assert_eq!(count(&db, "applications").await, 0, "{extra}");
	}

	// A git form that also carries an image field is a git application: the image is ignored.
	f.create("git", json!({ "image": "evil/image" })).await;
	assert_eq!(app_source(&db, "git").await.0, "git");

	// A save cannot turn an image application into a git one, or back.
	f.create_image("img", json!({})).await;
	let id = application_id(&db, "img").await;
	let body = f
		.post(
			&format!("{}/{id}/settings", f.base()),
			&f.form("img", json!({ "source": "git" })),
		)
		.await;
	assert_contains(&body, "Image is required.");
	assert_eq!(app_source(&db, "img").await.0, "image");
}

/// Creating an image application through the page works and the General tab shows its image.
#[db_test]
async fn creating_an_image_app_shows_the_image_on_general_and_deploys_it(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f
		.create_image(
			"whoami",
			json!({ "registry_username": "me", "registry_password": SECRET }),
		)
		.await;
	assert_contains(&body, "Application created");
	assert_contains(&body, "registry or CI");
	assert_not_contains(&body, "git provider");
	let webhook_secret = shown_value(&body, "Webhook secret");
	assert_eq!(webhook_secret.len(), 48);
	assert_eq!(
		app_source(&db, "whoami").await,
		(
			"image".to_owned(),
			Some("traefik/whoami:v1.10".to_owned()),
			true
		)
	);
	assert_eq!(stored_secrets(&db, "whoami").await, (false, false, true));
	assert_not_contains(&body, SECRET);

	let id = application_id(&db, "whoami").await;
	let app_uri = format!("{}/{id}", f.base());
	let (s, page) = f.get(&app_uri).await;
	assert_eq!(s, StatusCode::OK);
	// The header says what it runs and from where, and an image is never "cloned": nothing
	// on its page speaks of a repository or a git provider.
	assert_contains(&page, "traefik/whoami:v1.10");
	assert_contains(&page, "Docker image app");
	assert_not_contains(&page, "Not cloned yet");
	let (_, hooks) = f.get(&format!("{app_uri}/webhooks")).await;
	assert_contains(&hooks, "Deploy on a new image");
	assert_not_contains(&hooks, "git provider");
	assert_not_contains(&hooks, "GitHub");
	// The form holds the image and no git field.
	let (s, page) = f.get(&format!("{app_uri}/settings")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "&quot;image&quot;:&quot;traefik/whoami:v1.10&quot;");
	assert_contains(&page, "data-bind:registry_username");
	assert_not_contains(&page, "data-bind:repo_url");
	assert_not_contains(&page, "data-bind:build_strategy");
	assert_not_contains(&page, SECRET);

	// The project's grid draws it with its image.
	let (_, grid) = f
		.get(&format!("/teams/{}/projects/{}", f.team, f.project))
		.await;
	assert_contains(&grid, "traefik/whoami:v1.10");

	// Deploy queues a deployment (the agent would pull the image) and opens its page.
	let body = f.post_empty(&format!("{app_uri}/deploy")).await;
	let deployment = newest_deployment(&db).await;
	assert_contains(
		&body,
		&format!("window.location.assign('{app_uri}/deployments/{deployment}')"),
	);
	assert_eq!(deployment_status(&db, deployment).await, "queued");
}

/// The picker passes `?environment=<id>`; the page carries it into the created application.
#[db_test]
async fn an_application_is_created_in_the_chosen_environment(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.post(
		&format!("/teams/{}/projects/{}/environments", f.team, f.project),
		&json!({ "newName": "Staging", "newSlug": "staging" }),
	)
	.await;
	let staging = env_id(&db, f.project, "staging").await;
	let uri = format!("{}/new?environment={staging}&source=image", f.base());
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(
		&page,
		&format!("&quot;environment&quot;:&quot;{staging}&quot;"),
	);
	assert_contains(&page, &format!("environment={staging}"));

	f.create_image("whoami", json!({ "environment": staging.to_string() }))
		.await;
	assert_eq!(app_environment(&db, "whoami").await, staging);
	f.create("plain", json!({})).await;
	assert_ne!(app_environment(&db, "plain").await, staging);

	// Its page sits in the environment it lives in: the breadcrumb names it.
	let id = application_id(&db, "whoami").await;
	let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
	assert_contains(&page, "Staging");
}

/// Was `a_non_active_server_cannot_be_chosen_by_a_forged_post`.
#[db_test]
async fn a_non_active_server_cannot_be_chosen_by_a_forged_post(db: PgPool) {
	let f = Fixture::new(&db).await;
	// Another team's server is not found at all.
	let (_, other_team) = user_with_team(&f.app, "bob").await;
	let theirs = local_server_id(&db, other_team).await;
	let body = f
		.create("web", json!({ "server_id": theirs.to_string() }))
		.await;
	assert_contains(&body, "That no longer exists.");
	assert_eq!(count(&db, "applications").await, 0);
}

// --- general, delete ------------------------------------------------------------------------------

/// Was `settings_are_prefilled_and_saving_toasts`.
#[db_test]
async fn the_general_tab_is_prefilled_and_saving_toasts(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create(
		"web",
		json!({ "memory_limit_mb": "256", "cpu_limit": "0.5", "build_strategy": "nixpacks" }),
	)
	.await;
	let id = application_id(&db, "web").await;
	let uri = format!("{}/{id}/settings", f.base());
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "&quot;name&quot;:&quot;Web&quot;");
	assert_contains(&page, "&quot;memory_limit_mb&quot;:&quot;256&quot;");
	assert_contains(&page, "&quot;cpu_limit&quot;:&quot;0.5&quot;");
	assert_contains(&page, "&quot;build_strategy&quot;:&quot;nixpacks&quot;");
	assert_contains(&page, "Unsaved changes");
	assert_not_contains(&page, "&quot;server_id&quot;");

	let body = f
		.post(
			&uri,
			&f.form(
				"web2",
				json!({ "name": "Web 2", "container_port": "8080", "memory_limit_mb": "" }),
			),
		)
		.await;
	assert_contains(&body, "Settings saved");
	assert_contains(&body, "app-header");
	assert_contains(&body, "web2.127-0-0-1.sslip.io");
	let (_, page) = f.get(&uri).await;
	assert_contains(&page, "Web 2");
	assert_contains(&page, "&quot;container_port&quot;:&quot;8080&quot;");

	// Settings has its own address; the application's root is the Deployments tab.
	let (s, _, _) = get_html(&f.app, &uri, Some(&f.cookie)).await;
	assert_eq!(s, StatusCode::OK);
}

/// Was `deleting_needs_the_slug_typed_exactly`.
#[db_test]
async fn deleting_needs_the_slug_typed_exactly(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create("doomed", json!({})).await;
	let id = application_id(&db, "doomed").await;
	let uri = format!("{}/{id}/delete", f.base());

	// The dialog is on every tab and says what it will do; its button waits for the slug.
	let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
	assert_contains(&page, "id=\"delete-dialog\"");
	assert_contains(&page, "Type ");
	assert_contains(&page, "$confirm !== 'doomed'");

	let body = f.post(&uri, &json!({ "confirm": "nope" })).await;
	assert_contains(&body, "doesn't match");
	assert_eq!(count(&db, "applications").await, 1);

	let body = f.post(&uri, &json!({ "confirm": "doomed" })).await;
	assert_contains(
		&body,
		&format!(
			"window.location.assign('/teams/{}/projects/{}?environment=",
			f.team, f.project
		),
	);
	assert_eq!(count(&db, "applications").await, 0);
	let (s, _) = f.get(&format!("{}/{id}", f.base())).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn the_advanced_tab_holds_the_identifiers_and_the_danger_zone(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("advanced").await;
	assert_contains(&page, "Danger zone");
	assert_contains(&page, "Delete application");
	assert_contains(&page, &f.application.to_string());
	// The dialog the button opens is the one the header menu opens.
	assert_contains(&page, "commandfor=\"delete-dialog\"");
	assert_eq!(page.matches("id=\"delete-dialog\"").count(), 1);
}

/// Was `the_project_overview_links_apps_with_url_and_latest_status`.
#[db_test]
async fn the_project_overview_links_apps_with_url_and_latest_status(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create("web", json!({})).await;
	f.create("idle", json!({ "name": "Idle" })).await;
	let id = application_id(&db, "web").await;
	seed_deployment(&db, id, "running").await;

	let (s, page) = f
		.get(&format!("/teams/{}/projects/{}", f.team, f.project))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, &format!("href=\"{}/{id}\"", f.base()));
	assert_contains(&page, "web.127-0-0-1.sslip.io");
	assert_contains(&page, "Running");
	assert_contains(&page, "Not deployed");
	// The create-resource picker (its own page now) sends "Git repository" and "Docker image"
	// to the new page.
	let (s, page) = f
		.get(&format!(
			"/teams/{}/projects/{}/production/create",
			f.team, f.project
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Git repository");
	assert_contains(&page, "Docker image");
	assert_contains(&page, &format!("{}/new?environment=", f.base()));
	assert_contains(&page, "source=image");
}

/// Was `names_are_escaped`.
#[db_test]
async fn names_are_escaped(db: PgPool) {
	let f = Fixture::new(&db).await;
	let name = "<script>alert(1)</script>";
	f.create("xss", json!({ "name": name, "git_ref": "<b>x</b>" }))
		.await;
	let id = application_id(&db, "xss").await;
	for uri in [
		format!("{}/{id}", f.base()),
		format!("{}/{id}/deployments", f.base()),
		format!("{}/{id}/models", f.base()),
		format!("{}/{id}/webhooks", f.base()),
		format!("{}/{id}/advanced", f.base()),
		format!("/teams/{}/projects/{}", f.team, f.project),
	] {
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&page, name);
		assert_not_contains(&page, "<b>x</b>");
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}
}

/// Was `another_teams_application_is_the_404_page_and_cannot_be_changed`.
#[db_test]
async fn another_teams_application_is_the_404_page_and_cannot_be_changed(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create(
		"secret-app",
		json!({ "name": "Secret app", "git_token": TOKEN }),
	)
	.await;
	let id = application_id(&db, "secret-app").await;
	let (bob_cookie, bob_team) = ui_session(&f.app, "bob").await;

	let tabs = [
		"",
		"/settings",
		"/env",
		"/domains",
		"/deployments",
		"/logs",
		"/volumes",
		"/models",
		"/webhooks",
		"/advanced",
	];
	let mut uris = vec![
		// Alice's team, Alice's project.
		format!("/teams/{}/projects/{}/apps/new", f.team, f.project),
		// Bob's own team with Alice's ids.
		format!("/teams/{bob_team}/projects/{}/apps/new", f.project),
		format!("/teams/{bob_team}/projects/{}", f.project),
	];
	for tab in tabs {
		uris.push(format!(
			"/teams/{}/projects/{}/apps/{id}{tab}",
			f.team, f.project
		));
		uris.push(format!(
			"/teams/{bob_team}/projects/{}/apps/{id}{tab}",
			f.project
		));
	}
	for uri in uris {
		let (s, _, body) = get_html(&f.app, &uri, Some(&bob_cookie)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_contains(&body, "Not found");
		assert_not_contains(&body, "Secret app");
		assert_not_contains(&body, "secret-app");
	}

	let before = stored_secrets(&db, "secret-app").await;
	let secret_before = webhook_secret_blob(&db, "secret-app").await;
	let mine = format!("{}/{id}", f.base());
	let theirs = format!("/teams/{bob_team}/projects/{}/apps/{id}", f.project);
	let mut posts = vec![
		(f.base(), f.form("planted", json!({}))),
		(
			format!("{mine}/settings"),
			f.form("secret-app", json!({ "name": "Hacked" })),
		),
		(format!("{mine}/delete"), json!({ "confirm": "secret-app" })),
		(
			format!("{theirs}/delete"),
			json!({ "confirm": "secret-app" }),
		),
		(
			format!("{theirs}/settings"),
			f.form("secret-app", json!({ "name": "Hacked" })),
		),
	];
	for base in [&mine, &theirs] {
		for path in ["/webhooks/rotate", "/deploy", "/restart", "/stop"] {
			posts.push((format!("{base}{path}"), json!({})));
		}
	}
	for (uri, signals) in posts {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&bob_cookie), &signals).await;
		assert_contains(&body, "That no longer exists.");
		assert_not_contains(&body, "Application created");
	}
	assert_eq!(count(&db, "applications").await, 1);
	assert_eq!(stored_secrets(&db, "secret-app").await, before);
	assert_eq!(webhook_secret_blob(&db, "secret-app").await, secret_before);
	let (_, page) = f.get(&mine).await;
	assert_contains(&page, "Secret app");
	assert_not_contains(&page, "Hacked");
}

/// Was `a_read_only_member_sees_no_actions_and_forged_posts_fail`.
#[db_test]
async fn a_read_only_member_sees_no_actions_and_forged_posts_fail(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create("web", json!({ "name": "Shared" })).await;
	let id = application_id(&db, "web").await;
	let carol = f.member("carol").await;

	let (s, _, page) = get_html(&f.app, &format!("{}/{id}", f.base()), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Shared");
	// The page shows the facts, with no form and none of the tabs that need more than reading.
	let main = page_main(&page);
	assert_not_contains(main, "Unsaved changes");
	assert_not_contains(main, "Advanced");
	assert_not_contains(main, "Webhooks");
	assert_not_contains(main, "Delete");
	let (s, _, page) = get_html(
		&f.app,
		&format!("/teams/{}/projects/{}", f.team, f.project),
		Some(&carol),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "New application");

	for uri in [
		format!("{}/{id}/webhooks", f.base()),
		format!("{}/{id}/advanced", f.base()),
		format!("{}/new", f.base()),
	] {
		let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
		assert_contains(&page, "permission");
	}

	for (uri, signals) in [
		(f.base(), f.form("forged", json!({}))),
		(
			format!("{}/{id}/settings", f.base()),
			f.form("web", json!({ "name": "Forged" })),
		),
		(
			format!("{}/{id}/delete", f.base()),
			json!({ "confirm": "web" }),
		),
		(format!("{}/{id}/webhooks/rotate", f.base()), json!({})),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
		assert_contains(&body, "You don't have permission to do that.");
	}
	assert_eq!(count(&db, "applications").await, 1);
	let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
	assert_not_contains(&page, "Forged");

	// The owner has the form, the tabs and the delete dialog.
	let (_, page) = f.get(&format!("{}/{id}/settings", f.base())).await;
	assert_contains(&page, "Unsaved changes");
	assert_contains(&page, "Advanced");
	assert_contains(&page, "Webhooks");
	assert_contains(&page, "id=\"delete-dialog\"");
}

/// Was `the_json_api_still_creates_updates_and_deletes`.
#[db_test]
async fn the_json_api_still_creates_updates_and_deletes(db: PgPool) {
	let app = crate::tests::app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "P", "slug": "p" })),
	)
	.await;
	let project = project["id"].as_i64().unwrap();
	let server = local_server_id(&db, team).await;
	let base = format!("/api/v1/teams/{team}/projects/{project}/applications");
	let body = json!({
		"name": "A", "slug": "a", "server_id": server,
		"repo_url": "https://example.com/a.git", "container_port": 80,
	});
	let (s, created) = call(&app, "POST", &base, Some(&token), Some(body)).await;
	assert_eq!(s, StatusCode::OK, "{created}");
	assert!(
		created["webhook_secret"]
			.as_str()
			.is_some_and(|s| s.len() == 48)
	);
	let id = created["id"].as_i64().unwrap();

	let (_, list) = call(&app, "GET", &base, Some(&token), None).await;
	assert_eq!(list[0]["slug"], "a");
	assert!(list[0].get("webhook_secret").is_none());

	let (s, updated) = call(
		&app,
		"PATCH",
		&format!("{base}/{id}"),
		Some(&token),
		Some(json!({
			"name": "B", "slug": "a", "repo_url": "https://example.com/a.git",
			"container_port": 81,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{updated}");
	assert_eq!(updated["name"], "B");

	let (s, _) = call(&app, "DELETE", &format!("{base}/{id}"), Some(&token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&db, "applications").await, 0);
}

// --- the header and its actions -----------------------------------------------------------------

/// Was `the_header_buttons_follow_the_latest_state`.
#[db_test]
async fn the_header_buttons_follow_the_latest_state(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let base = f.url("");
	let (_, page) = f.get(&base).await;
	assert_contains(&page, "Deploy");
	assert_contains(&page, &format!("{base}/deploy"));
	// The page says what it is: logo, name, type, status, address.
	assert_contains(&page, "Application");
	assert_contains(&page, "Git");
	assert_contains(&page, "Not deployed");
	assert_contains(&page, "web.127-0-0-1.sslip.io");
	// Nothing built or running: Restart and Stop exist but are disabled.
	assert_not_contains(&page, &format!("{base}/restart"));
	assert_not_contains(&page, &format!("{base}/stop"));
	assert_contains(&page, "disabled");

	f.seed(100, "running", Some("img:1"), Some("c1")).await;
	let (_, page) = f.get(&base).await;
	assert_contains(&page, &format!("{base}/restart"));
	assert_contains(&page, &format!("{base}/stop"));
	assert_contains(&page, "Running");

	set_deployment_status(&db, 100, "stopped").await;
	let (_, page) = f.get(&base).await;
	assert_contains(&page, &format!("{base}/restart"));
	assert_not_contains(&page, &format!("{base}/stop"));
}

#[db_test]
async fn the_header_is_one_fragment_that_polls_only_while_a_deployment_is_in_flight(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	let uri = f.url("header");
	let (s, body) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "datastar-patch-elements");
	assert_contains(&body, "id=\"app-header\"");
	assert_not_contains(&body, "data-on-interval");

	f.seed(101, "building", None, None).await;
	let (_, body) = f.get(&uri).await;
	assert_contains(&body, "data-on-interval__duration.3s");
	assert_contains(&body, &uri);

	// A page of another team is the real 404, so a poll that lost its application stops.
	let (bob, bob_team) = ui_session(&f.app, "bob").await;
	let theirs = format!(
		"/teams/{bob_team}/projects/{}/apps/{}/header",
		f.project, f.application
	);
	let (s, _, _) = get_html(&f.app, &theirs, Some(&bob)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn the_menu_offers_rollback_to_a_build_and_delete_behind_dialogs(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let (_, page) = f.get(&f.url("")).await;
	// Nothing to roll back to yet: the entry is there and disabled.
	assert_contains(&page, "id=\"app-menu\"");
	assert_contains(&page, "id=\"rollback-dialog\"");
	assert_contains(&page, "id=\"delete-dialog\"");
	assert_not_contains(&page, "$_rb = '");

	f.seed(100, "stopped", Some("img:1"), None).await;
	f.seed(101, "running", Some("img:2"), Some("c1")).await;
	let (_, page) = f.get(&f.url("")).await;
	// The newest build that is not live is the one the menu rolls back to.
	assert_contains(&page, "$_rb = '100'");
	assert_not_contains(&page, "$_rb = '101'");
	// The dialog posts to the rollback of whichever id the signal holds.
	assert_contains(
		&page,
		&format!("{}/deployments/' + $_rb + '/rollback", f.url("")),
	);
	// The rollback dialog starts on the safe choice.
	let rollback = page.split("id=\"rollback-dialog\"").nth(1).unwrap();
	assert!(
		rollback
			.split("</dialog>")
			.next()
			.unwrap()
			.contains("autofocus"),
		"{rollback}"
	);
}

/// Was `deploy_queues_a_deployment_and_opens_its_page_with_a_streaming_log`.
#[db_test]
async fn deploy_queues_a_deployment_and_opens_its_page_with_a_streaming_log(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let body = f.post_empty(&format!("{}/deploy", f.url(""))).await;
	let id = newest_deployment(&db).await;
	assert_contains(
		&body,
		&format!("window.location.assign('{}/deployments/{id}')", f.url("")),
	);
	assert_eq!(deployment_status(&db, id).await, "queued");

	let page_uri = format!("{}/deployments/{id}", f.url(""));
	let (s, page) = f.get(&page_uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Queued");
	assert_contains(&page, &format!("Deployment #{id}"));
	assert_contains(&page, "Cancel deployment");
	// Polls while queued, and the log view points at this deployment's log.
	assert_contains(&page, &format!("{page_uri}/status"));
	assert_contains(&page, &format!("{page_uri}/log"));
	assert_contains(&page, "id=\"build-log\"");
	// The viewer is a named region the keyboard can scroll, with search, follow and copy.
	assert_contains(&page, "id=\"build-log-scroll\"");
	assert_contains(&page, "tabindex=\"0\"");
	assert_contains(&page, "aria-label=\"Build log\"");
	assert_contains(&page, "Search the build log");
	assert_contains(&page, "data-bind:_follow");
	assert_contains(&page, "Copy");

	// A fake job log: the agent isn't needed.
	let job = deploy_job_id(&db, id).await;
	let mut log = f.state.logs.open(JobId(job)).await.unwrap();
	log.push(proto::Stream::Stdout, "Step 1/3 : FROM alpine")
		.await;
	log.push(proto::Stream::Stderr, "<script>alert(1)</script>")
		.await;
	log.finish().await;
	let (s, log_body) = f.get(&format!("{page_uri}/log")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&log_body, "selector #build-log");
	assert_contains(&log_body, "Step 1/3 : FROM alpine");
	assert_contains(&log_body, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(&log_body, "<script>alert(1)");
	assert_contains(&log_body, "_live");

	// Cancelling the queued deployment works end to end, and polling shows it.
	let body = f.post_empty(&format!("{page_uri}/cancel")).await;
	assert_contains(&body, "Cancelling");
	let (_, page) = f.get(&page_uri).await;
	assert_contains(&page, "Cancelled");
	assert_not_contains(&page, "Cancel deployment");
	assert_not_contains(&page, &format!("{page_uri}/status"));
}

/// Was `the_history_lists_deployments_and_polls_only_while_one_is_in_flight`.
#[db_test]
async fn the_history_lists_deployments_and_polls_only_while_one_is_in_flight(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let uri = f.url("deployments");
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Nothing has been deployed yet.");

	f.seed(100, "stopped", Some("img:1"), None).await;
	let (_, page) = f.get(&uri).await;
	assert_contains(&page, "Stopped");
	assert_contains(&page, "abcdef1");
	assert_not_contains(&page, "abcdef12");
	assert_contains(&page, ">Webhook<");
	assert_contains(&page, "1m 15s");
	assert_contains(&page, "ui-timeline");
	assert_not_contains(&page, "data-on-interval");

	f.seed(101, "building", None, None).await;
	let (_, page) = f.get(&uri).await;
	assert_contains(&page, "Building");
	assert_contains(&page, "data-on-interval__duration.3s");
	let fragment = f.get(&format!("{uri}/table")).await.1;
	assert_contains(&fragment, "datastar-patch-elements");
	assert_contains(&fragment, "data-on-interval__duration.3s");

	set_deployment_status(&db, 101, "failed").await;
	let fragment = f.get(&format!("{uri}/table")).await.1;
	assert_not_contains(&fragment, "data-on-interval");

	// A user-triggered one says so.
	f.post_empty(&format!("{}/deploy", f.url(""))).await;
	let (_, page) = f.get(&uri).await;
	assert_contains(&page, "Deployed by hand");
}

/// Was `rollback_and_cancel_show_only_in_valid_states`.
#[db_test]
async fn rollback_and_cancel_show_only_in_valid_states(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	f.seed(101, "running", Some("img:2"), Some("c1")).await;
	f.seed(102, "failed", None, None).await;
	f.seed(103, "queued", None, None).await;
	f.seed(104, "building", None, None).await;
	let (_, page) = f.get(&f.url("deployments")).await;
	// A row's Rollback opens the dialog for that deployment; only a finished build has one.
	let rollback = |id: i64| format!("$_rb = '{id}'; el.commandForElement");
	let cancel = |id: i64| format!("{}/deployments/{id}/cancel", f.url(""));
	assert_contains(&page, &rollback(100));
	assert_contains(&page, "rollback-dialog");
	assert_contains(&page, "Roll back");
	for id in [101, 102, 103, 104] {
		assert_not_contains(&page, &rollback(id));
	}
	assert_contains(&page, &cancel(103));
	assert_contains(&page, &cancel(104));
	for id in [100, 101, 102] {
		assert_not_contains(&page, &cancel(id));
	}

	// The page of a running deployment offers no cancel either.
	let (_, page) = f.get(&format!("{}/deployments/101", f.url(""))).await;
	assert_not_contains(&page, "Cancel deployment");
	let (_, page) = f.get(&format!("{}/deployments/104", f.url(""))).await;
	assert_contains(&page, "Cancel deployment");
}

/// Was `rollback_redeploys_the_image_and_refuses_what_cannot_be_rolled_back`.
#[db_test]
async fn rollback_redeploys_the_image_and_refuses_what_cannot_be_rolled_back(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	f.seed(101, "running", Some("img:2"), Some("c1")).await;
	f.seed(102, "failed", None, None).await;
	let rollback = |id: i64| format!("{}/deployments/{id}/rollback", f.url(""));

	let before = count(&db, "deployments").await;
	for (id, message) in [
		(101, "live or still in progress"),
		(102, "never produced an image"),
	] {
		let body = f.post_empty(&rollback(id)).await;
		assert_contains(&body, message);
		assert_contains(&body, "_action_error");
	}
	let body = f.post_empty(&rollback(999)).await;
	assert_contains(&body, "That no longer exists.");
	assert_eq!(count(&db, "deployments").await, before);

	let body = f.post_empty(&rollback(100)).await;
	let new = newest_deployment(&db).await;
	assert_ne!(new, 102);
	assert_contains(
		&body,
		&format!("window.location.assign('{}/deployments/{new}')", f.url("")),
	);
	assert_eq!(deployment_image(&db, new).await.as_deref(), Some("img:1"));
	assert_eq!(count(&db, "deployments").await, before + 1);
}

/// Was `cancelling_a_finished_deployment_is_refused_inline`.
#[db_test]
async fn cancelling_a_finished_deployment_is_refused_inline(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	let body = f
		.post_empty(&format!("{}/deployments/100/cancel", f.url("")))
		.await;
	assert_contains(&body, "not in progress");
	assert_not_contains(&body, "Cancelling");
}

/// Was `stop_and_restart_show_the_api_refusals_inline`.
#[db_test]
async fn stop_and_restart_show_the_api_refusals_inline(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let body = f.post_empty(&format!("{}/stop", f.url(""))).await;
	assert_contains(&body, "not currently running");
	let body = f.post_empty(&format!("{}/restart", f.url(""))).await;
	assert_contains(&body, "no previous successful build");
	assert_eq!(count(&db, "deployments").await, 0);

	f.seed(100, "stopped", Some("img:1"), None).await;
	let body = f.post_empty(&format!("{}/restart", f.url(""))).await;
	let new = newest_deployment(&db).await;
	assert_contains(
		&body,
		&format!("window.location.assign('{}/deployments/{new}')", f.url("")),
	);
	assert_eq!(count(&db, "deployments").await, 2);
}

/// Was `the_logs_tab_says_so_when_nothing_runs`.
#[db_test]
async fn the_logs_tab_says_so_when_nothing_runs(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let uri = f.url("logs");
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "isn't running");
	assert_not_contains(&page, "runtime-log");

	f.seed(100, "stopped", Some("img:1"), None).await;
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "isn't running");

	// The stream endpoint refuses in a signal, not a 500.
	let (s, body) = f.get(&format!("{uri}/stream")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "not currently running");
	assert_contains(&body, "#runtime-log");
	assert_contains(&body, "_live");
}

/// Was `the_logs_tab_streams_a_chosen_tail_while_running`.
#[db_test]
async fn the_logs_tab_streams_a_chosen_tail_while_running(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "running", Some("img:1"), Some("c1")).await;
	let uri = f.url("logs");
	let (s, page) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "isn't running");
	assert_contains(&page, "id=\"runtime-log\"");
	assert_contains(&page, &format!("{uri}/stream?tail=200&amp;follow=1"));
	for tail in ["200", "1000", "5000"] {
		assert_contains(&page, &format!("value=\"{tail}\""));
	}

	let (_, page) = f.get(&format!("{uri}?tail=5000&follow=0")).await;
	assert_contains(&page, &format!("{uri}/stream?tail=5000&amp;follow=0"));
	assert_contains(&page, "value=\"5000\" selected");
	// Anything else falls back to the default.
	let (_, page) = f.get(&format!("{uri}?tail=999999&follow=x")).await;
	assert_contains(&page, &format!("{uri}/stream?tail=200&amp;follow=1"));

	// With no agent behind it the stream fails inline, not with a 500.
	let (s, body) = f.get(&format!("{uri}/stream")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "#runtime-log");
}

/// Was `a_read_only_member_sees_no_buttons_and_forged_posts_are_refused`.
#[db_test]
async fn a_read_only_member_sees_no_buttons_and_forged_deployment_posts_are_refused(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	f.seed(101, "running", Some("img:2"), Some("c1")).await;
	f.seed(102, "queued", None, None).await;
	let carol = f.member("carol").await;

	for uri in [
		f.url(""),
		f.url("deployments"),
		f.url("deployments/100"),
		f.url("deployments/102"),
		f.url("logs"),
	] {
		let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		for control in [
			">Rollback<",
			">Cancel<",
			">Restart<",
			">Stop<",
			">Deploy<",
			"/deploy'",
			"/stop'",
			"/restart'",
			"rollback-dialog",
			"delete-dialog",
		] {
			assert_not_contains(&page, control);
		}
	}
	let (_, _, page) = get_html(&f.app, &f.url(""), Some(&carol)).await;
	assert_contains(&page, "Deployments");

	let before = count(&db, "deployments").await;
	for uri in [
		format!("{}/deploy", f.url("")),
		format!("{}/restart", f.url("")),
		format!("{}/stop", f.url("")),
		format!("{}/deployments/100/rollback", f.url("")),
		format!("{}/deployments/102/cancel", f.url("")),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &json!({})).await;
		assert_contains(&body, "You don't have permission to do that.");
	}
	assert_eq!(count(&db, "deployments").await, before);
	assert_eq!(deployment_status(&db, 102).await, "queued");
	assert_eq!(deployment_status(&db, 101).await, "running");
}

/// Was `text_from_a_failed_build_is_escaped`.
#[db_test]
async fn text_from_a_failed_build_is_escaped(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "failed", None, None).await;
	set_deployment_error(&db, 100, "<script>alert(1)</script>").await;
	let (s, page) = f.get(&format!("{}/deployments/100", f.url(""))).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "<script>alert(1)");
	assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
}

/// Was `another_teams_deployments_are_the_404_page_and_cannot_be_changed`.
#[db_test]
async fn another_teams_deployments_are_the_404_page_and_cannot_be_changed(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.seed(100, "stopped", Some("img:1"), None).await;
	f.seed(101, "queued", None, None).await;
	let (bob, bob_team) = ui_session(&f.app, "bob").await;
	let theirs = format!(
		"/teams/{bob_team}/projects/{}/apps/{}",
		f.project, f.application
	);

	for uri in [
		format!("{theirs}/deployments"),
		format!("{theirs}/deployments/100"),
		format!("{theirs}/logs"),
		format!("{theirs}/deployments/100/log"),
	] {
		let (s, _, body) = get_html(&f.app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_not_contains(&body, "Stopped");
	}
	// Alice's own team path, Bob's cookie: not a member, so the same page.
	for uri in [
		f.url("deployments"),
		f.url("deployments/100"),
		f.url("logs"),
	] {
		let (s, _, _) = get_html(&f.app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
	}

	let before = count(&db, "deployments").await;
	for base in [theirs.clone(), f.url("")] {
		for path in [
			"/deploy",
			"/restart",
			"/stop",
			"/deployments/100/rollback",
			"/deployments/101/cancel",
		] {
			let (_, _, body) =
				post_signals(&f.app, &format!("{base}{path}"), Some(&bob), &json!({})).await;
			assert_contains(&body, "That no longer exists.");
		}
	}
	assert_eq!(count(&db, "deployments").await, before);
	assert_eq!(deployment_status(&db, 101).await, "queued");
}

/// Was `the_json_deployment_endpoints_still_work`.
#[db_test]
async fn the_json_deployment_endpoints_still_work(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let api = format!(
		"/api/v1/teams/{}/projects/{}/applications/{}",
		f.team, f.project, f.application
	);
	let (s, d) = call(
		&f.app,
		"POST",
		&format!("{api}/deployments"),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED, "{d}");
	let id = d["id"].as_i64().unwrap();
	assert_eq!(d["status"], "queued");
	let (s, d) = call(
		&f.app,
		"POST",
		&format!("{api}/deployments/{id}/cancel"),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED, "{d}");
	assert_eq!(d["status"], "cancelled");
	let (s, e) = call(&f.app, "POST", &format!("{api}/stop"), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
	let (s, e) = call(
		&f.app,
		"POST",
		&format!("{api}/restart"),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
	let (s, _) = call(&f.app, "GET", &format!("{api}/logs"), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

// --- environment ------------------------------------------------------------------------------------

/// Was `an_env_value_never_appears_in_any_response`.
#[db_test]
async fn an_env_value_never_appears_in_any_response(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let (a, b) = (format!("{SECRET}-a"), format!("{SECRET}-b"));
	let body = f.set_env("API_TOKEN", &a).await;
	assert_contains(&body, "API_TOKEN");
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	f.set_env("API_TOKEN", &b).await;
	f.post(
		&f.url("env/bulk"),
		&json!({ "env_bulk": format!("# c\nBULK_ONE={b}\nnope {b}\nexport BULK_TWO=\"{a}\"\n") }),
	)
	.await;
	let page = f.page("env").await;
	assert_contains(&page, "API_TOKEN");
	assert_contains(&page, "BULK_TWO");
	assert_contains(&page, "Value hidden");
	for tab in [
		"",
		"domains",
		"volumes",
		"deployments",
		"webhooks",
		"advanced",
		"models",
	] {
		f.page(tab).await;
	}
	f.get(&format!("/teams/{}/projects/{}", f.team, f.project))
		.await;
	// The value reached the database encrypted, and the JSON API agrees.
	let stored = env_var_ciphertext(&db, "API_TOKEN").await;
	assert!(!String::from_utf8_lossy(&stored).contains(SECRET));
	let (_, list) = call(&f.app, "GET", &f.api("env-vars"), Some(&f.token), None).await;
	f.seen.borrow_mut().push(list.to_string());

	let seen = f.seen.borrow();
	assert!(seen.len() >= 9);
	for body in seen.iter() {
		assert_not_contains(body, SECRET);
	}
}

/// Was `setting_a_key_again_replaces_it_and_delete_removes_it`.
#[db_test]
async fn setting_a_key_again_replaces_it_and_delete_removes_it(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.set_env("MODE", "one").await;
	f.set_env("MODE", "two").await;
	f.set_env("OTHER", "x").await;
	assert_eq!(f.api_keys().await, ["MODE", "OTHER"]);
	assert_eq!(count(&db, "env_vars").await, 2);

	let id = env_var_id(&db, "MODE").await;
	let body = f.post_empty(&f.url(&format!("env/{id}/delete"))).await;
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	assert_not_contains(&body, "MODE");
	assert_contains(&body, "OTHER");
	assert_eq!(f.api_keys().await, ["OTHER"]);
	let body = f.post_empty(&f.url(&format!("env/{id}/delete"))).await;
	assert_contains(&body, "That no longer exists.");
}

/// A change to the configuration says it waits for the next deploy, and offers to do it now.
#[db_test]
async fn a_saved_change_offers_to_redeploy_now(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("env").await;
	assert_contains(&page, "id=\"saved\"");
	assert_not_contains(&page, "Redeploy now");
	let body = f.set_env("MODE", "one").await;
	assert_contains(&body, "Redeploy now");
	assert_contains(&body, &format!("{}/deploy", f.url("")));
}

/// Was `a_bad_variable_name_is_reported_inline_and_saves_nothing`.
#[db_test]
async fn a_bad_variable_name_is_reported_inline_and_saves_nothing(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	for key in ["1ABC", "A B", "", "A-B"] {
		let body = f.set_env(key, "v").await;
		assert_contains(&body, "Name must be letters, digits and underscores");
		assert_not_contains(&body, "Saved.");
	}
	assert_eq!(count(&db, "env_vars").await, 0);
}

/// Was `a_bulk_paste_saves_the_good_lines_and_names_the_bad_one`.
#[db_test]
async fn a_bulk_paste_saves_the_good_lines_and_names_the_bad_one(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	// The editor is a dialog on the Environment tab.
	let page = f.page("env").await;
	assert_contains(&page, "id=\"env-bulk\"");
	assert_contains(&page, "Edit as .env");
	let paste = format!(
		"# my app\n\nFIRST=1\nthis line has no equals {SECRET}\nSECOND=\"two words\"\nTHIRD={SECRET} # note\n"
	);
	let body = f
		.post(&f.url("env/bulk"), &json!({ "env_bulk": paste }))
		.await;
	assert_contains(&body, "Saved 3 variables.");
	assert_contains(&body, "Line 4: expected KEY=VALUE.");
	assert_not_contains(&body, "Line 3");
	assert_not_contains(&body, "Line 5");
	assert_not_contains(&body, SECRET);
	// The text stays in the box, and the dialog stays open, so the line can be fixed.
	assert_not_contains(&body, r#""env_bulk":"""#);
	assert_not_contains(&body, "\"_close\"");
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	assert_eq!(f.api_keys().await, ["FIRST", "SECOND", "THIRD"]);

	// A clean paste clears the box and closes the dialog.
	let body = f
		.post(&f.url("env/bulk"), &json!({ "env_bulk": "FOURTH=4" }))
		.await;
	assert_contains(&body, "Saved 1 variable.");
	assert_contains(&body, "env_bulk");
	assert_contains(&body, "\"_close\":\"env-bulk\"");
	assert_not_contains(&body, "Line ");
}

/// Was `a_paste_with_only_bad_lines_saves_nothing_and_shows_no_banner`.
#[db_test]
async fn a_paste_with_only_bad_lines_saves_nothing_and_shows_no_banner(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let body = f
		.post(&f.url("env/bulk"), &json!({ "env_bulk": "bad\n2X=1" }))
		.await;
	assert_contains(&body, "Saved 0 variables.");
	assert_contains(&body, "Line 1");
	assert_contains(&body, "Line 2");
	assert_not_contains(&body, "Changes apply");
	assert_eq!(count(&db, "env_vars").await, 0);

	let big = "A=1\n".repeat(501);
	let body = f
		.post(&f.url("env/bulk"), &json!({ "env_bulk": big }))
		.await;
	assert_contains(&body, "Paste at most 500 lines");
	assert_eq!(count(&db, "env_vars").await, 0);
}

// --- domains ----------------------------------------------------------------------------------------

/// Was `domains_are_added_listed_and_removed_with_dns_help`.
#[db_test]
async fn domains_are_added_listed_and_removed_with_dns_help(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("domains").await;
	assert_contains(&page, "No custom domains yet.");
	assert_contains(&page, "DNS setup");
	// The generated address is always there, over plain HTTP.
	assert_contains(&page, "Default address");
	assert_contains(&page, "web.127-0-0-1.sslip.io");
	// The local test server is reached as a name, so a CNAME to it.
	assert_contains(&page, "<strong>CNAME</strong> record");
	assert_contains(&page, "localhost");

	let body = f
		.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "  App.Example.com " }),
		)
		.await;
	assert_contains(&body, "app.example.com");
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	// With no ACME email the new domain is served over plain HTTP, and says so.
	assert_contains(&body, "HTTP only");
	let body = f
		.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "not a host" }),
		)
		.await;
	assert_contains(&body, "That hostname isn't valid.");

	let id = only_id(&db, "custom_domains").await;
	let body = f.post_empty(&f.url(&format!("domains/{id}/delete"))).await;
	assert_not_contains(&body, "app.example.com");
	assert_contains(&body, "No custom domains yet.");
	assert_eq!(count(&db, "custom_domains").await, 0);
}

/// Was `a_taken_hostname_is_a_conflict_shown_inline`.
#[db_test]
async fn a_taken_hostname_is_a_conflict_shown_inline(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.post(
		&f.url("domains"),
		&json!({ "domain_hostname": "a.example.com" }),
	)
	.await;
	let body = f
		.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "A.example.com" }),
		)
		.await;
	assert_contains(&body, "already routed to another application");
	assert_not_contains(&body, "Saved.");
	assert_eq!(count(&db, "custom_domains").await, 1);
}

/// Was `the_https_banner_follows_the_acme_email`.
#[db_test]
async fn the_https_banner_follows_the_acme_email(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.post(
		&f.url("domains"),
		&json!({ "domain_hostname": "a.example.com" }),
	)
	.await;
	let page = f.page("domains").await;
	assert_contains(&page, "HTTPS is off for custom domains.");
	assert_contains(&page, "HTTP only");

	let mut state = state(db.clone());
	state.config = std::sync::Arc::new(crate::state::Config {
		acme_email: Some("ops@example.com".to_owned()),
		..(*state.config).clone()
	});
	let app = crate::routes::routes(state).await.unwrap();
	let (s, _, page) = get_html(&app, &f.url("domains"), Some(&f.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "HTTPS is off");
	assert_not_contains(&page, "HTTP only");
	assert_contains(&page, "a.example.com");
	assert_contains(&page, "HTTPS");
	assert_contains(&page, "DNS setup");
}

// --- volumes ----------------------------------------------------------------------------------------

/// Was `volumes_are_added_and_a_duplicate_path_is_a_conflict`.
#[db_test]
async fn volumes_are_added_and_a_duplicate_path_is_a_conflict(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("volumes").await;
	assert_contains(&page, "No volumes yet.");

	let body = f
		.post(&f.url("volumes"), &json!({ "volume_path": " /var/data " }))
		.await;
	assert_contains(&body, "/var/data");
	assert_contains(&body, "podkit-vol-");
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	let body = f
		.post(&f.url("volumes"), &json!({ "volume_path": "/var/data" }))
		.await;
	assert_contains(&body, "a volume is already mounted at that path");
	assert_not_contains(&body, "Saved.");
	let body = f
		.post(&f.url("volumes"), &json!({ "volume_path": "relative" }))
		.await;
	assert_contains(&body, "absolute path");
	assert_eq!(count(&db, "volumes").await, 1);
}

/// Was `deleting_a_volume_asks_first_and_survives_an_unreachable_agent`.
#[db_test]
async fn deleting_a_volume_asks_first_and_survives_an_unreachable_agent(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.post(&f.url("volumes"), &json!({ "volume_path": "/data" }))
		.await;
	let page = f.page("volumes").await;
	let id = only_id(&db, "volumes").await;
	// The confirmation is a dialog of its own, named for the volume it deletes.
	assert_contains(&page, &format!("id=\"volume-delete-{id}\""));
	assert_contains(&page, &format!("commandfor=\"volume-delete-{id}\""));
	assert_contains(&page, "permanently deletes all data in /data");
	assert_contains(&page, "can't be recovered");
	assert_contains(&page, &format!("/volumes/{id}/delete"));

	// No agent runs in tests: the failure is reported, the row stays.
	let body = f.post_empty(&f.url(&format!("volumes/{id}/delete"))).await;
	assert_not_contains(&body, "Saved.");
	assert_contains(&body, "_error");
	assert_eq!(count(&db, "volumes").await, 1);
}

// --- scope, permissions and escaping of the configuration tabs ---------------------------------------

/// Was `another_teams_app_is_404_on_every_tab_and_action`.
#[db_test]
async fn another_teams_app_is_404_on_every_tab_and_action(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.set_env("KEEP", "v").await;
	f.post(
		&f.url("domains"),
		&json!({ "domain_hostname": "keep.example.com" }),
	)
	.await;
	f.post(&f.url("volumes"), &json!({ "volume_path": "/keep" }))
		.await;
	let (_, bob_team) = ui_session(&f.app, "bob").await;
	let bob = ui_login(&f.app, "bob").await;

	// Alice's team, and Bob's own team with Alice's ids.
	for team in [f.team, bob_team] {
		for tab in ["env", "domains", "volumes"] {
			let uri = format!(
				"/teams/{team}/projects/{}/apps/{}/{tab}",
				f.project, f.application
			);
			let (s, _, body) = get_html(&f.app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_contains(&body, "Not found");
			assert_not_contains(&body, "KEEP");
			assert_not_contains(&body, "keep.example.com");
			assert_not_contains(&body, "/keep");

			let (_, _, body) = post_signals(
				&f.app,
				&uri,
				Some(&bob),
				&json!({
					"env_key": "EVIL", "env_value": "x", "domain_hostname": "evil.example.com",
					"volume_path": "/evil",
				}),
			)
			.await;
			assert_contains(&body, "That no longer exists.");
		}
		let uri = format!(
			"/teams/{team}/projects/{}/apps/{}/env/bulk",
			f.project, f.application
		);
		let (_, _, body) =
			post_signals(&f.app, &uri, Some(&bob), &json!({ "env_bulk": "EVIL=1" })).await;
		assert_contains(&body, "That no longer exists.");
	}
	assert_eq!(count(&db, "env_vars").await, 1);
	assert_eq!(count(&db, "custom_domains").await, 1);
	assert_eq!(count(&db, "volumes").await, 1);
}

/// Was `each_resource_is_authorized_on_its_own`.
#[db_test]
async fn each_resource_is_authorized_on_its_own(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	f.set_env("KEEP", "v").await;
	f.post(
		&f.url("domains"),
		&json!({ "domain_hostname": "keep.example.com" }),
	)
	.await;
	f.post(&f.url("volumes"), &json!({ "volume_path": "/keep" }))
		.await;
	let carol = f.member("carol").await;
	let env_id = only_id(&db, "env_vars").await;
	let domain_id = only_id(&db, "custom_domains").await;
	let volume_id = only_id(&db, "volumes").await;

	// A read-only member sees each tab, without its forms or buttons.
	for (tab, hidden, delete) in [
		("env", "Save variable", format!("env/{env_id}/delete")),
		(
			"domains",
			"Add domain",
			format!("domains/{domain_id}/delete"),
		),
		(
			"volumes",
			"Add volume",
			format!("volumes/{volume_id}/delete"),
		),
	] {
		let (s, _, page) = get_html(&f.app, &f.url(tab), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK, "{tab}");
		assert_not_contains(&page, hidden);
		assert_not_contains(&page, &delete);
		assert_not_contains(&page, "/delete");
	}
	// Forged posts are refused, for each resource.
	for (uri, signals) in [
		(f.url("env"), json!({ "env_key": "X", "env_value": "1" })),
		(f.url("env/bulk"), json!({ "env_bulk": "X=1" })),
		(f.url(&format!("env/{env_id}/delete")), json!({})),
		(
			f.url("domains"),
			json!({ "domain_hostname": "x.example.com" }),
		),
		(f.url("volumes"), json!({ "volume_path": "/x" })),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
		assert!(body.contains("permission"), "{uri} not refused: {body}");
	}
	assert_eq!(count(&db, "env_vars").await, 1);
	assert_eq!(count(&db, "custom_domains").await, 1);
	assert_eq!(count(&db, "volumes").await, 1);

	// Taking away one read permission closes that tab only.
	revoke_permission(&db, 2, "read", "env_var").await;
	let (s, _, page) = get_html(&f.app, &f.url("env"), Some(&carol)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_not_contains(&page, "KEEP");
	let (s, _, page) = get_html(&f.app, &f.url("domains"), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "/env\"");
	assert_contains(&page, "/domains\"");
	let (s, _, _) = get_html(&f.app, &f.url("volumes"), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);

	// And a missing create on the owner's role blocks that resource alone.
	revoke_permission(&db, 1, "create", "domain").await;
	let body = f
		.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "y.example.com" }),
		)
		.await;
	assert_contains(&body, "permission");
	let body = f.set_env("STILL_OK", "1").await;
	assert_contains(&body, "STILL_OK");
	let body = f
		.post(&f.url("volumes"), &json!({ "volume_path": "/still-ok" }))
		.await;
	assert_contains(&body, "/still-ok");
	revoke_permission(&db, 1, "delete", "volume").await;
	let page = f.page("volumes").await;
	assert_not_contains(&page, &format!("volumes/{volume_id}/delete"));
	assert_not_contains(&page, "volume-delete-");
	let page = f.page("env").await;
	assert_contains(&page, &format!("env/{env_id}/delete"));
}

/// Was `names_are_escaped_on_the_config_tabs`.
#[db_test]
async fn names_are_escaped_on_the_config_tabs(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let name = "<script>alert(1)</script>";
	rename_applications(&db, name).await;
	for tab in ["env", "domains", "volumes", "logs", "deployments", "models"] {
		let page = f.page(tab).await;
		assert_not_contains(&page, name);
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}
}

// --- models and webhooks -------------------------------------------------------------------------------

#[db_test]
async fn the_models_tab_says_when_no_model_is_linked(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("models").await;
	assert_contains(&page, "No model is linked to this application.");
	assert_contains(&page, "Browse the model catalog");
	assert_contains(
		&page,
		&format!("/teams/{}/models/catalog?project={}", f.team, f.project),
	);

	// Taking away read model closes the tab, and its page.
	let carol = f.member("carol").await;
	let (s, _, page) = get_html(&f.app, &f.url("models"), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "Browse the model catalog");
	revoke_permission(&db, 2, "read", "model").await;
	let (s, _, page) = get_html(&f.app, &f.url("models"), Some(&carol)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_contains(&page, "permission");
	let (_, _, page) = get_html(&f.app, &f.url(""), Some(&carol)).await;
	assert_not_contains(page_main(&page), &format!("{}/models\"", f.url("")));
}

#[db_test]
async fn the_webhook_secret_is_shown_once_when_rotated_and_the_old_one_stops_working(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let before = webhook_secret_blob(&db, "web").await;
	let page = f.page("webhooks").await;
	assert_contains(&page, "Webhook URL");
	assert_contains(
		&page,
		&format!("http://podkit.test/webhooks/deploy/{}", f.application),
	);
	assert_contains(&page, "Rotate secret");
	assert_contains(&page, "id=\"webhook-rotate\"");
	assert_contains(&page, "X-Gitlab-Token");
	assert_eq!(webhook_secret_blob(&db, "web").await, before);

	let body = f.post_empty(&f.url("webhooks/rotate")).await;
	let secret = shown_value(&body, "New webhook secret");
	assert_eq!(secret.len(), 48, "{secret}");
	assert_contains(&body, "only this once");
	assert_contains(&body, "\"_close\":\"webhook-rotate\"");
	assert_ne!(webhook_secret_blob(&db, "web").await, before);
	// Stored encrypted, and never on a page afterwards.
	assert!(!String::from_utf8_lossy(&webhook_secret_blob(&db, "web").await).contains(&secret));
	for tab in ["", "webhooks", "advanced"] {
		assert_not_contains(&f.page(tab).await, &secret);
	}
	// Rotating again gives another one.
	let again = f.post_empty(&f.url("webhooks/rotate")).await;
	assert_ne!(shown_value(&again, "New webhook secret"), secret);
}

// --- the health gate ----------------------------------------------------------------------------

/// The Health section's fields, as the form sends them.
fn health_form(extra: Value) -> Value {
	let mut form = json!({
		"hc_mode": "http", "hc_path": "/up", "hc_status": "204", "hc_command": "",
		"hc_interval": "2", "hc_timeout": "1", "hc_retries": "4", "hc_start": "7",
		"hc_wait": "45", "hc_drain": "9", "hc_window": "10", "hc_after": "2",
	});
	merge(&mut form, extra);
	form
}

#[db_test]
async fn health_gate_section_is_on_the_general_tab_and_prefilled(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let page = f.page("settings").await;
	assert_contains(&page, "Health");
	assert_contains(&page, "Wait for the new container");
	assert_contains(&page, "&quot;hc_mode&quot;:&quot;off&quot;");
	// The save bar and its dirty dialog watch the new fields like the old ones.
	assert_contains(&page, "$hc_mode !== $_initial.hc_mode");
	assert_contains(&page, "$hc_after !== $_initial.hc_after");
	// Numbers are text inputs with a numeric keyboard, never type=number.
	assert_contains(&page, "inputmode=\"numeric\"");
	assert_not_contains(&page, "type=\"number\"");
	assert_eq!(page.matches("<h1").count(), 1);
}

#[db_test]
async fn health_gate_saving_stores_the_settings_and_the_page_shows_them_back(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let uri = f.url("settings");
	let mut form = f.form("web", json!({}));
	merge(&mut form, health_form(json!({})));
	assert_contains(&f.post(&uri, &form).await, "Settings saved");
	assert_eq!(
		health::settings(&db, f.application).await,
		Some(("http".into(), "/up".into(), 204, 2, 10))
	);
	let page = f.page("settings").await;
	assert_contains(&page, "&quot;hc_mode&quot;:&quot;http&quot;");
	assert_contains(&page, "&quot;hc_path&quot;:&quot;/up&quot;");
	assert_contains(&page, "&quot;hc_wait&quot;:&quot;45&quot;");

	// A command check, then off again: the row goes.
	let mut form = f.form("web", json!({}));
	merge(
		&mut form,
		health_form(json!({ "hc_mode": "command", "hc_command": "pg_isready -q" })),
	);
	assert_contains(&f.post(&uri, &form).await, "Settings saved");
	assert_eq!(
		health::settings(&db, f.application).await.unwrap().0,
		"command"
	);
	let mut form = f.form("web", json!({}));
	merge(&mut form, health_form(json!({ "hc_mode": "off" })));
	assert_contains(&f.post(&uri, &form).await, "Settings saved");
	assert_eq!(health::settings(&db, f.application).await, None);
}

#[db_test]
async fn health_gate_a_bad_number_refuses_the_whole_save(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let uri = f.url("settings");
	for (extra, message) in [
		(
			json!({ "hc_interval": "abc" }),
			"interval must be a whole number",
		),
		(json!({ "hc_path": "health" }), "path must start with /"),
		(json!({ "hc_retries": "99" }), "retries must be between"),
		(json!({ "hc_wait": "1" }), "deploy wait must be between"),
	] {
		let mut form = f.form("web", json!({ "name": "Renamed" }));
		merge(&mut form, health_form(extra));
		let body = f.post(&uri, &form).await;
		assert_contains(&body, message);
		assert_not_contains(&body, "Settings saved");
	}
	// Nothing was stored, not even the name that was fine.
	assert_eq!(health::settings(&db, f.application).await, None);
	assert_not_contains(&f.page("settings").await, "Renamed");
}

#[db_test]
async fn health_gate_a_reader_sees_text_and_a_forged_post_changes_nothing(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let mut form = f.form("web", json!({}));
	merge(
		&mut form,
		health_form(json!({ "hc_command": "<script>alert(1)</script>", "hc_mode": "command" })),
	);
	f.post(&f.url("settings"), &form).await;
	let carol = f.member("carol").await;

	let (_, _, page) = get_html(&f.app, &f.url("settings"), Some(&carol)).await;
	let main = page_main(&page);
	assert_contains(main, "Health");
	assert_contains(main, "Automatic rollback");
	assert_not_contains(main, "Unsaved changes");
	// A command is data: escaped, never markup.
	assert_not_contains(main, "<script>alert(1)</script>");
	assert_contains(main, "&lt;script&gt;alert(1)&lt;/script&gt;");

	let mut forged = f.form("web", json!({}));
	merge(&mut forged, health_form(json!({ "hc_mode": "off" })));
	let (_, _, body) = post_signals(&f.app, &f.url("settings"), Some(&carol), &forged).await;
	assert_contains(&body, "You don't have permission to do that.");
	assert_eq!(
		health::settings(&db, f.application).await.unwrap().0,
		"command"
	);
}

#[db_test]
async fn health_gate_another_team_cannot_reach_or_change_the_settings(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	let mut form = f.form("web", json!({}));
	merge(&mut form, health_form(json!({})));
	f.post(&f.url("settings"), &form).await;
	let (bob_cookie, bob_team) = ui_session(&f.app, "bob").await;
	let theirs = format!(
		"/teams/{bob_team}/projects/{}/apps/{}/settings",
		f.project, f.application
	);
	let (s, _, body) = get_html(&f.app, &theirs, Some(&bob_cookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, "/up");
	let mut forged = f.form("web", json!({}));
	merge(&mut forged, health_form(json!({ "hc_mode": "off" })));
	let (_, _, body) = post_signals(&f.app, &theirs, Some(&bob_cookie), &forged).await;
	assert_contains(&body, "That no longer exists.");
	assert_eq!(
		health::settings(&db, f.application).await.unwrap().0,
		"http"
	);
}

#[db_test]
async fn health_gate_the_timeline_and_header_say_what_the_gate_did(db: PgPool) {
	let f = Fixture::with_web(&db).await;
	// An old release, the bad one that was rolled back, the rollback itself (live), and a
	// deploy that is waiting for its check right now.
	f.seed(200, "stopped", Some("img:1"), Some("c1")).await;
	f.seed(201, "stopped", Some("img:2"), Some("c2")).await;
	f.seed(202, "running", Some("img:1"), Some("c3")).await;
	health::seed_gate(
		&db,
		201,
		"rolled_back",
		Some("the release crashed 40s after it went live"),
		None,
	)
	.await;
	health::seed_gate(&db, 202, "healthy", None, Some(201)).await;
	let page = f.page("").await;
	assert_contains(&page, "Rolled back");
	assert_contains(&page, "the release crashed 40s after it went live");
	assert_contains(&page, "Rolled back automatically");
	assert_contains(&page, "Replaced #201, which went bad");
	// The header says the live release is the previous image put back.
	assert_contains(&page, "Rolled back from #201");

	f.seed(203, "deploying", None, None).await;
	health::seed_gate(&db, 203, "waiting", None, None).await;
	let page = f.page("").await;
	assert_contains(&page, "Waiting for health\u{2026}");

	f.seed(204, "failed", None, None).await;
	set_deployment_error(
		&db,
		204,
		"the new container did not become healthy: <b>x</b>",
	)
	.await;
	health::seed_gate(&db, 204, "unhealthy", None, None).await;
	let page = f.page("").await;
	assert_contains(
		&page,
		"Never became healthy; the previous container kept serving",
	);
	assert_not_contains(&page, "<b>x</b>");
}
