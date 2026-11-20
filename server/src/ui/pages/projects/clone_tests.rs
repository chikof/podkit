//! The clone dialog, its preview, the clone request and the clone page: what the owner
//! sees, what a person without `create project` does not, forged posts, escaping, secrets
//! and tenancy. Names start with `clone_env_ui` so `cargo test -p server -- clone_env`
//! runs them with the service tests.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::environments::clone::{CloneStatus, find_by_id, save_report};
use crate::ids::{ApplicationId, CloneId};
use crate::tests::grid::{env_id, only_clone_id};
use crate::tests::{
	PgPool, app, count, db_test, member_role, project_id, rename_applications, revoke_permission,
	seed_app_and_database,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, ui_session,
};

const SECRET: &str = "S3CR3T-5e1a0c52-4b0f-4c53-8a0f-1f5a6b7c8d9e";

struct Ui {
	app: Router,
	cookie: String,
	team: i64,
	project: i64,
	env: i64,
	db: PgPool,
}

impl Ui {
	async fn new(db: PgPool) -> Self {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let (status, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Web", "slug": "web" }),
		)
		.await;
		assert!(status.is_success(), "{status} {body}");
		let project = project_id(&db, "web").await;
		seed_app_and_database(&db, team, project).await;
		crate::env_vars::upsert(&db, ApplicationId(5001), "API_KEY", SECRET.as_bytes())
			.await
			.unwrap();
		Self {
			env: env_id(&db, project, "production").await,
			app,
			cookie,
			team,
			project,
			db,
		}
	}

	fn base(&self) -> String {
		format!("/teams/{}/projects/{}", self.team, self.project)
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	async fn post(&self, uri: &str, signals: &Value) -> (StatusCode, String) {
		let (s, _, body) = post_signals(&self.app, uri, Some(&self.cookie), signals).await;
		(s, body)
	}

	fn signals(&self, name: &str, slug: &str) -> Value {
		json!({
			"envId": self.env.to_string(),
			"cloneName": name,
			"cloneSlug": slug,
			"cloneBlank": false,
			"cloneData": false,
			"cloneTarget": "",
			"cloneDeploy": false,
		})
	}

	async fn preview(&self, name: &str, slug: &str) -> String {
		let (status, body) = self
			.post(
				&format!("{}/environments/clone/preview", self.base()),
				&self.signals(name, slug),
			)
			.await;
		assert_eq!(status, StatusCode::OK, "{body}");
		body
	}

	async fn start(&self, name: &str, slug: &str) -> String {
		self.post(
			&format!("{}/environments/clone", self.base()),
			&self.signals(name, slug),
		)
		.await
		.1
	}
}

fn h1_count(page: &str) -> usize {
	page.matches("<h1").count()
}

#[db_test]
async fn clone_env_ui_the_environment_menu_offers_the_dialog(db: PgPool) {
	let ui = Ui::new(db).await;
	let (status, page) = ui.get(&format!("{}/production", ui.base())).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&page, "Clone environment");
	assert_contains(&page, "id=\"environment-clone\"");
	assert_contains(&page, "Leave secret values blank");
	assert_contains(&page, "Copy the data too");
	assert_contains(&page, "Deploy after cloning");
	assert_contains(&page, "Name the environment to see what will be created.");
	assert_contains(&page, "cloneName");
	assert_eq!(h1_count(&page), 1, "one h1 per page");
	// The dialog is native and light-dismissed like the others, with no role of its own.
	assert_contains(&page, "commandfor=\"environment-clone\"");
	assert_not_contains(&page, "aria-modal");
}

#[db_test]
async fn clone_env_ui_without_create_project_there_is_no_clone_item(db: PgPool) {
	let ui = Ui::new(db).await;
	let role = member_role(&ui.db, ui.team, "alice").await;
	revoke_permission(&ui.db, role, "create", "project").await;
	let (_, page) = ui.get(&format!("{}/production", ui.base())).await;
	assert_not_contains(&page, "Clone environment");
	assert_not_contains(&page, "id=\"environment-clone\"");
}

#[db_test]
async fn clone_env_ui_preview_shows_the_names_and_domains_it_will_make(db: PgPool) {
	let ui = Ui::new(db).await;
	let body = ui.preview("Staging", "staging").await;
	assert_contains(&body, "id=\"clone-preview\"");
	assert_contains(&body, "Storefront-staging");
	assert_contains(&body, "storefront-staging.127-0-0-1.sslip.io");
	assert_contains(&body, "orders-staging");
	assert_contains(&body, "new credentials, no public port");
	assert_contains(&body, "aria-live");
	assert_not_contains(&body, SECRET);
}

#[db_test]
async fn clone_env_ui_preview_without_a_name_is_a_hint_not_an_error(db: PgPool) {
	let ui = Ui::new(db).await;
	let body = ui.preview("", "").await;
	assert_contains(&body, "Name the environment to see what will be created.");
	let body = ui.preview("Staging", "Not A Slug").await;
	assert_contains(&body, "Slug must be");
}

