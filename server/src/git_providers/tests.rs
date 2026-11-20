//! Git provider tests, against stub servers for the GitHub, GitLab and Gitea APIs: the app
//! manifest exchange, JWT-signed installation tokens, repository listing, webhooks, and the
//! rules every slice follows (secrets stay secret, other teams get 404).

use std::fmt::Write as _;
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use tower::ServiceExt;

use super::api::{Target, valid_repo};
use super::flow::{self, StartInput};
use super::service::{self, NewTokenProvider};
use super::stubs::{self, APP_KEY, GITEA_TOKEN, GITHUB_WEBHOOK_SECRET, GITLAB_TOKEN};
use super::{Credentials, Kind, jwt};
use crate::error::ServerError;
use crate::ids::{GitProviderId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{call, count, state, team_role, user_id, user_with_team};

fn url(team: i64) -> String {
	format!("/api/v1/teams/{team}/git-providers")
}

/// The state and router of one test, sharing a database, with podkit "public" at a made-up
/// address when `public` so webhook creation is attempted.
async fn world(db: PgPool, public: bool) -> (AppState, axum::Router) {
	let mut st = state(db);
	if public {
		let mut config = (*st.config).clone();
		config.public_url = Some("https://podkit.test".to_owned());
		st.config = std::sync::Arc::new(config);
	}
	let router = crate::routes::routes(st.clone()).await.unwrap();
	(st, router)
}

/// Connects a GitHub App through the manifest flow against the stub, as `user` of `team`.
async fn connect_github(
	st: &AppState,
	stub_url: &str,
	user: UserId,
	team: TeamId,
) -> GitProviderId {
	let start = flow::start(
		st,
		user,
		team,
		"http://podkit.local",
		StartInput {
			name: "podkit-stub".to_owned(),
			base_url: stub_url.to_owned(),
			..Default::default()
		},
	)
	.await
	.unwrap();
	let state = start
		.action
		.split("state=")
		.nth(1)
		.expect("the form's address carries the state")
		.to_owned();
	flow::finish(st, user, "good-code", &state)
		.await
		.unwrap()
		.provider
		.id
}

fn sign(secret: &str, body: &[u8]) -> String {
	let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
	mac.update(body);
	let mut tag = String::from("sha256=");
	for b in mac.finalize().into_bytes() {
		write!(tag, "{b:02x}").unwrap();
	}
	tag
}

type Headers = Vec<(&'static str, String)>;

async fn post_raw(
	app: &axum::Router,
	uri: &str,
	headers: &Headers,
	body: &[u8],
) -> (StatusCode, Value) {
	let mut req = Request::builder().method("POST").uri(uri);
	for (k, v) in headers {
		req = req.header(*k, v.as_str());
	}
	let res = app
		.clone()
		.oneshot(req.body(Body::from(body.to_vec())).unwrap())
		.await
		.unwrap();
	let status = res.status();
	let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
		.await
		.unwrap();
	(
		status,
		serde_json::from_slice(&bytes).unwrap_or(Value::Null),
	)
}

/// A project and the id of the team's server, for creating applications.
async fn project_and_server(app: &axum::Router, token: &str, team: i64) -> (i64, i64) {
	let t = Some(token);
	let (s, project) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "{s} {project}");
	let (_, servers) = call(
		app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		None,
	)
	.await;
	(
		project["id"].as_i64().unwrap(),
		servers[0]["id"].as_i64().unwrap(),
	)
}

/// Creates an application through `provider`/`repo`; returns the response.
async fn create_app(
	app: &axum::Router,
	token: &str,
	team: i64,
	(project, server): (i64, i64),
	provider: GitProviderId,
	repo: &str,
	slug: &str,
) -> (StatusCode, Value) {
	call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects/{project}/applications"),
		Some(token),
		Some(json!({
			"name": slug, "slug": slug, "server_id": server, "git_provider_id": provider.0,
			"git_repo": repo, "git_ref": "main", "container_port": 80,
		})),
	)
	.await
}

async fn token_provider(
	st: &AppState,
	user: UserId,
	team: i64,
	kind: Kind,
	name: &str,
	base: &str,
	token: &str,
) -> service::ProviderView {
	service::create_token(
		st,
		user,
		TeamId(team),
		NewTokenProvider {
			kind,
			name: name.to_owned(),
			base_url: Some(base.to_owned()),
			token: token.to_owned(),
		},
	)
	.await
	.unwrap()
}

// ---- the JWT ------------------------------------------------------------------------------

