//! Autodeploy tests, against stub GitHub, GitLab and Gitea APIs (real HTTP on loopback): waiting
//! for checks (pass, fail, timeout), path filters, branch patterns, freezes and forcing,
//! coalescing, skip markers, the commit status posted back, and the rules every slice follows
//! (other teams get 404, a role without the permission is refused).

use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::engine::{self, Advance, Outcome, Push};
use super::service::{self, FreezeInput, SettingsInput};
use super::{NewFreeze, Settings, State};
use crate::applications::{self, Application};
use crate::error::ServerError;
use crate::git_providers::api::StatusState;
use crate::git_providers::stubs::{self, GITEA_TOKEN, GITLAB_TOKEN};
use crate::git_providers::tests::{
	connect_github, create_app, project_and_server, token_provider, world,
};
use crate::git_providers::{Kind, service as provider_service};
use crate::ids::{ApplicationId, GitProviderId, ProjectId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{count, join_as_project_reader, user_id, user_with_team};

/// A world with one user, `alice`, and her team's project and server.
struct Rig {
	st: AppState,
	app: axum::Router,
	db: PgPool,
	token: String,
	team: i64,
	place: (i64, i64),
	alice: UserId,
}

async fn rig(db: PgPool) -> Rig {
	let (st, app) = world(db.clone(), true).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = UserId(user_id(&db, "alice").await);
	let place = project_and_server(&app, &token, team).await;
	Rig {
		st,
		app,
		db,
		token,
		team,
		place,
		alice,
	}
}

impl Rig {
	async fn app_on(&self, provider: GitProviderId, repo: &str, slug: &str) -> Application {
		let (s, a) = create_app(
			&self.app,
			&self.token,
			self.team,
			self.place,
			provider,
			repo,
			slug,
		)
		.await;
		assert!(s.is_success(), "{s} {a}");
		applications::find_by_id(&self.db, ApplicationId(a["id"].as_i64().unwrap()))
			.await
			.unwrap()
			.unwrap()
	}

	async fn gitea(&self) -> (stubs::Running<stubs::Gitea>, Application) {
		let ge = stubs::gitea().await;
		let p = token_provider(
			&self.st,
			self.alice,
			self.team,
			Kind::Gitea,
			"home",
			&ge.url,
			GITEA_TOKEN,
		)
		.await;
		let app = self.app_on(p.id, "ops/app-02", "home-app").await;
		(ge, app)
	}

	async fn gitlab(&self) -> (stubs::Running<stubs::Gitlab>, Application) {
		let gl = stubs::gitlab().await;
		let p = token_provider(
			&self.st,
			self.alice,
			self.team,
			Kind::Gitlab,
			"lab",
			&gl.url,
			GITLAB_TOKEN,
		)
		.await;
		let app = self.app_on(p.id, "group/proj-01", "lab-app").await;
		(gl, app)
	}

	async fn github(&self) -> (stubs::Running<stubs::Github>, Application) {
		let gh = stubs::github().await;
		let p = connect_github(&self.st, &gh.url, self.alice, TeamId(self.team)).await;
		let app = self.app_on(p, "acme/repo-001", "hub-app").await;
		(gh, app)
	}

	fn project(&self) -> ProjectId {
		ProjectId(self.place.0)
	}

	async fn states(&self) -> Vec<String> {
		sqlx::query_scalar("SELECT state FROM autodeploy_pushes ORDER BY id")
			.fetch_all(&self.db)
			.await
			.unwrap()
	}

	async fn detail_of(&self, state: &str) -> String {
		sqlx::query_scalar(
			"SELECT detail FROM autodeploy_pushes WHERE state = $1 ORDER BY id DESC LIMIT 1",
		)
		.bind(state)
		.fetch_one(&self.db)
		.await
		.unwrap()
	}

	async fn deployments(&self) -> i64 {
		count(&self.db, "deployments").await
	}

	async fn events(&self) -> i64 {
		count(&self.db, "notification_events").await
	}

	async fn audited(&self, action: &str) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = $1")
			.bind(action)
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	async fn one_push(&self) -> i64 {
		sqlx::query_scalar("SELECT id FROM autodeploy_pushes")
			.fetch_one(&self.db)
			.await
			.unwrap()
	}
}

fn sha(n: u64) -> String {
	format!("{n:040x}")
}

/// A push payload of the shape the three providers send.
fn push_of(git_ref: &str, before: u64, after: u64, message: &str, modified: &[&str]) -> Push {
	Push::from_payload(
		json!({
			"ref": git_ref,
			"before": sha(before),
			"after": sha(after),
			"head_commit": { "message": message },
			"commits": [{ "id": sha(after), "message": message, "added": [], "modified": modified, "removed": [] }],
		})
		.to_string()
		.as_bytes(),
	)
}

fn main_push(after: u64) -> Push {
	push_of("refs/heads/main", after - 1, after, "work", &["src/a.rs"])
}

async fn settings(r: &Rig, app: &Application, s: Settings) {
	super::save_settings(&r.db, app.id, &s).await.unwrap();
}

fn waits() -> Settings {
	Settings {
		wait_for_ci: true,
		ci_timeout_secs: 30,
		post_status: true,
		..Settings::default()
	}
}

fn later(secs: i64) -> OffsetDateTime {
	OffsetDateTime::now_utc() + Duration::seconds(secs)
}

fn posted(c: &stubs::Commits) -> Vec<(String, Value)> {
	c.posted.lock().unwrap().clone()
}

fn set<T>(slot: &std::sync::Mutex<Option<T>>, value: T) {
	*slot.lock().unwrap() = Some(value);
}

fn running_suite() -> Value {
	json!({ "check_suites": [{ "status": "in_progress", "conclusion": null, "latest_check_runs_count": 2, "app": { "slug": "actions" } }] })
}

fn passed_suite() -> Value {
	json!({ "check_suites": [{ "status": "completed", "conclusion": "success", "latest_check_runs_count": 2, "app": { "slug": "actions" } }] })
}

fn pending_ci() -> Value {
	json!({ "total_count": 1, "statuses": [{ "status": "pending", "context": "ci" }] })
}

fn passed_ci() -> Value {
	json!({ "total_count": 1, "statuses": [{ "status": "success", "context": "ci" }] })
}

fn once_freeze(from: Duration, to: Duration) -> NewFreeze {
	NewFreeze {
		kind: "once",
		start_minute: None,
		end_minute: None,
		utc_offset_minutes: 0,
		starts_at: Some(OffsetDateTime::now_utc() + from),
		ends_at: Some(OffsetDateTime::now_utc() + to),
		note: "release week".into(),
	}
}

fn once_input(from: &str, to: &str) -> FreezeInput {
	FreezeInput {
		kind: "once".into(),
		from: from.into(),
		to: to.into(),
		..FreezeInput::default()
	}
}

// ---- the payloads and the forms ------------------------------------------------------------

#[test]
fn autodeploy_reads_what_the_three_providers_send() {
	// GitHub and Gitea: head_commit and commits with file lists.
	let p = Push::from_payload(
		json!({
			"ref": "refs/heads/main", "before": sha(1), "after": sha(3),
			"head_commit": { "message": "newest\n\nbody" },
			"commits": [
				{ "id": sha(2), "message": "older", "added": ["a"], "modified": ["b"], "removed": [] },
				{ "id": sha(3), "message": "newest", "added": [], "modified": ["b", "c"], "removed": ["d"] },
			],
		})
		.to_string()
		.as_bytes(),
	);
	assert_eq!(p.git_ref, "refs/heads/main");
	assert_eq!(p.after.as_deref(), Some(sha(3).as_str()));
	assert_eq!(p.message, "newest\n\nbody");
	assert_eq!(p.commits, vec![sha(2), sha(3)]);
	assert_eq!(
		p.payload_files,
		Some(["a", "b", "c", "d"].map(str::to_owned).to_vec())
	);

	// GitLab: no head_commit; the message is the commit the push ends on.
	let p = Push::from_payload(
		json!({
			"ref": "refs/heads/main", "before": sha(1), "after": sha(2), "checkout_sha": sha(2),
			"commits": [{ "id": sha(2), "message": "fix [skip ci]", "added": [], "modified": ["x"], "removed": [] }],
		})
		.to_string()
		.as_bytes(),
	);
	assert_eq!(p.message, "fix [skip ci]");

	// Whatever is missing stays empty: the application's rules decide what that means.
	let p = Push::from_payload(b"not json");
	assert!(p.git_ref.is_empty() && p.after.is_none() && p.payload_files.is_none());
	let p = Push::from_payload(
		json!({ "ref": "refs/heads/x", "commits": [{ "id": "a" }] })
			.to_string()
			.as_bytes(),
	);
	assert!(
		p.payload_files.is_none(),
		"no file lists in the payload: unknown, not empty"
	);
}

#[test]
fn autodeploy_settings_forms_are_checked() {
	let ok = SettingsInput {
		enabled: true,
		branches: " main \n\nrelease/*\n".into(),
		paths: "api/**\n!docs/**".into(),
		wait_for_ci: true,
		ci_timeout_min: "20".into(),
		post_status: true,
		..SettingsInput::default()
	}
	.parse()
	.unwrap();
	assert_eq!(ok.branch_patterns, ["main", "release/*"]);
	assert_eq!(ok.path_filters, ["api/**", "!docs/**"]);
	assert_eq!(ok.ci_timeout_secs, 1200);
	for bad in ["abc", "0", "1441", "-5"] {
		let r = SettingsInput {
			ci_timeout_min: bad.into(),
			..SettingsInput::default()
		}
		.parse();
		assert!(matches!(r, Err(ServerError::Validation(_))), "{bad}");
	}
	let long = "x".repeat(200);
	assert!(
		SettingsInput {
			branches: long,
			..SettingsInput::default()
		}
		.parse()
		.is_err()
	);
	let many = (0..30)
		.map(|i| format!("p{i}"))
		.collect::<Vec<_>>()
		.join("\n");
	assert!(
		SettingsInput {
			paths: many,
			..SettingsInput::default()
		}
		.parse()
		.is_err()
	);
	assert!(
		SettingsInput {
			branches: "a\u{7}b".into(),
			..SettingsInput::default()
		}
		.parse()
		.is_err()
	);
	assert_eq!(
		SettingsInput::default().parse().unwrap().ci_timeout_secs,
		900,
		"empty takes the default"
	);
}

#[test]
fn autodeploy_freeze_forms_are_checked() {
	let weekly = |sd: &str, st: &str, ed: &str, et: &str| FreezeInput {
		kind: "recurring".into(),
		start_day: sd.into(),
		start_time: st.into(),
		end_day: ed.into(),
		end_time: et.into(),
		..FreezeInput::default()
	};
	let ok = weekly("4", "16:00", "0", "08:00").parse().unwrap();
	assert_eq!(
		(ok.start_minute, ok.end_minute),
		(Some(4 * 24 * 60 + 960), Some(480))
	);
	// A one-off window is read on the clock the offset names.
	let once = FreezeInput {
		offset: "120".into(),
		..once_input("2026-12-24T00:00", "2026-12-27T00:00")
	}
	.parse()
	.unwrap();
	assert_eq!(
		once.starts_at.unwrap().unix_timestamp(),
		time::macros::datetime!(2026-12-23 22:00 UTC).unix_timestamp()
	);
	for bad in [
		once_input("2026-12-27T00:00", "2026-12-24T00:00"),
		once_input("tomorrow", "later"),
		weekly("9", "10:00", "1", "10:00"),
		weekly("1", "25:00", "1", "10:00"),
		weekly("1", "10:00", "1", "10:00"),
		FreezeInput {
			kind: "sometimes".into(),
			..FreezeInput::default()
		},
		FreezeInput {
			offset: "5000".into(),
			..weekly("1", "01:00", "2", "01:00")
		},
	] {
		assert!(
			matches!(bad.parse(), Err(ServerError::Validation(_))),
			"{bad:?}"
		);
	}
}

// ---- waiting for checks --------------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_waits_for_gitea_statuses_then_deploys_and_posts_back(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(&r, &app, waits()).await;
	let c = &ge.state.commits;
	set(
		&c.checks,
		json!({ "state": "pending", "total_count": 2, "statuses": [
			{ "status": "pending", "context": "ci/build" },
			{ "status": "pending", "context": "podkit/deploy" },
		]}),
	);

	let out = engine::handle(&r.st, &app, &main_push(10)).await.unwrap();
	assert!(matches!(out, Outcome::Waiting), "{out:?}");
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["waiting_ci".to_owned()])
	);
	let sent = posted(c);
	assert_eq!(sent.len(), 1);
	assert_eq!(sent[0].0, format!("ops/app-02@{}", sha(10)));
	assert_eq!(sent[0].1["state"], "pending");
	assert_eq!(sent[0].1["context"], "podkit/deploy");

	// Still running: another pass changes nothing.
	engine::process(&r.st, later(5)).await;
	assert_eq!(r.deployments().await, 0);

	// The build passes. podkit's own pending status is not a check, so it is not waited for.
	set(
		&c.checks,
		json!({ "state": "pending", "total_count": 2, "statuses": [
			{ "status": "success", "context": "ci/build" },
			{ "status": "pending", "context": "podkit/deploy" },
		]}),
	);
	engine::process(&r.st, later(10)).await;
	assert_eq!(r.deployments().await, 1);
	assert_eq!(r.states().await, ["deployed"]);
	let sent = posted(c);
	assert_eq!(sent.len(), 2, "{sent:?}");
	assert_eq!(sent[1].1["state"], "pending");
	assert_eq!(sent[1].1["description"], "Deploying with podkit.");
	// The link goes back to the application's page.
	let link = sent[1].1["target_url"].as_str().unwrap();
	assert!(
		link.starts_with("https://podkit.test/teams/")
			&& link.ends_with(&format!("/apps/{}", app.id)),
		"{link}"
	);

	// When the deploy ends, success is posted on the same commit.
	let dep: i64 = sqlx::query_scalar("SELECT deployment_id FROM autodeploy_pushes")
		.fetch_one(&r.db)
		.await
		.unwrap();
	engine::post_result(&r.st, dep, StatusState::Success, "Deployed with podkit.").await;
	let sent = posted(c);
	assert_eq!(sent.len(), 3);
	assert_eq!(sent[2].0, format!("ops/app-02@{}", sha(10)));
	assert_eq!(sent[2].1["state"], "success");
}

