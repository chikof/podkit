//! The Git providers page and the provider source of the new-application form: what they
//! show, what they never show, and who may act.

use axum::http::{StatusCode, header};
use serde_json::json;

use crate::git_providers::stubs::{self, GITEA_TOKEN};
use crate::state::AppState;
use crate::tests::{
	PgPool, application_id, count, db_test, local_server_id, project_id, state, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};

struct World {
	state: AppState,
	app: axum::Router,
	cookie: String,
	team: i64,
}

async fn world(db: &PgPool) -> World {
	let state = state(db.clone());
	let app = crate::routes::routes(state.clone()).await.unwrap();
	let (cookie, team) = ui_session(&app, "alice").await;
	World {
		state,
		app,
		cookie,
		team,
	}
}

impl World {
	fn url(&self, rest: &str) -> String {
		format!("/teams/{}/git-providers{rest}", self.team)
	}

	/// Connects a Gitea at the stub through the page's own action.
	async fn connect_gitea(&self, name: &str, stub: &str) -> String {
		let (s, _, body) = post_signals(
			&self.app,
			&self.url("/gitea"),
			Some(&self.cookie),
			&json!({ "ge_name": name, "ge_url": stub, "ge_token": GITEA_TOKEN }),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{body}");
		assert_not_contains(&body, "_error\":\"");
		body
	}

	async fn provider_id(&self, db: &PgPool, name: &str) -> i64 {
		crate::tests::git_providers::provider_id(db, name).await
	}

	/// A project and its first server, for the create form.
	async fn project(&self, db: &PgPool) -> (i64, i64) {
		post_signals(
			&self.app,
			&format!("/teams/{}/projects", self.team),
			Some(&self.cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		(
			project_id(db, "shop").await,
			local_server_id(db, self.team).await,
		)
	}
}

/// What follows `marker` up to the next quote.
fn after<'a>(body: &'a str, marker: &str) -> &'a str {
	let rest = &body[body.find(marker).expect(marker) + marker.len()..];
	&rest[..rest.find('"').unwrap_or(rest.len())]
}

#[db_test]
async fn git_providers_page_lists_providers_without_secrets_and_escapes_names(db: PgPool) {
	let gitea = stubs::gitea().await;
	let w = world(&db).await;

	let (s, _, empty) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&empty, "<h1>Git providers</h1>");
	assert_contains(&empty, "No git providers yet.");
	assert_contains(&empty, "podkit has no public address.");
	// Reachable from the sidebar and the palette.
	assert_contains(&empty, &format!("/teams/{}/git-providers", w.team));

	w.connect_gitea("<script>alert(1)</script>", &gitea.url)
		.await;
	let (s, _, page) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(main, "<script>alert(1)");
	assert_contains(main, "Gitea");
	assert_contains(main, "127.0.0.1:");
	assert_contains(main, "Not used");
	assert_eq!(page.matches("<h1").count(), 1);
	// The token is write-only: not on the page, not in any signal.
	assert_not_contains(&page, GITEA_TOKEN);
	assert_contains(&page, "ge_token");
	assert_eq!(count(&db, "git_providers").await, 1);