#[test]
fn git_providers_jwt_is_rs256_ten_minutes_at_most_and_verifies() {
	use base64::Engine;
	use base64::engine::general_purpose::URL_SAFE_NO_PAD;
	use rsa::{Pkcs1v15Sign, RsaPublicKey};

	let now = 1_800_000_000;
	let token = jwt::app_token(APP_KEY, 4242, now).unwrap();
	let parts: Vec<&str> = token.split('.').collect();
	assert_eq!(parts.len(), 3);
	let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
	let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
	assert_eq!(header, json!({ "alg": "RS256", "typ": "JWT" }));
	assert_eq!(claims["iss"], "4242");
	assert!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap() <= 600);
	assert!(
		claims["iat"].as_i64().unwrap() < now,
		"back-dated for clock drift"
	);

	let public = RsaPublicKey::from(&jwt::parse_key(APP_KEY).unwrap());
	let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
	public
		.verify(
			Pkcs1v15Sign::new_unprefixed(),
			&jwt::digest_info(&format!("{}.{}", parts[0], parts[1])),
			&signature,
		)
		.expect("the signature verifies under the app's public key");
	assert!(
		public
			.verify(
				Pkcs1v15Sign::new_unprefixed(),
				&jwt::digest_info("tampered"),
				&signature
			)
			.is_err()
	);
	assert_eq!(
		jwt::app_token("not a key", 1, now),
		Err(jwt::JwtError::BadKey)
	);
}

#[test]
fn git_providers_repo_names_and_addresses_are_checked() {
	for ok in ["acme/app", "group/sub/app", "a.b/c_d-e"] {
		assert!(valid_repo(ok), "{ok}");
	}
	for bad in [
		"",
		"app",
		"acme/",
		"/app",
		"acme/../x",
		"acme/a b",
		"-x/y",
		"a/b?c",
		"a//b",
	] {
		assert!(!valid_repo(bad), "{bad}");
	}
	for ok in [
		"https://git.example.com",
		"http://127.0.0.1:3000/",
		"https://x.test/gitea/",
	] {
		assert!(service::normalize_base_url(ok).is_ok(), "{ok}");
	}
	assert_eq!(
		service::normalize_base_url(" https://x.test/gitea/ ").unwrap(),
		"https://x.test/gitea"
	);
	for bad in [
		"",
		"ftp://x",
		"https://u:p@x.test",
		"https://x.test?a=1",
		"git@x.test:a/b",
		"x.test",
	] {
		assert!(service::normalize_base_url(bad).is_err(), "{bad}");
	}
}

// ---- GitHub: manifest flow, tokens, listing -----------------------------------------------

#[sqlx::test]
async fn git_providers_manifest_exchange_stores_the_app_and_hides_its_secrets(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), true).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);

	// The form podkit hands the browser: the manifest, and a state bound to this user.
	let start = flow::start(
		&st,
		alice,
		TeamId(team),
		"http://podkit.local",
		StartInput {
			name: "podkit-stub".to_owned(),
			organization: "acme".to_owned(),
			base_url: gh.url.clone(),
		},
	)
	.await
	.unwrap();
	assert!(
		start.action.starts_with(&format!(
			"{}/organizations/acme/settings/apps/new?state=",
			gh.url
		)),
		"{}",
		start.action
	);
	let manifest: Value = serde_json::from_str(&start.manifest).unwrap();
	assert_eq!(
		manifest["redirect_url"],
		"http://podkit.local/git-providers/github/callback"
	);
	assert_eq!(manifest["default_permissions"]["contents"], "read");
	assert_eq!(manifest["public"], false);
	let hook = manifest["hook_attributes"]["url"].as_str().unwrap();
	assert!(
		hook.starts_with("https://podkit.test/git-providers/github/webhook/"),
		"{hook}"
	);

	// GitHub's redirect: the code is exchanged, the app stored.
	let state = start.action.split("state=").nth(1).unwrap().to_owned();
	let done = flow::finish(&st, alice, "good-code", &state).await.unwrap();
	assert_eq!(done.provider.kind, Kind::GithubApp);
	assert_eq!(done.provider.app_slug.as_deref(), Some("podkit-stub"));
	assert!(
		done.install_url.starts_with(&format!(
			"{}/apps/podkit-stub/installations/new?state=",
			gh.url
		)),
		"{}",
		done.install_url
	);
	assert_eq!(count(&db, "git_providers").await, 1);
	// The id the manifest advertised in its webhook address is the provider's.
	assert!(hook.ends_with(&done.provider.id.to_string()));

	// Credentials are encrypted at rest and never in any response.
	let blob: Vec<u8> = sqlx::query_scalar("SELECT credentials FROM git_providers")
		.fetch_one(&db)
		.await
		.unwrap();
	let raw = String::from_utf8_lossy(&blob);
	for secret in ["BEGIN RSA", GITHUB_WEBHOOK_SECRET, "never-stored"] {
		assert!(!raw.contains(secret), "stored in the clear: {secret}");
	}
	let t = Some(token.as_str());
	let (_, list) = call(&app, "GET", &url(team), t, None).await;
	let (_, one) = call(
		&app,
		"GET",
		&format!("{}/{}", url(team), done.provider.id),
		t,
		None,
	)
	.await;
	for body in [list.to_string(), one.to_string()] {
		for secret in [
			"BEGIN RSA",
			"PRIVATE",
			GITHUB_WEBHOOK_SECRET,
			"never-stored",
			"private_key",
			"webhook_secret",
		] {
			assert!(!body.contains(secret), "{secret} in {body}");
		}
	}
	assert_eq!(list[0]["kind"], "github_app");
	assert_eq!(list[0]["push_webhooks"], true);
	assert_eq!(
		list[0]["install_url"],
		format!("{}/apps/podkit-stub/installations/new", gh.url)
	);

	// The app is tested with a JWT: it asks GitHub who it is.
	let (s, who) = call(
		&app,
		"POST",
		&format!("{}/{}/test", url(team), done.provider.id),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{who}");
	assert_eq!(who["login"], "podkit-stub");
	assert_eq!(who["detail"], "Installed on acme, solo.");
	assert!(gh.state.jwt_ok.load(Ordering::SeqCst) >= 2);
	assert_eq!(gh.state.jwt_bad.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn git_providers_manifest_callback_refuses_a_forged_foreign_or_stale_state(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), false).await;
	let (_, alice_team) = user_with_team(&app, "alice").await;
	let _ = user_with_team(&app, "mallory").await;
	let alice = UserId(user_id(&db, "alice").await);
	let mallory = UserId(user_id(&db, "mallory").await);
	let alice_team = TeamId(alice_team);

	let start = flow::start(
		&st,
		alice,
		alice_team,
		"http://podkit.local",
		StartInput {
			name: "mine".to_owned(),
			base_url: gh.url.clone(),
			..Default::default()
		},
	)
	.await
	.unwrap();
	let good = start.action.split("state=").nth(1).unwrap().to_owned();
	let message = |r: Result<flow::Finished, ServerError>| match r {
		Err(ServerError::Validation(m)) => m,
		other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
	};

	// Not ours at all.
	assert!(message(flow::finish(&st, alice, "good-code", "AAAA").await).contains("not valid"));
	assert!(message(flow::finish(&st, alice, "good-code", "").await).contains("not valid"));
	// A real state, but another signed-in user is finishing it: the CSRF case.
	assert!(message(flow::finish(&st, mallory, "good-code", &good).await).contains("not valid"));
	assert!(flow::installed(&st, mallory, &good).is_err());
	// A state that has expired.
	let stale = flow::stale_state_for_tests(&st, alice, alice_team);
	assert!(message(flow::finish(&st, alice, "good-code", &stale).await).contains("not valid"));
	// Nothing was stored by any of those.
	assert_eq!(count(&db, "git_providers").await, 0);
	// The owner still can, and then installed() names the team.
	assert_eq!(flow::installed(&st, alice, &good).unwrap(), alice_team);
	flow::finish(&st, alice, "good-code", &good).await.unwrap();
	assert_eq!(count(&db, "git_providers").await, 1);
	// A code GitHub does not know is its 404, in words.
	let again = flow::finish(&st, alice, "stale-code", &good).await;
	assert!(
		matches!(again, Err(ServerError::GitProvider(_))),
		"{:?}",
		again.map(|_| ())
	);
}