#[sqlx::test]
async fn autodeploy_failed_gitlab_pipeline_deploys_nothing_and_tells_the_team(db: PgPool) {
	let r = rig(db).await;
	let (gl, app) = r.gitlab().await;
	settings(&r, &app, waits()).await;
	let c = &gl.state.commits;
	// The newest pipeline counts: an older failure was retried.
	set(
		&c.checks,
		json!([
			{ "id": 4, "status": "success", "source": "push" },
			{ "id": 5, "status": "failed", "source": "push" },
		]),
	);
	let out = engine::handle(&r.st, &app, &main_push(20)).await.unwrap();
	assert!(matches!(out, Outcome::Failed), "{out:?}");
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["ci_failed".to_owned()])
	);
	assert!(r.detail_of("ci_failed").await.contains("pipeline #5"));
	assert_eq!(r.events().await, 1, "the team hears about it");
	let (kind, title): (String, String) =
		sqlx::query_as("SELECT kind, title FROM notification_events")
			.fetch_one(&r.db)
			.await
			.unwrap();
	assert_eq!(kind, "deploy_failed");
	assert!(title.contains("was not deployed"), "{title}");
	// GitLab's words: pending first, then failed, under the status name podkit uses.
	let sent = posted(c);
	assert_eq!(
		sent.iter()
			.map(|(_, b)| b["state"].as_str().unwrap())
			.collect::<Vec<_>>(),
		["pending", "failed"]
	);
	assert!(
		sent.iter().all(
			|(k, b)| k == &format!("group/proj-01@{}", sha(20)) && b["name"] == "podkit/deploy"
		)
	);
	assert!(
		sent[1].1["description"]
			.as_str()
			.unwrap()
			.contains("pipeline #5")
	);

	// The pipeline a status post makes is `external` and not CI; a passing push pipeline wins.
	set(
		&c.checks,
		json!([
			{ "id": 9, "status": "running", "source": "external" },
			{ "id": 8, "status": "success", "source": "push" },
		]),
	);
	let out = engine::handle(&r.st, &app, &main_push(21)).await.unwrap();
	assert!(matches!(out, Outcome::Deployed(_)), "{out:?}");
}