	// A wrong token is refused with the reason, and nothing more is stored.
	let (s, _, body) = post_signals(
		&w.app,
		&w.url("/gitea"),
		Some(&w.cookie),
		&json!({ "ge_name": "bad", "ge_url": gitea.url, "ge_token": "nope" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "rejected the credentials");
	assert_eq!(count(&db, "git_providers").await, 1);
}

#[db_test]
async fn git_providers_forged_posts_and_other_teams_are_refused(db: PgPool) {
	let gitea = stubs::gitea().await;
	let w = world(&db).await;
	w.connect_gitea("home", &gitea.url).await;
	let id = w.provider_id(&db, "home").await;

	let (_, m_team) = user_with_team(&w.app, "mallory").await;
	let mallory = ui_login(&w.app, "mallory").await;

	// Her own team's page shows nothing of alice's.
	let (s, _, page) = get_html(
		&w.app,
		&format!("/teams/{m_team}/git-providers"),
		Some(&mallory),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(page_main(&page), "No git providers yet.");
	assert_not_contains(page_main(&page), "provider-list");
	// Alice's page is not hers: the 404 page.
	let (s, _, _) = get_html(&w.app, &w.url(""), Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// Forged actions, in alice's team and through her own team with alice's provider id.
	for (path, body) in [
		(
			w.url("/gitea"),
			json!({ "ge_name": "evil", "ge_url": gitea.url, "ge_token": GITEA_TOKEN }),
		),
		(w.url("/github/start"), json!({ "gh_name": "evil" })),
		(w.url(&format!("/{id}/delete")), json!({})),
		(w.url(&format!("/{id}/test")), json!({})),
		(
			format!("/teams/{m_team}/git-providers/{id}/delete"),
			json!({}),
		),
		(
			format!("/teams/{m_team}/git-providers/{id}/test"),
			json!({}),
		),
	] {
		let (s, _, out) = post_signals(&w.app, &path, Some(&mallory), &body).await;
		assert!(
			matches!(
				s,
				StatusCode::OK | StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
			),
			"{path}: {s}"
		);
		assert_not_contains(&out, "Connected as");
	}
	for path in [
		format!("/teams/{m_team}/git-providers/{id}/repos"),
		format!("/teams/{m_team}/git-providers/{id}/branches?repo=ops/app-01"),
	] {
		let (s, _, out) = get_html(&w.app, &path, Some(&mallory)).await;
		// Not hers: the list says nothing of it, whatever the status.
		assert!(s.is_success() || s.is_client_error(), "{path}: {s}");
		assert_not_contains(&out, "ops/app-0");
	}
	assert_eq!(
		count(&db, "git_providers").await,
		1,
		"nothing made, nothing lost"
	);

	// Signed out: sent to sign in.
	let (s, h, _) = get_html(&w.app, &w.url(""), None).await;
	assert!(s.is_redirection(), "{s}");
	assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login"));
}

#[db_test]
async fn git_providers_test_and_delete_actions(db: PgPool) {
	let gitea = stubs::gitea().await;
	let w = world(&db).await;
	w.connect_gitea("home", &gitea.url).await;
	let id = w.provider_id(&db, "home").await;

	let (s, _, body) = post_signals(
		&w.app,
		&w.url(&format!("/{id}/test")),
		Some(&w.cookie),
		&json!({}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "Connected as ops");

	// Delete is refused while an application uses it, with the names, then goes through.
	let (project, server) = w.project(&db).await;
	let (s, _, body) = post_signals(
		&w.app,
		&format!("/teams/{}/projects/{project}/apps", w.team),
		Some(&w.cookie),
		&json!({
			"name": "Api", "slug": "api", "server_id": server.to_string(), "source": "provider",
			"git_provider_id": id.to_string(), "git_repo": "ops/app-02", "git_ref": "master",
			"build_strategy": "dockerfile", "dockerfile_path": "Dockerfile", "container_port": "80",
		}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&db, "applications").await, 1, "{body}");
	let (_, _, body) = post_signals(
		&w.app,
		&w.url(&format!("/{id}/delete")),
		Some(&w.cookie),
		&json!({}),
	)
	.await;
	assert_contains(&body, "_delete_error");
	assert_contains(&body, "in use by Api");
	assert_eq!(count(&db, "git_providers").await, 1);
	let (_, _, page) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	assert_contains(page_main(&page), ">Api</a>");

	crate::tests::git_providers::delete_applications(&db).await;
	let (_, h, _) = post_signals(
		&w.app,
		&w.url(&format!("/{id}/delete")),
		Some(&w.cookie),
		&json!({}),
	)
	.await;
	assert_eq!(count(&db, "git_providers").await, 0);
	assert!(h.contains_key(header::SET_COOKIE), "a flash says it went");
}

#[db_test]
async fn git_providers_github_flow_in_the_browser_is_bound_to_the_session(db: PgPool) {
	let gh = stubs::github().await;
	let w = world(&db).await;

	// Step 1: the form that sends the browser to GitHub carries the manifest and a state.
	let (s, _, body) = post_signals(
		&w.app,
		&w.url("/github/start"),
		Some(&w.cookie),
		&json!({ "gh_name": "podkit-stub", "gh_org": "", "gh_url": gh.url }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "github-go-form");
	assert_contains(&body, "name=\"manifest\"");
	assert_contains(&body, "_gh_ready");
	assert_contains(
		&body,
		&format!("action=\"{}/settings/apps/new?state=", gh.url),
	);
	// The manifest names podkit's own callback, as the browser reached it.
	assert_contains(&body, "http://podkit.test/git-providers/github/callback");
	let state = after(&body, "/settings/apps/new?state=").to_owned();
	assert!(state.len() > 40, "{state}");

	// A start with no name is refused in words.
	let (_, _, bad) = post_signals(
		&w.app,
		&w.url("/github/start"),
		Some(&w.cookie),
		&json!({ "gh_name": "" }),
	)
	.await;
	assert_contains(&bad, "_error");

	// Step 2, as GitHub redirects the browser: wrong state, no state, another user.
	let callback = |state: &str, code: &str| {
		format!("/git-providers/github/callback?code={code}&state={state}")
	};
	let (_, _, out) = get_html(&w.app, &callback("garbage", "good-code"), Some(&w.cookie)).await;
	assert_contains(&out, "not valid or has expired");
	let (s, _, _) = get_html(&w.app, &callback("", ""), Some(&w.cookie)).await;
	assert!(s.is_client_error());
	let (_, _) = user_with_team(&w.app, "mallory").await;
	let mallory = ui_login(&w.app, "mallory").await;
	let (_, _, out) = get_html(&w.app, &callback(&state, "good-code"), Some(&mallory)).await;
	assert_contains(&out, "not valid or has expired");
	let (s, h, _) = get_html(&w.app, &callback(&state, "good-code"), None).await;
	assert!(s.is_redirection() && h[header::LOCATION].to_str().unwrap().starts_with("/login"));
	assert_eq!(
		count(&db, "git_providers").await,
		0,
		"none of those stored anything"
	);

	// The real thing: the owner's browser, the right state. On to GitHub's install page.
	let (s, h, _) = get_html(&w.app, &callback(&state, "good-code"), Some(&w.cookie)).await;
	assert!(s.is_redirection(), "{s}");
	let location = h[header::LOCATION].to_str().unwrap();
	assert!(
		location.starts_with(&format!(
			"{}/apps/podkit-stub/installations/new?state=",
			gh.url
		)),
		"{location}"
	);
	assert_eq!(count(&db, "git_providers").await, 1);
	let (_, _, page) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	let main = page_main(&page);
	assert_contains(main, "podkit-stub");
	assert_contains(main, "GitHub App");
	assert_contains(main, "Install on GitHub");
	for secret in ["BEGIN RSA", "whsec", "PRIVATE KEY"] {
		assert_not_contains(&page, secret);
	}

	// After the install page: back to this team's list; anyone else is sent home.
	let (s, h, _) = get_html(
		&w.app,
		&format!(
			"/git-providers/github/installed?state={state}&installation_id=7&setup_action=install"
		),
		Some(&w.cookie),
	)
	.await;
	assert!(s.is_redirection());
	assert_eq!(h[header::LOCATION], w.url("").as_str());
	let (s, h, _) = get_html(
		&w.app,
		&format!("/git-providers/github/installed?state={state}"),
		Some(&mallory),
	)
	.await;
	assert!(s.is_redirection());
	assert_eq!(h[header::LOCATION], "/");
	let _ = &w.state;
}

#[db_test]
async fn git_providers_picker_searches_on_the_server_and_escapes_what_it_shows(db: PgPool) {
	let gitea = stubs::gitea().await;
	let w = world(&db).await;
	w.connect_gitea("home", &gitea.url).await;
	let id = w.provider_id(&db, "home").await;

	let (s, _, first) = get_html(
		&w.app,
		&w.url(&format!("/{id}/repos?q=&page=1")),
		Some(&w.cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&first, "id=\"repo-results\"");
	assert_contains(&first, "ops/app-01");
	assert_contains(&first, "ui-repo-item");
	assert_contains(&first, "Page 1");
	// Choosing a repository fills the form and asks for its branches.
	assert_contains(&first, "$git_repo = ");
	assert_contains(&first, "/branches?repo=");
	assert_contains(&first, "Next");

	let (_, _, hit) = get_html(
		&w.app,
		&w.url(&format!("/{id}/repos?q=app-31")),
		Some(&w.cookie),
	)
	.await;
	assert_contains(&hit, "ops/app-31");
	assert_not_contains(&hit, "ops/app-01");
	let (_, _, none) = get_html(
		&w.app,
		&w.url(&format!("/{id}/repos?q=zzz")),
		Some(&w.cookie),
	)
	.await;
	assert_contains(&none, "No repository matches.");

	let (_, _, branches) = get_html(
		&w.app,
		&w.url(&format!("/{id}/branches?repo=ops/app-01")),
		Some(&w.cookie),
	)
	.await;
	assert_contains(&branches, "datalist id=\"branch-list\"");
	assert_contains(&branches, "value=\"master\"");
	assert_contains(&branches, "value=\"feature\"");

	// A provider that does not exist is a line in the list, not a broken page.
	let (_, _, down) = get_html(&w.app, &w.url("/4242/repos?q=x"), Some(&w.cookie)).await;
	assert_contains(&down, "That no longer exists.");
}

#[db_test]
async fn git_providers_new_application_offers_the_provider_source(db: PgPool) {
	let gitea = stubs::gitea().await;
	let w = world(&db).await;
	let (project, server) = w.project(&db).await;
	let new = format!("/teams/{}/projects/{project}/apps/new", w.team);

	// Before any provider exists, the tab says where to connect one.
	let (_, _, none) = get_html(&w.app, &format!("{new}?source=provider"), Some(&w.cookie)).await;
	assert_contains(page_main(&none), "Git provider");
	assert_contains(page_main(&none), "No git provider is connected yet.");

	w.connect_gitea("home", &gitea.url).await;
	let id = w.provider_id(&db, "home").await;
	let (s, _, page) = get_html(&w.app, &format!("{new}?source=provider"), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "Git repository");
	assert_contains(main, "Docker image");
	assert_contains(main, &format!("value=\"{id}\""));
	assert_contains(main, "home \u{b7} Gitea");
	assert_contains(main, "id=\"repo-results\"");
	assert_contains(main, "list=\"branch-list\"");
	assert_contains(main, "&quot;source&quot;:&quot;provider&quot;");
	// No token or deploy key to type for a provider.
	assert_not_contains(main, "Access token");
	assert_not_contains(main, "Deploy key");
	assert_eq!(page.matches("<h1").count(), 1);
	// The plain URL source keeps its own fields.
	let (_, _, git) = get_html(&w.app, &format!("{new}?source=git"), Some(&w.cookie)).await;
	assert_contains(page_main(&git), "Repository URL");

	// Creating: the application is the provider's, with no credential of its own.
	let (_, _, created) = post_signals(
		&w.app,
		&format!("/teams/{}/projects/{project}/apps", w.team),
		Some(&w.cookie),
		&json!({
			"name": "Api", "slug": "api", "server_id": server.to_string(), "source": "provider",
			"git_provider_id": id.to_string(), "git_repo": "ops/app-02", "git_ref": "master",
			"build_strategy": "dockerfile", "dockerfile_path": "Dockerfile", "container_port": "80",
		}),
	)
	.await;
	assert_contains(&created, "Application created");
	// No public address here, so the page says the webhook is by hand.
	assert_contains(&created, "no public address");
	let row = crate::tests::git_providers::app_provider(&db, "api").await;
	assert_eq!(row.0, Some(id));
	assert_eq!(row.1.as_deref(), Some("ops/app-02"));
	assert_eq!(row.2.unwrap(), format!("{}/ops/app-02.git", gitea.url));

	// A form that names no repository, or a provider of someone else, is refused in words.
	for extra in [
		json!({ "git_repo": "" }),
		json!({ "git_provider_id": "99" }),
	] {
		let mut form = json!({
			"name": "Two", "slug": "two", "server_id": server.to_string(), "source": "provider",
			"git_provider_id": id.to_string(), "git_repo": "ops/app-03", "git_ref": "master",
			"container_port": "80",
		});
		for (k, v) in extra.as_object().unwrap() {
			form[k] = v.clone();
		}
		let (_, _, out) = post_signals(
			&w.app,
			&format!("/teams/{}/projects/{project}/apps", w.team),
			Some(&w.cookie),
			&form,
		)
		.await;
		assert_contains(&out, "_error");
	}
	assert_eq!(count(&db, "applications").await, 1);

	// Its settings: the repository is shown, not edited, and there is no credential field.
	let app_id = application_id(&db, "api").await;
	let (s, _, settings) = get_html(
		&w.app,
		&format!(
			"/teams/{}/projects/{project}/apps/{app_id}/settings",
			w.team
		),
		Some(&w.cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&settings);
	assert_contains(main, "ops/app-02");
	assert_contains(main, "Cloned through");
	assert_contains(main, "home");
	assert_not_contains(main, "Access token");
	assert_not_contains(main, "Repository URL");
	assert_not_contains(&settings, GITEA_TOKEN);
	let _ = &w.state;
}