#[sqlx::test]
async fn git_providers_installation_tokens_are_minted_per_clone_from_a_signed_jwt(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), false).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let id = connect_github(&st, &gh.url, alice, TeamId(team)).await;

	let first = service::clone_access(&st, id, "acme/repo-001")
		.await
		.unwrap();
	let second = service::clone_access(&st, id, "acme/repo-001")
		.await
		.unwrap();
	let other = service::clone_access(&st, id, "solo/dockerized")
		.await
		.unwrap();

	// The clone URL is plain: the token travels as a header, never in the URL.
	assert_eq!(first.repo_url, format!("{}/acme/repo-001.git", gh.url));
	assert!(!first.repo_url.contains("ghs_"));
	// Each clone gets a token of its own, for the installation that holds the repository.
	assert!(first.token.starts_with("ghs_1_"), "{}", first.token);
	assert!(second.token.starts_with("ghs_1_") && second.token != first.token);
	assert!(other.token.starts_with("ghs_2_"), "{}", other.token);
	assert_eq!(gh.state.minted.load(Ordering::SeqCst), 3);

	// Every one of those asks was signed by the app's key (the stub checks RS256, `iss` and
	// the ten-minute limit) and none was refused.
	assert_eq!(gh.state.jwt_bad.load(Ordering::SeqCst), 0);
	assert!(gh.state.jwt_ok.load(Ordering::SeqCst) >= 6);

	// The token is not stored anywhere: not in the provider row.
	let row: Vec<u8> = sqlx::query_scalar("SELECT credentials FROM git_providers")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&row).contains("ghs_"));

	// A repository the app is not installed on is a plain refusal, not a token.
	let err = service::clone_access(&st, id, "stranger/secret")
		.await
		.err()
		.unwrap();
	assert!(
		matches!(&err, ServerError::GitProvider(m) if m.contains("not installed")),
		"{err:?}"
	);
	assert!(
		service::clone_access(&st, id, "../etc/passwd")
			.await
			.is_err()
	);
}