#[sqlx::test]
async fn autodeploy_github_checks_time_out_or_deploy_anyway(db: PgPool) {
	let r = rig(db).await;
	let (gh, app) = r.github().await;
	settings(&r, &app, waits()).await;
	let c = &gh.state.commits;
	set(&c.suites, running_suite());
	set(
		&c.checks,
		json!({ "state": "success", "statuses": [{ "state": "success", "context": "legacy-ci" }] }),
	);
	let out = engine::handle(&r.st, &app, &main_push(30)).await.unwrap();
	assert!(matches!(out, Outcome::Waiting), "{out:?}");
	engine::process(&r.st, later(10)).await;
	assert_eq!(r.states().await, ["waiting_ci"]);
	assert_eq!(r.events().await, 0);

	// The wait runs out: nothing deploys, the team is told, the commit shows a failure.
	engine::process(&r.st, later(40)).await;
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["ci_timeout".to_owned()])
	);
	assert!(
		r.detail_of("ci_timeout")
			.await
			.contains("did not finish in time")
	);
	assert_eq!(r.events().await, 1);
	let sent = posted(c);
	assert_eq!(sent.last().unwrap().1["state"], "failure");
	assert_eq!(sent.last().unwrap().1["context"], "podkit/deploy");
	assert!(
		gh.state.jwt_ok.load(Ordering::SeqCst) > 0,
		"the app signed in with its key"
	);

	// With "deploy anyway", the same wait ends in a deploy.
	settings(
		&r,
		&app,
		Settings {
			deploy_on_ci_timeout: true,
			..waits()
		},
	)
	.await;
	engine::handle(&r.st, &app, &main_push(31)).await.unwrap();
	engine::process(&r.st, later(40)).await;
	assert_eq!(r.deployments().await, 1);
	assert!(r.detail_of("deployed").await.contains("deployed anyway"));

	// A passed suite and status deploy at once; empty suites (apps that ran nothing) are not checks.
	set(
		&c.suites,
		json!({ "check_suites": [
			{ "status": "queued", "conclusion": null, "latest_check_runs_count": 0, "app": { "slug": "idle" } },
			{ "status": "completed", "conclusion": "success", "latest_check_runs_count": 1, "app": { "slug": "actions" } },
			{ "status": "completed", "conclusion": "neutral", "latest_check_runs_count": 1, "app": { "slug": "lint" } },
		]}),
	);
	let out = engine::handle(&r.st, &app, &main_push(32)).await.unwrap();
	assert!(matches!(out, Outcome::Deployed(_)), "{out:?}");

	// A failed suite stops it, and a failed status does too.
	set(
		&c.suites,
		json!({ "check_suites": [{ "status": "completed", "conclusion": "failure", "latest_check_runs_count": 1, "app": { "slug": "actions" } }] }),
	);
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(33)).await.unwrap(),
		Outcome::Failed
	));
	set(&c.suites, passed_suite());
	set(
		&c.checks,
		json!({ "state": "failure", "statuses": [{ "state": "error", "context": "legacy-ci" }] }),
	);
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(34)).await.unwrap(),
		Outcome::Failed
	));
}

#[sqlx::test]
async fn autodeploy_no_checks_deploys_after_a_grace_and_a_refusal_keeps_waiting(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(
		&r,
		&app,
		Settings {
			ci_timeout_secs: 900,
			..waits()
		},
	)
	.await;
	// A commit nothing checks: wait for a first check to appear, then go on.
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(40)).await.unwrap(),
		Outcome::Waiting
	));
	engine::process(&r.st, later(30)).await;
	assert_eq!(r.deployments().await, 0);
	engine::process(&r.st, later(100)).await;
	assert_eq!(r.deployments().await, 1);
	assert!(r.detail_of("deployed").await.contains("no checks"));

	// The provider refuses the checks call: say why, keep the push, and stop at the deadline.
	ge.state.commits.checks_http.store(403, Ordering::SeqCst);
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(41)).await.unwrap(),
		Outcome::Waiting
	));
	assert!(
		r.detail_of("waiting_ci")
			.await
			.contains("Could not read the checks")
	);
	engine::process(&r.st, later(1000)).await;
	assert_eq!(r.states().await.last().unwrap(), "ci_timeout");
	assert_eq!(r.deployments().await, 1);
}