#[db_test]
async fn clone_env_ui_preview_names_what_is_taken(db: PgPool) {
	let ui = Ui::new(db).await;
	let body = ui.preview("Prod again", "production").await;
	assert_contains(&body, "ui-clone-conflicts");
	assert_contains(&body, "already exists");
}

#[db_test]
async fn clone_env_ui_preview_escapes_resource_names(db: PgPool) {
	let ui = Ui::new(db).await;
	rename_applications(&ui.db, "<img src=x onerror=alert(1)>").await;
	let body = ui.preview("Staging", "staging").await;
	assert_not_contains(&body, "<img src=x");
	assert_contains(&body, "&lt;img src=x");
}

#[db_test]
async fn clone_env_ui_start_makes_the_environment_and_opens_the_clone_page(db: PgPool) {
	let ui = Ui::new(db).await;
	let environments = count(&ui.db, "environments").await;
	let body = ui.start("Staging", "staging").await;
	assert_contains(&body, "window.location.assign");
	assert_contains(&body, "/environments/clones/");
	assert_eq!(count(&ui.db, "environments").await, environments + 1);
	assert_eq!(count(&ui.db, "environment_clones").await, 1);

	let id = only_clone_id(&ui.db).await;
	let (status, page) = ui
		.get(&format!("{}/environments/clones/{id}", ui.base()))
		.await;
	assert_eq!(status, StatusCode::OK, "{page}");
	assert_contains(&page, "Start database orders-staging");
	assert_contains(&page, "Queued");
	assert_contains(&page, "Clone log");
	assert_eq!(h1_count(&page), 1);
	assert_not_contains(&page, SECRET);
	// While it runs the region polls itself.
	assert_contains(&page, "data-on-interval");
}

#[db_test]
async fn clone_env_ui_progress_polls_until_the_clone_is_final(db: PgPool) {
	let ui = Ui::new(db).await;
	ui.start("Staging", "staging").await;
	let id = only_clone_id(&ui.db).await;
	let url = format!("{}/environments/clones/{id}/progress", ui.base());
	let (_, running) = ui.get(&url).await;
	assert_contains(&running, "id=\"clone-progress\"");
	assert_contains(&running, "data-on-interval");

	let mut clone = find_by_id(&ui.db, CloneId(id)).await.unwrap().unwrap();
	clone.report.0[0].status = crate::environments::clone::StepStatus::Failed;
	clone.report.0[0].detail = Some("image pull failed".to_owned());
	save_report(&ui.db, clone.id, CloneStatus::Partial, &clone.report.0)
		.await
		.unwrap();
	let (_, done) = ui.get(&url).await;
	assert_not_contains(&done, "data-on-interval");
	assert_contains(&done, "Done with problems");
	assert_contains(&done, "image pull failed");
	assert_contains(&done, "Failed: ");
	assert_contains(&done, "exist");
}

#[db_test]
async fn clone_env_ui_a_forged_start_without_permission_changes_nothing(db: PgPool) {
	let ui = Ui::new(db).await;
	let role = member_role(&ui.db, ui.team, "alice").await;
	revoke_permission(&ui.db, role, "create", "database").await;
	let environments = count(&ui.db, "environments").await;
	let body = ui.start("Staging", "staging").await;
	assert_not_contains(&body, "window.location.assign");
	assert_eq!(count(&ui.db, "environments").await, environments);
	assert_eq!(count(&ui.db, "environment_clones").await, 0);
}

#[db_test]
async fn clone_env_ui_another_teams_clone_and_environment_are_not_found(db: PgPool) {
	let ui = Ui::new(db).await;
	ui.start("Staging", "staging").await;
	let id = only_clone_id(&ui.db).await;

	let (bob_cookie, bob_team) = ui_session(&ui.app, "bob").await;
	// Bob's own team in the path, Alice's project and clone.
	let (status, _, _) = get_html(
		&ui.app,
		&format!(
			"/teams/{bob_team}/projects/{}/environments/clones/{id}",
			ui.project
		),
		Some(&bob_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (_, _, leaked) = get_html(
		&ui.app,
		&format!(
			"/teams/{bob_team}/projects/{}/environments/clones/{id}/progress",
			ui.project
		),
		Some(&bob_cookie),
	)
	.await;
	assert_not_contains(&leaked, "clone-progress");
	assert_not_contains(&leaked, "orders-staging");

	let environments = count(&ui.db, "environments").await;
	let (_, _, body) = post_signals(
		&ui.app,
		&format!(
			"/teams/{bob_team}/projects/{}/environments/clone",
			ui.project
		),
		Some(&bob_cookie),
		&ui.signals("Staging", "bobs"),
	)
	.await;
	assert_not_contains(&body, "window.location.assign");
	assert_eq!(count(&ui.db, "environments").await, environments);
	// Alice's team path with a clone that is not hers is not found either.
	let (status, _) = ui
		.get(&format!("{}/environments/clones/{}", ui.base(), id + 1))
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}