#[sqlx::test]
async fn git_providers_github_listing_reads_every_page_and_searches_server_side(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let id = connect_github(&st, &gh.url, alice, TeamId(team)).await;
	let t = Some(token.as_str());
	let list = |q: &str, page: u32| format!("{}/{id}/repos?q={q}&page={page}", url(team));

	// 105 repositories of one installation (two GitHub pages) and one of another: 106 in all,
	// served 20 at a time.
	let (s, first) = call(&app, "GET", &list("", 1), t, None).await;
	assert_eq!(s, StatusCode::OK, "{first}");
	assert_eq!(first["repos"].as_array().unwrap().len(), 20);
	assert_eq!(first["next_page"], 2);
	assert_eq!(first["repos"][0]["full_name"], "acme/needle-service");
	assert_eq!(first["repos"][0]["default_branch"], "trunk");
	let mut seen = 0;
	let mut page = 1;
	let mut names = std::collections::HashSet::new();
	loop {
		let (_, p) = call(&app, "GET", &list("", page), t, None).await;
		for r in p["repos"].as_array().unwrap() {
			names.insert(r["full_name"].as_str().unwrap().to_owned());
			seen += 1;
		}
		match p["next_page"].as_u64() {
			Some(n) => page = u32::try_from(n).unwrap(),
			None => break,
		}
	}
	assert_eq!(seen, 106);
	assert_eq!(names.len(), 106, "no repository twice");
	assert!(names.contains("acme/repo-104") && names.contains("solo/dockerized"));

	// The search runs on the server: one match, no next page.
	let (_, found) = call(&app, "GET", &list("NEEDLE", 1), t, None).await;
	assert_eq!(found["repos"].as_array().unwrap().len(), 1);
	assert_eq!(found["repos"][0]["full_name"], "acme/needle-service");
	assert_eq!(found["next_page"], Value::Null);

	// Branches: the default first.
	let (s, branches) = call(
		&app,
		"GET",
		&format!("{}/{id}/branches?repo=acme/repo-001", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{branches}");
	assert_eq!(branches, json!(["trunk", "dev"]));
	let (s, _) = call(
		&app,
		"GET",
		&format!("{}/{id}/branches?repo=nope", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---- GitLab and Gitea ---------------------------------------------------------------------

#[sqlx::test]
async fn git_providers_gitlab_connects_lists_and_never_returns_its_token(db: PgPool) {
	let gl = stubs::gitlab().await;
	let (_, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());

	// A wrong token is refused with the reason, and nothing is stored.
	let (s, err) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(
			json!({ "kind": "gitlab", "name": "lab", "base_url": gl.url, "token": "glpat-wrong" }),
		),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_GATEWAY, "{err}");
	assert!(
		err["error"]
			.as_str()
			.unwrap()
			.contains("rejected the credentials")
	);
	assert!(!err.to_string().contains("glpat-wrong"));
	assert_eq!(count(&db, "git_providers").await, 0);

	let (s, made) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(
			json!({ "kind": "gitlab", "name": "lab", "base_url": format!("{}/", gl.url), "token": GITLAB_TOKEN }),
		),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{made}");
	assert_eq!(
		made["base_url"],
		Value::String(gl.url.clone()),
		"trailing slash trimmed"
	);
	assert!(!made.to_string().contains(GITLAB_TOKEN));
	let blob: Vec<u8> = sqlx::query_scalar("SELECT credentials FROM git_providers")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&blob).contains(GITLAB_TOKEN));
	let id = made["id"].as_i64().unwrap();

	// A name taken is a conflict.
	let (s, _) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "kind": "gitlab", "name": "lab", "base_url": gl.url, "token": GITLAB_TOKEN })),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT);

	// Paged through GitLab's `x-next-page`, searched by GitLab.
	let (_, p1) = call(&app, "GET", &format!("{}/{id}/repos", url(team)), t, None).await;
	assert_eq!(p1["repos"].as_array().unwrap().len(), 20);
	assert_eq!(p1["next_page"], 2);
	let (_, p3) = call(
		&app,
		"GET",
		&format!("{}/{id}/repos?page=3", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(p3["repos"].as_array().unwrap().len(), 5);
	assert_eq!(p3["next_page"], Value::Null);
	let (_, hit) = call(
		&app,
		"GET",
		&format!("{}/{id}/repos?q=proj-07", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(
		hit["repos"],
		json!([{ "full_name": "group/proj-07", "default_branch": "main", "private": true }])
	);
	assert!(
		gl.state
			.listings
			.lock()
			.unwrap()
			.contains(&(1, "proj-07".to_owned()))
	);
	let (_, branches) = call(
		&app,
		"GET",
		&format!("{}/{id}/branches?repo=group/proj-07", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(branches, json!(["main", "dev"]));
	let (s, who) = call(&app, "POST", &format!("{}/{id}/test", url(team)), t, None).await;
	assert_eq!((s, who["login"].clone()), (StatusCode::OK, json!("root")));
}

#[sqlx::test]
async fn git_providers_gitea_connects_and_pages_repositories(db: PgPool) {
	let ge = stubs::gitea().await;
	let (_, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());

	// Gitea has no default address: it must be given.
	let (s, err) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "kind": "gitea", "name": "home", "token": GITEA_TOKEN })),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");

	let (s, made) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "kind": "gitea", "name": "home", "base_url": ge.url, "token": GITEA_TOKEN })),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{made}");
	let id = made["id"].as_i64().unwrap();
	let (_, p1) = call(
		&app,
		"GET",
		&format!("{}/{id}/repos?page=1", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(p1["repos"].as_array().unwrap().len(), 20);
	assert_eq!(p1["next_page"], 2);
	let (_, p3) = call(
		&app,
		"GET",
		&format!("{}/{id}/repos?page=3", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(p3["repos"].as_array().unwrap().len(), 5);
	assert_eq!(p3["next_page"], Value::Null);
	let (_, hit) = call(
		&app,
		"GET",
		&format!("{}/{id}/repos?q=app-31", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(hit["repos"][0]["full_name"], "ops/app-31");
	let (_, branches) = call(
		&app,
		"GET",
		&format!("{}/{id}/branches?repo=ops/app-31", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(branches, json!(["master", "feature"]));
}

#[sqlx::test]
async fn git_providers_gitlab_and_gitea_applications_get_a_webhook_and_lose_it_on_delete(
	db: PgPool,
) {
	let gl = stubs::gitlab().await;
	let ge = stubs::gitea().await;
	let (st, app) = world(db.clone(), true).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let t = Some(token.as_str());
	let place = project_and_server(&app, &token, team).await;

	let lab = token_provider(&st, alice, team, Kind::Gitlab, "lab", &gl.url, GITLAB_TOKEN)
		.await
		.id;
	let home = token_provider(&st, alice, team, Kind::Gitea, "home", &ge.url, GITEA_TOKEN)
		.await
		.id;

	let (s, a) = create_app(&app, &token, team, place, lab, "group/proj-07", "lab-app").await;
	assert_eq!(s, StatusCode::OK, "{a}");
	assert_eq!(a["git_repo"], "group/proj-07");
	assert_eq!(a["repo_url"], format!("{}/group/proj-07.git", gl.url));
	assert_eq!(a["notice"], Value::Null, "the webhook was made");
	let secret = a["webhook_secret"].as_str().unwrap().to_owned();
	let lab_app = a["id"].as_i64().unwrap();
	{
		let made = gl.state.hooks_made.lock().unwrap();
		assert_eq!(made.len(), 1);
		assert_eq!(
			made[0].0, "group/proj-07",
			"matched as one path segment, so the client encoded the slash"
		);
		assert_eq!(
			made[0].1["url"],
			format!("https://podkit.test/webhooks/deploy/{lab_app}")
		);
		assert_eq!(made[0].1["token"], secret.as_str());
		assert_eq!(made[0].1["push_events"], true);
	}
	let (s, g) = create_app(&app, &token, team, place, home, "ops/app-02", "home-app").await;
	assert_eq!(s, StatusCode::OK, "{g}");
	{
		let made = ge.state.hooks_made.lock().unwrap();
		assert_eq!(made[0].0, "ops/app-02");
		assert_eq!(made[0].1["config"]["secret"], g["webhook_secret"]);
		assert_eq!(made[0].1["events"], json!(["push"]));
	}

	// The provider cannot be deleted while either application uses it.
	let (s, in_use) = call(&app, "DELETE", &format!("{}/{lab}", url(team)), t, None).await;
	assert_eq!(s, StatusCode::CONFLICT, "{in_use}");
	assert!(in_use["error"].as_str().unwrap().contains("lab-app"));

	// Deleting the application takes its webhook down.
	let project = place.0;
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/projects/{project}/applications/{lab_app}"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		gl.state.hooks_deleted.lock().unwrap().as_slice(),
		&[("group/proj-07".to_owned(), "77".to_owned())]
	);
	let (s, _) = call(&app, "DELETE", &format!("{}/{lab}", url(team)), t, None).await;
	assert_eq!(s, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn git_providers_without_a_public_address_say_so_instead_of_failing(db: PgPool) {
	let ge = stubs::gitea().await;
	let (st, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let place = project_and_server(&app, &token, team).await;
	let home = token_provider(&st, alice, team, Kind::Gitea, "home", &ge.url, GITEA_TOKEN).await;
	assert!(!home.push_webhooks);

	let (s, a) = create_app(&app, &token, team, place, home.id, "ops/app-02", "home-app").await;
	assert_eq!(s, StatusCode::OK, "{a}");
	assert!(
		a["notice"].as_str().unwrap().contains("no public address"),
		"{a}"
	);
	assert!(
		ge.state.hooks_made.lock().unwrap().is_empty(),
		"no webhook the provider cannot call"
	);
}

// ---- push webhooks ------------------------------------------------------------------------

fn push(branch: &str, repo: &str) -> Vec<u8> {
	json!({ "ref": format!("refs/heads/{branch}"), "repository": { "full_name": repo } })
		.to_string()
		.into_bytes()
}

fn github_headers(delivery: &str, sig: String) -> Headers {
	vec![
		("x-github-event", "push".to_owned()),
		("x-github-delivery", delivery.to_owned()),
		("x-hub-signature-256", sig),
	]
}

#[sqlx::test]
async fn git_providers_github_webhook_checks_signature_and_ignores_replays(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), true).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let provider = connect_github(&st, &gh.url, alice, TeamId(team)).await;
	let place = project_and_server(&app, &token, team).await;
	let (s, a) = create_app(&app, &token, team, place, provider, "acme/repo-001", "api").await;
	assert_eq!(s, StatusCode::OK, "{a}");
	let (s, _) = create_app(
		&app,
		&token,
		team,
		place,
		provider,
		"acme/repo-002",
		"other",
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let hook = format!("/git-providers/github/webhook/{provider}");
	let body = push("main", "acme/repo-001");
	let good = sign(GITHUB_WEBHOOK_SECRET, &body);
	let send = |h: Headers, body: Vec<u8>| {
		let (app, hook) = (app.clone(), hook.clone());
		async move { post_raw(&app, &hook, &h, &body).await }
	};

	// No signature, a wrong one, a signature of another body, and another secret: all refused
	// alike, and nothing is deployed or remembered.
	let (s, _) = send(vec![("x-github-event", "push".to_owned())], body.clone()).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = send(github_headers("d0", "sha256=00".to_owned()), body.clone()).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let other = sign(GITHUB_WEBHOOK_SECRET, b"other");
	let (s, _) = send(github_headers("d0", other), body.clone()).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = send(
		github_headers("d0", sign("wrong-secret", &body)),
		body.clone(),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "deployments").await, 0);
	assert_eq!(
		count(&db, "git_webhook_deliveries").await,
		0,
		"a refused request is not recorded"
	);

	// A valid push to the followed branch deploys exactly the matching application.
	let (s, queued) = send(github_headers("d1", good.clone()), body.clone()).await;
	assert_eq!(s, StatusCode::ACCEPTED, "{queued}");
	assert_eq!(queued["queued"], 1);
	assert_eq!(count(&db, "deployments").await, 1);

	// The same delivery again is a replay: acknowledged, ignored, nothing more is deployed.
	let (s, again) = send(github_headers("d1", good.clone()), body.clone()).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(again["ignored"], "replayed");
	assert_eq!(count(&db, "deployments").await, 1);

	// A new delivery of the same push does deploy; another branch and another repository do not.
	let (s, _) = send(github_headers("d2", good), body.clone()).await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(count(&db, "deployments").await, 2);
	let other_branch = push("feature", "acme/repo-001");
	let sig = sign(GITHUB_WEBHOOK_SECRET, &other_branch);
	let (s, r) = send(github_headers("d3", sig), other_branch).await;
	assert_eq!((s, r["queued"].clone()), (StatusCode::ACCEPTED, json!(0)));
	let stranger = push("main", "acme/not-ours");
	let sig = sign(GITHUB_WEBHOOK_SECRET, &stranger);
	let (_, r) = send(github_headers("d4", sig), stranger).await;
	assert_eq!(r["queued"], 0);
	assert_eq!(count(&db, "deployments").await, 2);

	// A ping is acknowledged; a tag push is not a branch push.
	let ping = b"{}".to_vec();
	let (s, r) = send(
		vec![
			("x-github-event", "ping".to_owned()),
			("x-hub-signature-256", sign(GITHUB_WEBHOOK_SECRET, &ping)),
		],
		ping,
	)
	.await;
	assert_eq!((s, r["pong"].clone()), (StatusCode::OK, json!(true)));
	let tag = json!({ "ref": "refs/tags/v1", "repository": { "full_name": "acme/repo-001" } })
		.to_string()
		.into_bytes();
	let sig = sign(GITHUB_WEBHOOK_SECRET, &tag);
	let (_, r) = send(github_headers("d5", sig), tag).await;
	assert_eq!(r["ignored"], "not a branch push");

	// A provider that does not exist answers like a bad signature.
	let (s, _) = post_raw(
		&app,
		"/git-providers/github/webhook/12345",
		&vec![("x-hub-signature-256", sign("x", &body))],
		&body,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "deployments").await, 2);
}

#[sqlx::test]
async fn git_providers_per_application_webhook_follows_branch_and_ignores_replays(db: PgPool) {
	let ge = stubs::gitea().await;
	let (st, app) = world(db.clone(), true).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let place = project_and_server(&app, &token, team).await;
	let home = token_provider(&st, alice, team, Kind::Gitea, "home", &ge.url, GITEA_TOKEN)
		.await
		.id;
	let (_, a) = create_app(&app, &token, team, place, home, "ops/app-02", "home-app").await;
	let secret = a["webhook_secret"].as_str().unwrap().to_owned();
	let hook = format!("/webhooks/deploy/{}", a["id"]);

	// Gitea signs with the bare hex digest in `X-Gitea-Signature`.
	let bare = |body: &[u8]| sign(&secret, body).trim_start_matches("sha256=").to_owned();
	let gitea = |delivery: &str, sig: String| -> Headers {
		vec![
			("x-gitea-event", "push".to_owned()),
			("x-gitea-delivery", delivery.to_owned()),
			("x-gitea-signature", sig),
		]
	};
	let go = |h: Headers, body: Vec<u8>| {
		let (app, hook) = (app.clone(), hook.clone());
		async move { post_raw(&app, &hook, &h, &body).await }
	};
	let body = push("main", "ops/app-02");
	let (s, _) = go(gitea("g1", "00".to_owned()), body.clone()).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = go(gitea("g1", bare(&body)), body.clone()).await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(count(&db, "deployments").await, 1);
	// Redelivered: ignored.
	let (s, r) = go(gitea("g1", bare(&body)), body.clone()).await;
	assert_eq!((s, r["ignored"].clone()), (StatusCode::OK, json!(true)));
	assert_eq!(count(&db, "deployments").await, 1);
	// Another branch: not what this application follows.
	let feature = push("feature", "ops/app-02");
	let (s, r) = go(gitea("g2", bare(&feature)), feature).await;
	assert_eq!((s, r["ignored"].clone()), (StatusCode::OK, json!(true)));
	assert_eq!(count(&db, "deployments").await, 1);
	// GitLab's token header still works, and a non-push event is not a deploy.
	let (s, _) = go(
		vec![
			("x-gitlab-token", secret.clone()),
			("x-gitlab-event", "Push Hook".to_owned()),
			("x-gitlab-event-uuid", "u1".to_owned()),
		],
		body.clone(),
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	let (_, r) = go(
		vec![
			("x-gitlab-token", secret),
			("x-gitlab-event", "Merge Request Hook".to_owned()),
		],
		body,
	)
	.await;
	assert_eq!(r["ignored"], true);
	assert_eq!(count(&db, "deployments").await, 2);
}

// ---- tenancy, permissions, and what is in use ---------------------------------------------

#[sqlx::test]
async fn git_providers_are_invisible_to_other_teams(db: PgPool) {
	let ge = stubs::gitea().await;
	let (_, app) = world(db.clone(), false).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (mallory, m_team) = user_with_team(&app, "mallory").await;
	let (s, made) = call(
		&app,
		"POST",
		&url(a_team),
		Some(&alice),
		Some(json!({ "kind": "gitea", "name": "home", "base_url": ge.url, "token": GITEA_TOKEN })),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED);
	let id = made["id"].as_i64().unwrap();
	let m = Some(mallory.as_str());

	// Through her own team's path, alice's provider does not exist.
	for (method, path) in [
		("GET", format!("{}/{id}", url(m_team))),
		("DELETE", format!("{}/{id}", url(m_team))),
		("POST", format!("{}/{id}/test", url(m_team))),
		("GET", format!("{}/{id}/repos", url(m_team))),
		(
			"GET",
			format!("{}/{id}/branches?repo=ops/app-01", url(m_team)),
		),
	] {
		let (s, _) = call(&app, method, &path, m, None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	// Through alice's team path she is not a member: refused outright.
	for (method, path) in [
		("GET", url(a_team)),
		("GET", format!("{}/{id}", url(a_team))),
		("DELETE", format!("{}/{id}", url(a_team))),
	] {
		let (s, _) = call(&app, method, &path, m, None).await;
		assert!(
			s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
			"{method} {path}: {s}"
		);
	}
	let (_, list) = call(&app, "GET", &url(m_team), m, None).await;
	assert_eq!(list, json!([]));

	// An application cannot be pointed at another team's provider.
	let place = project_and_server(&app, &mallory, m_team).await;
	let (s, r) = create_app(
		&app,
		&mallory,
		m_team,
		place,
		GitProviderId(id),
		"ops/app-02",
		"steal",
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "{r}");
	assert_eq!(count(&db, "applications").await, 0);
	// And alice's provider is still there.
	let (s, _) = call(
		&app,
		"GET",
		&format!("{}/{id}", url(a_team)),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
}

#[sqlx::test]
async fn git_providers_need_their_own_permissions(db: PgPool) {
	let ge = stubs::gitea().await;
	let (st, app) = world(db.clone(), false).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let owner = UserId(user_id(&db, "alice").await);
	let provider = token_provider(&st, owner, team, Kind::Gitea, "home", &ge.url, GITEA_TOKEN)
		.await
		.id;

	// A member whose role holds no git_provider permission may not even list them.
	let (viewer, _) = user_with_team(&app, "viewer").await;
	let viewer_id = user_id(&db, "viewer").await;
	team_role(&db, 9_000_001, team, "nothing").await;
	sqlx::query(
		"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES (9000002, $1, $2, 9000001)",
	)
	.bind(team)
	.bind(viewer_id)
	.execute(&db)
	.await
	.unwrap();
	let v = Some(viewer.as_str());
	let new = json!({ "kind": "gitea", "name": "x", "base_url": ge.url, "token": GITEA_TOKEN });
	for (method, path, body) in [
		("GET", format!("{}/{provider}", url(team)), None),
		("DELETE", format!("{}/{provider}", url(team)), None),
		("POST", format!("{}/{provider}/test", url(team)), None),
		("GET", format!("{}/{provider}/repos", url(team)), None),
		("POST", url(team), Some(new)),
		(
			"POST",
			format!("{}/github/manifest", url(team)),
			Some(json!({ "name": "x" })),
		),
		("GET", url(team), None),
	] {
		let (s, _) = call(&app, method, &path, v, body).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
	}
	assert_eq!(count(&db, "git_providers").await, 1);
	let (s, _) = call(&app, "GET", &url(team), Some(&alice), None).await;
	assert_eq!(s, StatusCode::OK);
}

#[sqlx::test]
async fn git_providers_in_use_cannot_be_deleted_until_nothing_uses_them(db: PgPool) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let t = Some(token.as_str());
	let provider = connect_github(&st, &gh.url, alice, TeamId(team)).await;
	let place = project_and_server(&app, &token, team).await;
	let (s, a) = create_app(&app, &token, team, place, provider, "acme/repo-001", "api").await;
	assert_eq!(s, StatusCode::OK, "{a}");

	let (s, refused) = call(
		&app,
		"DELETE",
		&format!("{}/{provider}", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT, "{refused}");
	assert!(refused["error"].as_str().unwrap().contains("api"));
	assert_eq!(count(&db, "git_providers").await, 1);
	// The listing says who uses it.
	let (_, one) = call(&app, "GET", &format!("{}/{provider}", url(team)), t, None).await;
	assert_eq!(one["used_by"][0]["name"], "api");
	assert_eq!(one["used_by"][0]["repo"], "acme/repo-001");

	let project = place.0;
	let id = a["id"].as_i64().unwrap();
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/projects/{project}/applications/{id}"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("{}/{provider}", url(team)),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NO_CONTENT);
	assert_eq!(count(&db, "git_providers").await, 0);
}

#[sqlx::test]
async fn git_providers_applications_take_no_credential_beside_a_provider_and_clone_with_its_token(
	db: PgPool,
) {
	let gh = stubs::github().await;
	let (st, app) = world(db.clone(), false).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let provider = connect_github(&st, &gh.url, alice, TeamId(team)).await;
	let (project, server) = project_and_server(&app, &token, team).await;
	let body = |extra: &Value| {
		let mut b = json!({
			"name": "x", "slug": "x", "server_id": server, "git_provider_id": provider.0,
			"git_repo": "acme/repo-001", "container_port": 80,
		});
		for (k, v) in extra.as_object().unwrap() {
			b[k] = v.clone();
		}
		b
	};
	let path = format!("/api/v1/teams/{team}/projects/{project}/applications");
	for extra in [
		json!({ "repo_url": "https://example.com/a.git" }),
		json!({ "git_token": "t" }),
		json!({ "generate_deploy_key": true }),
		json!({ "image": "traefik/whoami" }),
		json!({ "git_repo": "" }),
		json!({ "git_repo": "../../x" }),
	] {
		let (s, r) = call(&app, "POST", &path, Some(&token), Some(body(&extra))).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{extra} {r}");
	}
	assert_eq!(count(&db, "applications").await, 0);
	assert_eq!(
		count(&db, "ssh_keys").await,
		0,
		"no key made for a refused application"
	);

	let (s, a) = call(&app, "POST", &path, Some(&token), Some(body(&json!({})))).await;
	assert_eq!(s, StatusCode::OK, "{a}");
	assert_eq!(a["repo_url"], format!("{}/acme/repo-001.git", gh.url));
	assert_eq!(a["git_provider_id"], provider.0);
	let row =
		crate::applications::find_by_id(&db, crate::ids::ApplicationId(a["id"].as_i64().unwrap()))
			.await
			.unwrap()
			.unwrap();
	assert!(row.git_token.is_none() && row.deploy_key_id.is_none());

	// The deploy job asks the provider for its token at clone time: a fresh one every deploy.
	let source = || {
		let (st, row) = (st.clone(), row.clone());
		async move {
			let deployment = crate::deployments::insert(&st.db, row.id, None, None, None)
				.await
				.unwrap();
			match crate::deployments::run::app_source(&st, &row, &deployment)
				.await
				.ok()
				.unwrap()
			{
				proto::Source::Git {
					auth: proto::GitAuth::HttpsToken { token },
					repo_url,
					..
				} => (repo_url, token),
				_ => panic!("not a git clone with a token"),
			}
		}
	};
	let (url1, t1) = source().await;
	let (_, t2) = source().await;
	assert_eq!(url1, format!("{}/acme/repo-001.git", gh.url));
	assert!(
		t1.starts_with("ghs_1_") && t2.starts_with("ghs_1_") && t1 != t2,
		"{t1} {t2}"
	);

	// General-tab style update: the branch can change, the provider and repository cannot, and
	// no credential may be added.
	let one = format!("{path}/{}", a["id"]);
	let (s, u) = call(
		&app,
		"PATCH",
		&one,
		Some(&token),
		Some(json!({
			"name": "x", "slug": "x", "repo_url": "https://elsewhere.test/y.git",
			"git_ref": "dev", "container_port": 80,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{u}");
	assert_eq!(u["git_ref"], "dev");
	assert_eq!(
		u["repo_url"],
		format!("{}/acme/repo-001.git", gh.url),
		"the provider's address stays"
	);
	assert_eq!(u["git_repo"], "acme/repo-001");
	let (s, _) = call(
		&app,
		"PATCH",
		&one,
		Some(&token),
		Some(json!({ "name": "x", "slug": "x", "git_token": "t", "container_port": 80 })),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn git_providers_a_wrong_kind_of_credential_is_not_used(db: PgPool) {
	let (st, _) = world(db, false).await;
	let creds = Credentials::Token {
		token: "t".to_owned(),
	};
	let target = Target {
		kind: Kind::GithubApp,
		base_url: "http://127.0.0.1:1",
		credentials: &creds,
		http: &st.http,
	};
	assert!(target.test().await.is_err());
	assert!(
		Kind::parse("github_app") == Some(Kind::GithubApp)
			&& Kind::parse("bitbucket").is_none()
			&& Kind::Gitea.default_base_url().is_none()
	);
}