#[sqlx::test]
async fn autodeploy_checks_are_not_asked_of_an_application_without_a_provider(db: PgPool) {
	let r = rig(db).await;
	// An application made from a plain repository URL has no provider to ask.
	crate::tests::seed_application_on(&r.db, r.place.0, r.place.1).await;
	sqlx::query("UPDATE applications SET source_kind = 'git' WHERE id = 5002")
		.execute(&r.db)
		.await
		.unwrap();
	let app = applications::find_by_id(&r.db, ApplicationId(5002))
		.await
		.unwrap()
		.unwrap();
	settings(&r, &app, waits()).await;
	let out = engine::handle(&r.st, &app, &main_push(50)).await.unwrap();
	assert!(matches!(out, Outcome::Deployed(_)), "{out:?}");
	// Nothing could be asked, so the push did not wait at all.
	assert!(r.detail_of("deployed").await.starts_with("Deployed"));
}

// ---- path filters and branch rules ---------------------------------------------------------

#[sqlx::test]
async fn autodeploy_path_filters_use_the_providers_files(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(
		&r,
		&app,
		Settings {
			path_filters: vec!["api/**".into(), "!docs/**".into()],
			..Settings::default()
		},
	)
	.await;
	let c = &ge.state.commits;

	*c.files.lock().unwrap() = vec!["docs/readme.md".into()];
	let out = engine::handle(
		&r.st,
		&app,
		&push_of("refs/heads/main", 59, 60, "docs", &["api/x.rs"]),
	)
	.await
	.unwrap();
	assert!(
		matches!(out, Outcome::Filtered),
		"the provider's list wins over the payload's: {out:?}"
	);
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["filtered".to_owned()])
	);
	assert_eq!(c.file_reads.lock().unwrap().as_slice(), [sha(60)]);

	*c.files.lock().unwrap() = vec!["web/app.js".into(), "api/main.rs".into()];
	let out = engine::handle(&r.st, &app, &main_push(61)).await.unwrap();
	assert!(matches!(out, Outcome::Deployed(_)), "{out:?}");

	// The provider is down: the payload's own file lists decide.
	c.files_http.store(500, Ordering::SeqCst);
	let out = engine::handle(
		&r.st,
		&app,
		&push_of("refs/heads/main", 61, 62, "x", &["web/app.js"]),
	)
	.await
	.unwrap();
	assert!(matches!(out, Outcome::Filtered), "{out:?}");
	// Nothing known at all: the push is not stopped.
	let blind = Push::from_payload(
		json!({ "ref": "refs/heads/main", "before": sha(62), "after": sha(63) })
			.to_string()
			.as_bytes(),
	);
	assert!(matches!(
		engine::handle(&r.st, &app, &blind).await.unwrap(),
		Outcome::Deployed(_)
	));
	// The first deployment had not started when the last push replaced it.
	assert_eq!(
		r.states().await,
		["filtered", "superseded", "filtered", "deployed"]
	);
}

#[sqlx::test]
async fn autodeploy_asks_each_provider_for_the_files_of_a_push(db: PgPool) {
	let r = rig(db).await;
	let (gh, hub) = r.github().await;
	let (gl, lab) = r.gitlab().await;
	let (ge, home) = r.gitea().await;
	*gh.state.commits.files.lock().unwrap() = vec!["a.rs".into(), "b.rs".into()];
	*gl.state.commits.files.lock().unwrap() = vec!["a.rs".into()];
	*ge.state.commits.files.lock().unwrap() = vec!["g.rs".into()];
	let (before, after) = (sha(1), sha(2));
	let (hub_p, lab_p, home_p) = (
		hub.git_provider_id.unwrap(),
		lab.git_provider_id.unwrap(),
		home.git_provider_id.unwrap(),
	);

	// A range where there is a compare API, with the commits of the push where there is not.
	let files =
		provider_service::changed_files(&r.st, hub_p, "acme/repo-001", Some(&before), &after, &[])
			.await
			.unwrap();
	assert_eq!(files, ["a.rs", "b.rs"]);
	assert_eq!(
		gh.state.commits.file_reads.lock().unwrap().as_slice(),
		[format!("{before}...{after}")]
	);
	let files =
		provider_service::changed_files(&r.st, lab_p, "group/proj-01", Some(&before), &after, &[])
			.await
			.unwrap();
	assert_eq!(files, ["a.rs"]);
	assert_eq!(
		gl.state.commits.file_reads.lock().unwrap().as_slice(),
		[format!("{before}..{after}")]
	);
	let commits = vec![sha(5), sha(6)];
	let files = provider_service::changed_files(
		&r.st,
		home_p,
		"ops/app-02",
		Some(&before),
		&after,
		&commits,
	)
	.await
	.unwrap();
	assert_eq!(files, ["g.rs"]);
	assert_eq!(
		ge.state.commits.file_reads.lock().unwrap().as_slice(),
		commits.as_slice()
	);

	// A branch that was just made has no "before": the commit itself is read.
	let zero = "0".repeat(40);
	provider_service::changed_files(&r.st, hub_p, "acme/repo-001", Some(&zero), &after, &[])
		.await
		.unwrap();
	provider_service::changed_files(&r.st, lab_p, "group/proj-01", Some(&zero), &after, &[])
		.await
		.unwrap();
	assert_eq!(
		gh.state.commits.file_reads.lock().unwrap().last().unwrap(),
		&after
	);
	assert_eq!(
		gl.state.commits.file_reads.lock().unwrap().last().unwrap(),
		&after
	);

	// What goes into a path is checked first.
	assert!(
		provider_service::changed_files(&r.st, hub_p, "acme/repo-001", None, "../etc", &[])
			.await
			.is_err()
	);
	assert!(
		provider_service::ci(&r.st, lab_p, "group/proj-01", "zz?")
			.await
			.is_err()
	);
}

#[sqlx::test]
async fn autodeploy_branch_patterns_tags_and_the_off_switch(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	let go = |git_ref: &'static str, n: u64| {
		let (st, app) = (r.st.clone(), app.clone());
		async move {
			engine::handle(&st, &app, &push_of(git_ref, n, n + 1, "x", &[]))
				.await
				.unwrap()
		}
	};

	// No rules: the branch the application follows, and not a tag.
	assert!(matches!(
		go("refs/heads/feature", 1).await,
		Outcome::Ignored
	));
	assert!(matches!(go("refs/tags/v1", 1).await, Outcome::Ignored));
	assert!(matches!(
		go("refs/heads/main", 2).await,
		Outcome::Deployed(_)
	));
	// Ignored pushes leave no row: a busy repository would fill the list with them.
	assert_eq!(r.states().await, ["deployed"]);

	settings(
		&r,
		&app,
		Settings {
			branch_patterns: vec!["release/*".into(), "hotfix-*".into()],
			..Settings::default()
		},
	)
	.await;
	assert!(
		matches!(go("refs/heads/main", 4).await, Outcome::Ignored),
		"main is no longer listed"
	);
	assert!(matches!(
		go("refs/heads/release/1.2", 4).await,
		Outcome::Deployed(_)
	));
	assert!(matches!(
		go("refs/heads/hotfix-9", 5).await,
		Outcome::Deployed(_)
	));
	assert!(matches!(
		go("refs/heads/release/1/2", 6).await,
		Outcome::Ignored
	));
	assert!(
		matches!(go("refs/tags/release/1", 6).await, Outcome::Ignored),
		"tags are off"
	);

	settings(
		&r,
		&app,
		Settings {
			branch_patterns: vec!["v*".into()],
			deploy_tags: true,
			..Settings::default()
		},
	)
	.await;
	assert!(matches!(
		go("refs/tags/v1.0", 7).await,
		Outcome::Deployed(_)
	));
	assert!(matches!(go("refs/tags/nightly", 8).await, Outcome::Ignored));

	// A deleted ref, and "off".
	let deleted = push_of("refs/tags/v2", 9, 0, "", &[]);
	assert!(matches!(
		engine::handle(&r.st, &app, &deleted).await.unwrap(),
		Outcome::Ignored
	));
	settings(
		&r,
		&app,
		Settings {
			enabled: false,
			..Settings::default()
		},
	)
	.await;
	assert!(matches!(go("refs/heads/main", 10).await, Outcome::Ignored));
}

#[sqlx::test]
async fn autodeploy_skip_markers_stop_a_deploy(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	for (n, msg) in [
		(2, "docs: typo [skip deploy]"),
		(3, "wip [SKIP CI]\n\nlong body"),
	] {
		let out = engine::handle(
			&r.st,
			&app,
			&push_of("refs/heads/main", n - 1, n, msg, &["a"]),
		)
		.await
		.unwrap();
		assert!(matches!(out, Outcome::Skipped), "{msg}: {out:?}");
	}
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["skipped".to_owned(); 2])
	);
	assert!(r.detail_of("skipped").await.contains("[skip ci]"));
	// The same text without the marker deploys; a skipped commit does not disturb a waiting push.
	let plain = push_of("refs/heads/main", 3, 4, "skip the deploy please", &[]);
	assert!(matches!(
		engine::handle(&r.st, &app, &plain).await.unwrap(),
		Outcome::Deployed(_)
	));
	settings(&r, &app, waits()).await;
	engine::handle(&r.st, &app, &main_push(5)).await.unwrap();
	engine::handle(
		&r.st,
		&app,
		&push_of("refs/heads/main", 5, 6, "[skip deploy]", &[]),
	)
	.await
	.unwrap();
	assert_eq!(
		r.states()
			.await
			.iter()
			.filter(|s| *s == "waiting_ci")
			.count(),
		1
	);
}

// ---- freezes -------------------------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_a_push_in_a_freeze_waits_and_deploys_when_it_ends(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(
		&r,
		&app,
		Settings {
			post_status: true,
			..Settings::default()
		},
	)
	.await;
	super::add_freeze(
		&r.db,
		app.id,
		&once_freeze(Duration::hours(-1), Duration::hours(1)),
		r.alice,
	)
	.await
	.unwrap();

	let out = engine::handle(&r.st, &app, &main_push(70)).await.unwrap();
	assert!(matches!(out, Outcome::Frozen), "{out:?}");
	assert_eq!(
		(r.deployments().await, r.states().await),
		(0, vec!["frozen".to_owned()])
	);
	assert!(r.detail_of("frozen").await.contains("freeze window"));
	assert_eq!(
		posted(&ge.state.commits).len(),
		1,
		"pending: held by a freeze"
	);

	engine::process(&r.st, later(600)).await;
	assert_eq!(r.deployments().await, 0, "still inside the window");
	assert_eq!(
		posted(&ge.state.commits).len(),
		1,
		"no second status for the same hold"
	);
	engine::process(&r.st, later(2 * 3600)).await;
	assert_eq!(
		(r.deployments().await, r.states().await),
		(1, vec!["deployed".to_owned()])
	);
	assert!(r.detail_of("deployed").await.starts_with("Deployed"));
}

#[sqlx::test]
async fn autodeploy_a_weekly_freeze_from_the_database_holds_a_push(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	// Two weekly windows that between them cover the whole week bar its very last minute.
	for (start, end) in [(0, 5000), (5000, 10079)] {
		let w = NewFreeze {
			kind: "recurring",
			start_minute: Some(start),
			end_minute: Some(end),
			starts_at: None,
			ends_at: None,
			..once_freeze(Duration::ZERO, Duration::ZERO)
		};
		super::add_freeze(&r.db, app.id, &w, r.alice).await.unwrap();
	}
	let view = service::get(&r.st, r.alice, TeamId(r.team), r.project(), app.id)
		.await
		.unwrap();
	assert_eq!(view.freezes.len(), 2);
	let out = engine::handle(&r.st, &app, &main_push(72)).await.unwrap();
	// Sunday 23:59 is the one minute no window holds.
	assert!(
		matches!(out, Outcome::Frozen | Outcome::Deployed(_)),
		"{out:?}"
	);
}

#[sqlx::test]
async fn autodeploy_a_maintainer_can_force_a_frozen_push(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(
		&r,
		&app,
		Settings {
			post_status: true,
			..Settings::default()
		},
	)
	.await;
	super::add_freeze(
		&r.db,
		app.id,
		&once_freeze(Duration::hours(-1), Duration::hours(5)),
		r.alice,
	)
	.await
	.unwrap();
	engine::handle(&r.st, &app, &main_push(80)).await.unwrap();
	let push = r.one_push().await;

	let team = TeamId(r.team);
	let result = service::force(&r.st, r.alice, team, r.project(), app.id, push)
		.await
		.unwrap();
	assert!(matches!(result, Advance::Released(_)), "{result:?}");
	assert_eq!(
		(r.deployments().await, r.states().await),
		(1, vec!["deployed".to_owned()])
	);
	assert!(r.detail_of("deployed").await.contains("maintainer"));
	assert_eq!(r.audited("app.autodeploy.force").await, 1);
	let triggered_by: Option<i64> = sqlx::query_scalar("SELECT triggered_by FROM deployments")
		.fetch_one(&r.db)
		.await
		.unwrap();
	assert_eq!(
		triggered_by,
		Some(r.alice.0),
		"the deployment names who forced it"
	);
	assert_eq!(
		posted(&ge.state.commits).last().unwrap().1["description"],
		"Deploying with podkit."
	);

	// Forcing the same push twice is refused, not repeated.
	let again = service::force(&r.st, r.alice, team, r.project(), app.id, push).await;
	assert!(
		matches!(again, Err(ServerError::Validation(_))),
		"{again:?}"
	);
	assert_eq!(r.deployments().await, 1);

	// Forcing skips the wait for checks too.
	settings(&r, &app, waits()).await;
	set(&ge.state.commits.checks, pending_ci());
	engine::handle(&r.st, &app, &main_push(81)).await.unwrap();
	let waiting: i64 = sqlx::query_scalar(
		"SELECT id FROM autodeploy_pushes WHERE state IN ('waiting_ci', 'frozen')",
	)
	.fetch_one(&r.db)
	.await
	.unwrap();
	service::force(&r.st, r.alice, team, r.project(), app.id, waiting)
		.await
		.unwrap();
	assert_eq!(r.deployments().await, 2);
}

// ---- coalescing ----------------------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_pushes_in_quick_succession_deploy_only_the_latest(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(&r, &app, waits()).await;
	let c = &ge.state.commits;
	set(&c.checks, pending_ci());
	for n in [90, 91, 92] {
		assert!(matches!(
			engine::handle(&r.st, &app, &main_push(n)).await.unwrap(),
			Outcome::Waiting
		));
	}
	// Only the newest push is still waiting; the two before it were replaced.
	assert_eq!(r.states().await, ["superseded", "superseded", "waiting_ci"]);
	let cancelled: Vec<_> = posted(c)
		.into_iter()
		.filter(|(_, b)| b["state"] == "error")
		.collect();
	assert_eq!(cancelled.len(), 2, "each replaced commit is told so");
	assert_eq!(cancelled[0].0, format!("ops/app-02@{}", sha(90)));

	set(&c.checks, passed_ci());
	engine::process(&r.st, later(5)).await;
	assert_eq!(r.deployments().await, 1);
	let deployed: String =
		sqlx::query_scalar("SELECT commit_sha FROM autodeploy_pushes WHERE state = 'deployed'")
			.fetch_one(&r.db)
			.await
			.unwrap();
	assert_eq!(deployed, sha(92));
}

#[sqlx::test]
async fn autodeploy_a_newer_push_replaces_a_deployment_that_has_not_started(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	settings(
		&r,
		&app,
		Settings {
			post_status: true,
			..Settings::default()
		},
	)
	.await;
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(100)).await.unwrap(),
		Outcome::Deployed(_)
	));
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(101)).await.unwrap(),
		Outcome::Deployed(_)
	));
	assert_eq!(r.deployments().await, 2);
	assert_eq!(r.states().await, ["superseded", "deployed"]);
	let status_of = |commit: u64| {
		let db = r.db.clone();
		async move {
			sqlx::query_scalar::<_, String>(
				"SELECT d.status FROM deployments d JOIN autodeploy_pushes p ON p.deployment_id = d.id WHERE p.commit_sha = $1",
			)
			.bind(sha(commit))
			.fetch_one(&db)
			.await
			.unwrap()
		}
	};
	assert_eq!(status_of(100).await, "cancelled");
	assert_eq!(status_of(101).await, "queued");
	let states: Vec<(String, String)> = posted(&ge.state.commits)
		.into_iter()
		.map(|(k, b)| (k, b["state"].as_str().unwrap().to_owned()))
		.collect();
	assert!(
		states.contains(&(format!("ops/app-02@{}", sha(100)), "error".to_owned())),
		"{states:?}"
	);
	// The cancelled deployment's job ending does not post a second word about it.
	let dep: i64 =
		sqlx::query_scalar("SELECT deployment_id FROM autodeploy_pushes WHERE commit_sha = $1")
			.bind(sha(100))
			.fetch_one(&r.db)
			.await
			.unwrap();
	let before = posted(&ge.state.commits).len();
	engine::post_result(
		&r.st,
		dep,
		StatusState::Cancelled,
		"The deploy was cancelled.",
	)
	.await;
	assert_eq!(posted(&ge.state.commits).len(), before);
}

// ---- status back ---------------------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_posts_the_deploy_status_in_each_providers_words(db: PgPool) {
	let r = rig(db).await;
	let (gh, hub) = r.github().await;
	let (gl, lab) = r.gitlab().await;
	let (ge, home) = r.gitea().await;
	for (n, app) in [(110, &hub), (111, &lab), (112, &home)] {
		settings(
			&r,
			app,
			Settings {
				post_status: true,
				..Settings::default()
			},
		)
		.await;
		let out = engine::handle(&r.st, app, &main_push(n)).await.unwrap();
		let Outcome::Deployed(d) = out else {
			panic!("{out:?}")
		};
		engine::post_result(&r.st, d.id.0, StatusState::Success, "Deployed with podkit.").await;
		engine::post_result(&r.st, d.id.0, StatusState::Failure, "The deploy failed.").await;
		engine::post_result(
			&r.st,
			d.id.0,
			StatusState::Cancelled,
			"The deploy was cancelled.",
		)
		.await;
	}
	let words = |c: &stubs::Commits, key: &str| -> Vec<String> {
		posted(c)
			.into_iter()
			.map(|(_, b)| b[key].as_str().unwrap().to_owned())
			.collect()
	};
	assert_eq!(
		words(&gh.state.commits, "state"),
		["pending", "success", "failure", "error"]
	);
	assert_eq!(
		words(&ge.state.commits, "state"),
		["pending", "success", "failure", "error"]
	);
	assert_eq!(
		words(&gl.state.commits, "state"),
		["pending", "success", "failed", "canceled"]
	);
	assert!(
		words(&gh.state.commits, "context")
			.iter()
			.all(|c| c == "podkit/deploy")
	);
	assert!(
		words(&ge.state.commits, "context")
			.iter()
			.all(|c| c == "podkit/deploy")
	);
	assert!(
		words(&gl.state.commits, "name")
			.iter()
			.all(|c| c == "podkit/deploy")
	);
	// Every post carries the link back (GitLab as a query parameter).
	for c in [&gh.state.commits, &gl.state.commits, &ge.state.commits] {
		assert!(posted(c).iter().all(|(_, b)| {
			b["target_url"]
				.as_str()
				.is_some_and(|u| u.starts_with("https://podkit.test/teams/"))
		}));
	}
	// The commit and repository are the push's.
	assert_eq!(
		posted(&gh.state.commits)[0].0,
		format!("acme/repo-001@{}", sha(110))
	);
	assert_eq!(
		posted(&gl.state.commits)[0].0,
		format!("group/proj-01@{}", sha(111))
	);
}

#[sqlx::test]
async fn autodeploy_posts_nothing_unless_asked(db: PgPool) {
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	// No settings: the plain behaviour, and nothing is posted.
	let Outcome::Deployed(d) = engine::handle(&r.st, &app, &main_push(120)).await.unwrap() else {
		panic!()
	};
	engine::post_result(&r.st, d.id.0, StatusState::Success, "x").await;
	assert!(posted(&ge.state.commits).is_empty());
	// Settings with the post turned off.
	settings(
		&r,
		&app,
		Settings {
			post_status: false,
			..waits()
		},
	)
	.await;
	engine::handle(&r.st, &app, &main_push(121)).await.unwrap();
	engine::process(&r.st, later(100)).await;
	assert!(posted(&ge.state.commits).is_empty());
	// A provider that cannot be reached does not stop the deploy.
	settings(
		&r,
		&app,
		Settings {
			post_status: true,
			..Settings::default()
		},
	)
	.await;
	sqlx::query("UPDATE git_providers SET base_url = 'http://127.0.0.1:1'")
		.execute(&r.db)
		.await
		.unwrap();
	assert!(matches!(
		engine::handle(&r.st, &app, &main_push(122)).await.unwrap(),
		Outcome::Deployed(_)
	));
}

// ---- the webhooks --------------------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_the_application_webhook_applies_the_rules(db: PgPool) {
	use crate::git_providers::tests::{Headers, post_raw, sign};
	let r = rig(db).await;
	let (ge, app) = r.gitea().await;
	// The secret is returned once at creation; make a known one for this test.
	let secret = "whsec-for-the-test";
	let sealed = r.st.secrets.encrypt(secret).unwrap();
	sqlx::query("UPDATE applications SET webhook_secret = $2 WHERE id = $1")
		.bind(app.id.0)
		.bind(sealed)
		.execute(&r.db)
		.await
		.unwrap();
	settings(
		&r,
		&app,
		Settings {
			wait_for_ci: true,
			ci_timeout_secs: 30,
			path_filters: vec!["api/**".into()],
			..Settings::default()
		},
	)
	.await;
	set(&ge.state.commits.checks, pending_ci());
	*ge.state.commits.files.lock().unwrap() = vec!["api/x.rs".into()];

	let id = app.id.0;
	let go = |n: u64, delivery: &'static str, msg: &str, git_ref: &str| {
		let body = json!({
			"ref": git_ref, "before": sha(n - 1), "after": sha(n), "head_commit": { "message": msg },
			"commits": [{ "id": sha(n), "message": msg, "modified": ["api/x.rs"] }],
		})
		.to_string()
		.into_bytes();
		let headers: Headers = vec![
			("x-gitea-event", "push".to_owned()),
			("x-gitea-delivery", delivery.to_owned()),
			(
				"x-gitea-signature",
				sign(secret, &body).trim_start_matches("sha256=").to_owned(),
			),
		];
		let (app, uri) = (r.app.clone(), format!("/webhooks/deploy/{id}"));
		async move { post_raw(&app, &uri, &headers, &body).await }
	};
	let (s, body) = go(2, "w1", "work", "refs/heads/main").await;
	assert_eq!(
		(s.as_u16(), body["held"].as_str()),
		(202, Some("waiting_ci")),
		"{body}"
	);
	let (s, body) = go(3, "w2", "work [skip deploy]", "refs/heads/main").await;
	assert_eq!(
		(s.as_u16(), body["ignored"].clone(), body["reason"].as_str()),
		(200, json!(true), Some("skipped"))
	);
	let (_, body) = go(4, "w3", "work", "refs/heads/other").await;
	assert_eq!(body["ignored"], json!(true));
	assert!(body.get("reason").is_none());
	assert_eq!(r.deployments().await, 0);
	set(&ge.state.commits.checks, passed_ci());
	engine::process(&r.st, later(5)).await;
	assert_eq!(r.deployments().await, 1);
}

#[sqlx::test]
async fn autodeploy_the_github_app_webhook_decides_per_application_and_holds_tags(db: PgPool) {
	use crate::git_providers::stubs::GITHUB_WEBHOOK_SECRET;
	use crate::git_providers::tests::{github_headers, post_raw, sign};
	let r = rig(db).await;
	let (_gh, one) = r.github().await;
	// A second application on the same repository, following another branch, with rules.
	let (s, two) = crate::tests::call(
		&r.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications",
			r.team, r.place.0
		),
		Some(&r.token),
		Some(json!({
			"name": "second", "slug": "second", "server_id": r.place.1,
			"git_provider_id": one.git_provider_id.unwrap().0,
			"git_repo": "acme/repo-001", "git_ref": "release", "container_port": 80,
		})),
	)
	.await;
	assert!(s.is_success(), "{two}");
	let two = applications::find_by_id(&r.db, ApplicationId(two["id"].as_i64().unwrap()))
		.await
		.unwrap()
		.unwrap();
	settings(
		&r,
		&two,
		Settings {
			branch_patterns: vec!["v*".into()],
			deploy_tags: true,
			..Settings::default()
		},
	)
	.await;

	let hook = format!(
		"/git-providers/github/webhook/{}",
		one.git_provider_id.unwrap()
	);
	let send = |delivery: &'static str, git_ref: &str| {
		let body = json!({
			"ref": git_ref, "before": sha(1), "after": sha(2),
			"repository": { "full_name": "acme/repo-001" },
		})
		.to_string()
		.into_bytes();
		let headers = github_headers(delivery, sign(GITHUB_WEBHOOK_SECRET, &body));
		let (app, hook) = (r.app.clone(), hook.clone());
		async move { post_raw(&app, &hook, &headers, &body).await }
	};
	// main: the first application follows it; the second does not.
	let (s, b) = send("g1", "refs/heads/main").await;
	assert_eq!((s.as_u16(), b["queued"].clone()), (202, json!(1)), "{b}");
	// A tag matching the second application's pattern deploys only that one.
	let (s, b) = send("g2", "refs/tags/v1.0").await;
	assert_eq!((s.as_u16(), b["queued"].clone()), (202, json!(1)), "{b}");
	// A tag nobody wants is not a push anything follows.
	let (_, b) = send("g3", "refs/tags/nightly").await;
	assert_eq!(b["ignored"], "not a branch push");
	assert_eq!(r.deployments().await, 2);
}

// ---- tenancy and permissions ---------------------------------------------------------------

#[sqlx::test]
async fn autodeploy_other_teams_get_not_found(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	settings(&r, &app, waits()).await;
	super::add_freeze(
		&r.db,
		app.id,
		&once_freeze(Duration::hours(-1), Duration::hours(1)),
		r.alice,
	)
	.await
	.unwrap();
	engine::handle(&r.st, &app, &main_push(130)).await.unwrap();
	let push = r.one_push().await;
	let freeze: i64 = sqlx::query_scalar("SELECT id FROM autodeploy_freezes")
		.fetch_one(&r.db)
		.await
		.unwrap();

	let (_, bob_team) = user_with_team(&r.app, "bob").await;
	let bob = UserId(user_id(&r.db, "bob").await);
	let (team, project) = (TeamId(bob_team), r.project());
	let missing = |e: ServerError| matches!(e, ServerError::ApplicationNotFound);
	assert!(missing(
		service::get(&r.st, bob, team, project, app.id)
			.await
			.err()
			.unwrap()
	));
	assert!(missing(
		service::save(&r.st, bob, team, project, app.id, &SettingsInput::default())
			.await
			.err()
			.unwrap()
	));
	let fz = once_input("2030-01-01T00:00", "2030-01-02T00:00");
	assert!(missing(
		service::add_freeze(&r.st, bob, team, project, app.id, &fz)
			.await
			.err()
			.unwrap()
	));
	assert!(missing(
		service::remove_freeze(&r.st, bob, team, project, app.id, freeze)
			.await
			.err()
			.unwrap()
	));
	assert!(missing(
		service::force(&r.st, bob, team, project, app.id, push)
			.await
			.err()
			.unwrap()
	));
	// Nothing of Alice's changed.
	assert_eq!(r.states().await, ["waiting_ci"]);
	assert_eq!(count(&r.db, "autodeploy_freezes").await, 1);
	assert_eq!(r.deployments().await, 0);
	assert_eq!(
		r.audited("app.autodeploy.force").await + r.audited("app.autodeploy").await,
		0
	);
}

#[sqlx::test]
async fn autodeploy_a_freeze_of_another_application_is_not_removable_by_its_id(db: PgPool) {
	let r = rig(db).await;
	let (_ge, one) = r.gitea().await;
	let (_gl, two) = r.gitlab().await;
	super::add_freeze(
		&r.db,
		two.id,
		&once_freeze(Duration::hours(1), Duration::hours(2)),
		r.alice,
	)
	.await
	.unwrap();
	let freeze: i64 = sqlx::query_scalar("SELECT id FROM autodeploy_freezes")
		.fetch_one(&r.db)
		.await
		.unwrap();
	let e =
		service::remove_freeze(&r.st, r.alice, TeamId(r.team), r.project(), one.id, freeze).await;
	assert!(matches!(e, Err(ServerError::Validation(_))), "{e:?}");
	assert_eq!(count(&r.db, "autodeploy_freezes").await, 1);
}

#[sqlx::test]
async fn autodeploy_a_role_without_the_permission_is_refused(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	super::add_freeze(
		&r.db,
		app.id,
		&once_freeze(Duration::hours(-1), Duration::hours(1)),
		r.alice,
	)
	.await
	.unwrap();
	engine::handle(&r.st, &app, &main_push(140)).await.unwrap();
	let push = r.one_push().await;
	let freeze: i64 = sqlx::query_scalar("SELECT id FROM autodeploy_freezes")
		.fetch_one(&r.db)
		.await
		.unwrap();

	// Carol is in the team with a role that can read projects and nothing else.
	user_with_team(&r.app, "carol").await;
	let carol = UserId(user_id(&r.db, "carol").await);
	sqlx::query("DELETE FROM team_members WHERE user_id = $1")
		.bind(carol.0)
		.execute(&r.db)
		.await
		.unwrap();
	join_as_project_reader(&r.db, r.team, carol.0, 9_100).await;
	let (team, project) = (TeamId(r.team), r.project());
	let forbidden = |e: ServerError| matches!(e, ServerError::Forbidden);
	assert!(forbidden(
		service::get(&r.st, carol, team, project, app.id)
			.await
			.err()
			.unwrap()
	));
	assert!(forbidden(
		service::save(
			&r.st,
			carol,
			team,
			project,
			app.id,
			&SettingsInput::default()
		)
		.await
		.err()
		.unwrap()
	));
	assert!(forbidden(
		service::remove_freeze(&r.st, carol, team, project, app.id, freeze)
			.await
			.err()
			.unwrap()
	));
	assert!(forbidden(
		service::force(&r.st, carol, team, project, app.id, push)
			.await
			.err()
			.unwrap()
	));
	assert_eq!(r.deployments().await, 0);
	assert_eq!(count(&r.db, "autodeploy_settings").await, 0);
}

#[sqlx::test]
async fn autodeploy_saving_rules_and_freezes_is_audited_and_stored(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	let team = TeamId(r.team);
	let input = SettingsInput {
		enabled: true,
		branches: "main".into(),
		wait_for_ci: true,
		ci_timeout_min: "5".into(),
		post_status: true,
		..SettingsInput::default()
	};
	let saved = service::save(&r.st, r.alice, team, r.project(), app.id, &input)
		.await
		.unwrap();
	assert_eq!(saved.ci_timeout_secs, 300);
	assert_eq!(
		super::settings(&r.db, app.id).await.unwrap().unwrap(),
		saved
	);
	assert_eq!(r.audited("app.autodeploy").await, 1);
	let fz = once_input("2030-01-01T00:00", "2030-01-02T00:00");
	service::add_freeze(&r.st, r.alice, team, r.project(), app.id, &fz)
		.await
		.unwrap();
	assert_eq!(r.audited("app.autodeploy.freeze_add").await, 1);
	let view = service::get(&r.st, r.alice, team, r.project(), app.id)
		.await
		.unwrap();
	assert_eq!((view.freezes.len(), view.frozen_now), (1, false));
	service::remove_freeze(
		&r.st,
		r.alice,
		team,
		r.project(),
		app.id,
		view.freezes[0].id,
	)
	.await
	.unwrap();
	assert_eq!(r.audited("app.autodeploy.freeze_remove").await, 1);
	// A bad rule changes nothing.
	let bad = SettingsInput {
		ci_timeout_min: "0".into(),
		..input
	};
	assert!(
		service::save(&r.st, r.alice, team, r.project(), app.id, &bad)
			.await
			.is_err()
	);
	assert_eq!(
		super::settings(&r.db, app.id)
			.await
			.unwrap()
			.unwrap()
			.ci_timeout_secs,
		300
	);
}

#[sqlx::test]
async fn autodeploy_keeps_only_the_newest_finished_pushes(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	for n in 2..40 {
		engine::handle(
			&r.st,
			&app,
			&push_of("refs/heads/main", n - 1, n, "[skip deploy]", &[]),
		)
		.await
		.unwrap();
	}
	assert_eq!(count(&r.db, "autodeploy_pushes").await, 30);
	let newest: String =
		sqlx::query_scalar("SELECT commit_sha FROM autodeploy_pushes ORDER BY id DESC LIMIT 1")
			.fetch_one(&r.db)
			.await
			.unwrap();
	assert_eq!(newest, sha(39));
}

#[sqlx::test]
async fn autodeploy_an_application_with_no_rules_still_deploys_at_once(db: PgPool) {
	let r = rig(db).await;
	let (_ge, app) = r.gitea().await;
	let out = engine::handle(&r.st, &app, &main_push(150)).await.unwrap();
	assert!(matches!(out, Outcome::Deployed(_)), "{out:?}");
	assert!(State::from_stored("deployed").is_some());
	assert_eq!(
		super::settings(&r.db, app.id).await.unwrap(),
		None,
		"nothing was stored for it"
	);
}
