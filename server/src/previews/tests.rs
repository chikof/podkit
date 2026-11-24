//! Previews: reading the three providers' pull request events, what a preview is made of (a
//! fresh database, only the variables meant for it, nothing of production's networks), the fork
//! policy and `/preview`, limits and the idle sweep, the pull request comment, teardown, and the
//! rules every slice follows (other teams get 404, a role without the permission is refused).
//! Names start with `previews` so `cargo test -p server -- previews` runs them all.

#![allow(
	clippy::many_single_char_names,
	reason = "r is the rig, w the world, p the preview"
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::event::{self, Event};
use super::plan::{self, DbIn, Fate, Input, Plan, VarIn, Why};
use super::service::{self, SettingsInput};
use super::{Preview, Settings, State, build, comment, policy, run};
use crate::applications::{self, Application};
use crate::error::ServerError;
use crate::git_providers::stubs::{
	self, GITEA_TOKEN, GITHUB_WEBHOOK_SECRET, GITLAB_TOKEN, PrStub, Pulls, Running,
};
use crate::git_providers::tests::{
	Headers, connect_github, create_app, post_raw, project_and_server, sign, token_provider, world,
};
use crate::git_providers::{Kind, service as providers};
use crate::ids::{
	ApplicationId, DatabaseId, EnvVarId, GitProviderId, JobId, PreviewId, ProjectId, ServerId,
	SharedVarId, TeamId, UserId,
};
use crate::jobs::{Job, JobKind, JobStatus};
use crate::shared_env::{self, Scope, SharedVar};
use crate::state::AppState;
use crate::tests::{count, join_as_project_reader, user_id, user_with_team};
use crate::{env_vars, previews};

/// A password that must never reach a preview.
const PROD_PASSWORD: &str = "prod-db-password-7f3a9c";
/// A variable value that must never reach a preview.
const PROD_SECRET: &str = "prod-only-secret-91b2e0";
const HOOK_SECRET: &str = "hook-secret-for-previews";

fn sha(n: u64) -> String {
	format!("{n:040x}")
}

// ---- the events -------------------------------------------------------------------------

#[test]
fn previews_event_github_and_gitea_pull_requests() {
	for (kind, sync) in [
		(Kind::GithubApp, "synchronize"),
		(Kind::Gitea, "synchronized"),
	] {
		let body = |action: &str, merged: bool| {
			json!({
				"action": action, "number": 7,
				"pull_request": {
					"number": 7, "title": "Add things", "merged": merged,
					"head": { "ref": "feature", "sha": sha(1), "repo": { "full_name": "bob/app" } },
					"base": { "ref": "main", "repo": { "full_name": "acme/app" } },
				},
				"repository": { "full_name": "acme/app" },
			})
			.to_string()
			.into_bytes()
		};
		let parse =
			|action: &str, merged: bool| event::parse(kind, "pull_request", &body(action, merged));
		let Some(Event::Opened(pr)) = parse("opened", false) else {
			panic!("opened");
		};
		assert_eq!(
			(
				pr.repo.as_str(),
				pr.number,
				pr.head_ref.as_str(),
				pr.base_ref.as_str()
			),
			("acme/app", 7, "feature", "main")
		);
		assert_eq!(
			(pr.head_repo.as_str(), pr.head_sha.as_str()),
			("bob/app", sha(1).as_str())
		);
		assert!(pr.is_fork(), "a head in another repository is a fork");
		assert!(matches!(parse("reopened", false), Some(Event::Opened(_))));
		assert!(matches!(parse(sync, false), Some(Event::Synced(_))));
		assert!(matches!(
			parse("closed", false),
			Some(Event::Closed { merged: false, .. })
		));
		assert!(matches!(
			parse("closed", true),
			Some(Event::Closed { merged: true, .. })
		));
		// Labels, edits and assignments are not a push.
		for other in ["labeled", "edited", "assigned", "review_requested"] {
			assert_eq!(parse(other, false), None, "{other}");
		}
	}
}

#[test]
fn previews_event_gitlab_merge_requests() {
	let body = |action: &str, oldrev: Option<&str>| {
		let mut v = json!({
			"object_kind": "merge_request",
			"project": { "path_with_namespace": "group/proj" },
			"object_attributes": {
				"iid": 3, "title": "Add things", "source_branch": "feature", "target_branch": "main",
				"action": action, "last_commit": { "id": sha(2) },
				"source": { "path_with_namespace": "group/proj" },
				"target": { "path_with_namespace": "group/proj" },
			},
		});
		if let Some(o) = oldrev {
			v["object_attributes"]["oldrev"] = json!(o);
		}
		v.to_string().into_bytes()
	};
	let parse =
		|a: &str, o: Option<&str>| event::parse(Kind::Gitlab, "Merge Request Hook", &body(a, o));
	let Some(Event::Opened(pr)) = parse("open", None) else {
		panic!("open");
	};
	assert_eq!(
		(pr.number, pr.repo.as_str(), pr.head_sha.as_str()),
		(3, "group/proj", sha(2).as_str())
	);
	assert!(!pr.is_fork(), "the same project is not a fork");
	assert!(matches!(parse("reopen", None), Some(Event::Opened(_))));
	assert!(matches!(
		parse("update", Some(&sha(1))),
		Some(Event::Synced(_))
	));
	// An update with no new commits is a title or a label.
	assert_eq!(parse("update", None), None);
	assert!(matches!(
		parse("close", None),
		Some(Event::Closed { merged: false, .. })
	));
	assert!(matches!(
		parse("merge", None),
		Some(Event::Closed { merged: true, .. })
	));
	assert_eq!(parse("approved", None), None);
}

#[test]
fn previews_event_comments_and_the_preview_command() {
	let hub = |kind: Kind, pull: bool| {
		let mut v = json!({
			"action": "created", "is_pull": pull,
			"issue": { "number": 4 },
			"comment": { "body": "/preview", "user": { "login": "maint", "id": 5 } },
			"repository": { "full_name": "acme/app" },
		});
		if pull {
			v["issue"]["pull_request"] = json!({});
		}
		event::parse(kind, "issue_comment", v.to_string().as_bytes())
	};
	for kind in [Kind::GithubApp, Kind::Gitea] {
		let Some(Event::Comment {
			repo,
			number,
			body,
			login,
			user_id,
		}) = hub(kind, true)
		else {
			panic!("{kind:?}");
		};
		assert_eq!(
			(
				repo.as_str(),
				number,
				body.as_str(),
				login.as_str(),
				user_id
			),
			("acme/app", 4, "/preview", "maint", Some(5))
		);
		assert_eq!(
			hub(kind, false),
			None,
			"a comment on an issue is not about a preview"
		);
	}
	let note = json!({
		"object_kind": "note", "project": { "path_with_namespace": "group/proj" },
		"user": { "id": 9, "username": "maint" },
		"object_attributes": { "note": "/preview", "noteable_type": "MergeRequest" },
		"merge_request": { "iid": 3 },
	});
	assert!(matches!(
		event::parse(Kind::Gitlab, "Note Hook", note.to_string().as_bytes()),
		Some(Event::Comment {
			number: 3,
			user_id: Some(9),
			..
		})
	));
	let issue = json!({
		"object_kind": "note", "project": { "path_with_namespace": "group/proj" },
		"user": { "id": 9, "username": "maint" },
		"object_attributes": { "note": "/preview", "noteable_type": "Issue" },
	});
	assert_eq!(
		event::parse(Kind::Gitlab, "Note Hook", issue.to_string().as_bytes()),
		None
	);

	assert!(event::asks_for_preview("/preview"));
	assert!(event::asks_for_preview("looks fine\n  /Preview  \nthanks"));
	assert!(!event::asks_for_preview("please run /preview for me"));
	assert!(!event::asks_for_preview("/previews"));
	// Not an event, or not JSON.
	assert_eq!(event::parse(Kind::Gitea, "push", b"{}"), None);
	assert_eq!(event::parse(Kind::Gitea, "pull_request", b"nope"), None);
}

// ---- the plan ---------------------------------------------------------------------------

fn db_in() -> DbIn {
	DbIn {
		id: DatabaseId(5),
		name: "orders".to_owned(),
		url: "postgres://app:pw@podkit-db-5:5432/app".to_owned(),
		host: "podkit-db-5".to_owned(),
		memory_limit_mb: Some(2048),
		cpu_limit: Some(2.0),
	}
}

fn var(key: &str, value: &str, marked: bool) -> VarIn {
	VarIn {
		key: key.to_owned(),
		value: value.to_owned(),
		marked,
	}
}

fn plan_of(vars: &[VarIn], from_fork: bool, settings: &Settings) -> Plan {
	Plan::build(&Input {
		number: 12,
		app_name: "Shop",
		app_slug: "shop",
		memory_limit_mb: Some(1024),
		cpu_limit: Some(2.0),
		vars,
		dbs: &[db_in()],
		from_fork,
		settings,
	})
}

fn fate<'a>(plan: &'a Plan, key: &str) -> &'a Fate {
	&plan.vars.iter().find(|(k, _)| k == key).unwrap().1
}

#[test]
fn previews_plan_a_database_the_variables_use_is_made_again_empty_and_never_reached() {
	let vars = [
		var(
			"DATABASE_URL",
			"postgres://app:pw@podkit-db-5:5432/app",
			false,
		),
		var(
			"DSN",
			"postgres://app:pw@podkit-db-5:5432/app?sslmode=disable",
			false,
		),
		var("DB_HOST", "podkit-db-5", true),
		var("SECRET_KEY", "s3", false),
		var("APP_MODE", "preview", true),
	];
	let plan = plan_of(&vars, false, &Settings::default());
	// One new database, named after the pull request, from the same engine's row; nothing
	// in the plan copies data.
	assert_eq!(plan.databases.len(), 1);
	assert_eq!(plan.databases[0].source, DatabaseId(5));
	assert_eq!(plan.databases[0].name, "orders-pr-12-shop");
	// The URL is rewritten to the preview's own database, wherever it sits in the value.
	assert_eq!(
		fate(&plan, "DATABASE_URL"),
		&Fate::Rewrite {
			template: "{{db:5}}".to_owned()
		}
	);
	assert_eq!(
		fate(&plan, "DSN"),
		&Fate::Rewrite {
			template: "{{db:5}}?sslmode=disable".to_owned()
		}
	);
	let urls = std::collections::HashMap::from([(
		DatabaseId(5),
		"postgres://new:x@podkit-db-9:5432/new".to_owned(),
	)]);
	assert_eq!(
		plan::fill("{{db:5}}?sslmode=disable", &urls),
		"postgres://new:x@podkit-db-9:5432/new?sslmode=disable"
	);
	// A variable that names the host any other way would reach production: left out, even
	// when it is marked.
	assert_eq!(fate(&plan, "DB_HOST"), &Fate::Drop(Why::ReachesProduction));
	// Only what is marked comes along.
	assert_eq!(fate(&plan, "SECRET_KEY"), &Fate::Drop(Why::NotMarked));
	assert_eq!(fate(&plan, "APP_MODE"), &Fate::Keep);
	assert!(!plan.strip_logins);
	assert_eq!(
		(plan.env_slug.as_str(), plan.app_slug.as_str()),
		("pr-12-shop", "pr-12-shop")
	);
	// An application that uses no database gets none.
	let none = plan_of(&[var("APP_MODE", "x", true)], false, &Settings::default());
	assert!(none.databases.is_empty());
}

#[test]
fn previews_plan_a_fork_gets_no_variables_unless_the_application_allows_it() {
	let vars = [
		var(
			"DATABASE_URL",
			"postgres://app:pw@podkit-db-5:5432/app",
			false,
		),
		var("APP_MODE", "preview", true),
	];
	let strict = plan_of(&vars, true, &Settings::default());
	assert_eq!(fate(&strict, "APP_MODE"), &Fate::Drop(Why::Fork));
	// The preview's own database is not production's secret: the fork's code needs it to run.
	assert!(matches!(
		fate(&strict, "DATABASE_URL"),
		Fate::Rewrite { .. }
	));
	assert!(
		strict.strip_logins,
		"no stored git or registry login either"
	);
	let open = plan_of(
		&vars,
		true,
		&Settings {
			fork_variables: true,
			..Settings::default()
		},
	);
	assert_eq!(fate(&open, "APP_MODE"), &Fate::Keep);
	assert!(!open.strip_logins);
	// The team's own pull request never loses them for being a fork.
	assert_eq!(
		fate(&plan_of(&vars, false, &Settings::default()), "APP_MODE"),
		&Fate::Keep
	);
}

#[test]
fn previews_plan_caps_are_lower_than_production() {
	let none = Settings::default();
	let p = plan_of(&[var("A", "1", true)], false, &none);
	assert_eq!(
		(p.memory_mb, p.cpu),
		(512, 1.0),
		"half of 1024 MB and 2 cores"
	);
	assert_eq!(plan::default_memory(None), 512);
	assert_eq!(plan::default_memory(Some(100)), 128);
	assert_eq!(plan::default_memory(Some(4096)), 2048);
	assert!((plan::default_cpu(None) - 0.5).abs() < f32::EPSILON);
	assert!((plan::default_cpu(Some(0.1)) - 0.1).abs() < f32::EPSILON);
	let set = plan_of(
		&[var("A", "1", true)],
		false,
		&Settings {
			memory_limit_mb: Some(300),
			cpu_limit: Some(0.25),
			..none
		},
	);
	assert_eq!(
		(set.memory_mb, set.cpu.to_bits()),
		(300, 0.25_f32.to_bits())
	);
	let with_db = plan_of(
		&[var("U", &db_in().url, false)],
		false,
		&Settings::default(),
	);
	assert_eq!(
		(
			with_db.databases[0].memory_mb,
			with_db.databases[0].cpu.to_bits()
		),
		(512, 1.0_f32.to_bits())
	);
}

#[test]
fn previews_plan_names_stay_within_the_limits() {
	assert_eq!(plan::app_slug(12, "shop"), "pr-12-shop");
	let long = "x".repeat(80);
	let slug = plan::app_slug(123, &long);
	assert!(slug.len() <= 60 && slug.starts_with("pr-123-x"), "{slug}");
	assert!(!plan::app_slug(1, &format!("{}-", "y".repeat(52))).ends_with('-'));
	assert_eq!(plan::env_slug(9, "shop"), "pr-9-shop");
}

#[test]
fn previews_policy_secret_shared_variables_need_the_mark_and_a_trusted_pull_request() {
	let row = |key: &str, secret: bool, preview: bool| SharedVar {
		id: SharedVarId(1),
		project_id: None,
		environment_id: None,
		key: key.to_owned(),
		value: Vec::new(),
		is_secret: secret,
		preview,
	};
	let rows = || {
		vec![
			row("PLAIN", false, false),
			row("SECRET", true, false),
			row("MARKED", true, true),
		]
	};
	let keys = |v: Vec<SharedVar>| v.into_iter().map(|r| r.key).collect::<Vec<_>>();
	assert_eq!(
		keys(policy::restrict(rows(), false, false)),
		["PLAIN", "MARKED"]
	);
	assert_eq!(keys(policy::restrict(rows(), true, false)), ["PLAIN"]);
	assert_eq!(
		keys(policy::restrict(rows(), true, true)),
		["PLAIN", "MARKED"]
	);
}

// ---- the world --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Prov {
	Gitea,
	Gitlab,
	Github,
}

enum Stub {
	Gitea(Running<stubs::Gitea>),
	Gitlab(Running<stubs::Gitlab>),
	Github(Running<stubs::Github>),
}

/// An application cloned through a stubbed provider, with previews on, a Postgres and four
/// variables: the connection URL, the host alone, a secret and one marked for previews.
struct World {
	prov: Prov,
	app: Application,
	stub: Stub,
	repo: &'static str,
	prod_db: DatabaseId,
	prod_url: String,
}

#[derive(Clone, Copy)]
enum Ev {
	Open,
	Sync,
	Close,
	Merge,
}

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

static DELIVERY: AtomicU64 = AtomicU64::new(1);

impl Rig {
	fn project(&self) -> ProjectId {
		ProjectId(self.place.0)
	}

	fn enc(&self, v: &str) -> Vec<u8> {
		self.st.secrets.encrypt(v).unwrap()
	}

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

	async fn world_with(&self, prov: Prov, settings: Settings) -> World {
		let (stub, provider, repo, slug) = match prov {
			Prov::Gitea => {
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
				(Stub::Gitea(ge), p.id, "ops/app-02", "home-app")
			}
			Prov::Gitlab => {
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
				(Stub::Gitlab(gl), p.id, "group/proj-01", "lab-app")
			}
			Prov::Github => {
				let gh = stubs::github().await;
				let p = connect_github(&self.st, &gh.url, self.alice, TeamId(self.team)).await;
				(Stub::Github(gh), p, "acme/repo-001", "hub-app")
			}
		};
		let app = self.app_on(provider, repo, slug).await;
		sqlx::query("UPDATE applications SET webhook_secret = $2 WHERE id = $1")
			.bind(app.id)
			.bind(self.enc(HOOK_SECRET))
			.execute(&self.db)
			.await
			.unwrap();
		previews::save_settings(&self.db, app.id, &settings)
			.await
			.unwrap();
		let mut w = World {
			prov,
			app,
			stub,
			repo,
			prod_db: DatabaseId(0),
			prod_url: String::new(),
		};
		self.seed(&mut w).await;
		w
	}

	async fn world(&self, prov: Prov) -> World {
		self.world_with(
			prov,
			Settings {
				enabled: true,
				..Settings::default()
			},
		)
		.await
	}

	/// A production database in the application's environment and its variables.
	async fn seed(&self, w: &mut World) {
		let id = DatabaseId::generate();
		let credentials =
			json!({ "username": "app", "password": PROD_PASSWORD, "database": "app" });
		sqlx::query(
			"INSERT INTO databases (id, project_id, environment_id, server_id, name, engine, version,
				credentials, memory_limit_mb)
			VALUES ($1, $2, $3, $4, $6, 'postgres', '17', $5, 1024)",
		)
		.bind(id)
		.bind(w.app.project_id)
		.bind(w.app.environment_id)
		.bind(w.app.server_id)
		.bind(self.enc(&credentials.to_string()))
		.bind(match w.prov {
			Prov::Gitea => "orders",
			Prov::Gitlab => "orders-lab",
			Prov::Github => "orders-hub",
		})
		.execute(&self.db)
		.await
		.unwrap();
		let row = crate::databases::find_by_id(&self.db, id)
			.await
			.unwrap()
			.unwrap();
		let conn = build::connection(&self.st, &row).unwrap();
		let app = w.app.id;
		for (k, v) in [
			("DATABASE_URL", conn.url.as_str()),
			("DB_HOST", conn.host.as_str()),
			("SECRET_KEY", PROD_SECRET),
			("APP_MODE", "preview-ok"),
		] {
			env_vars::upsert(&self.db, app, k, &self.enc(v))
				.await
				.unwrap();
		}
		let marked = env_vars::list(&self.db, app)
			.await
			.unwrap()
			.into_iter()
			.find(|v| v.key == "APP_MODE")
			.unwrap();
		previews::set_var_preview(&self.db, app, marked.id, true)
			.await
			.unwrap();
		w.prod_db = id;
		w.prod_url = conn.url;
	}

	async fn preview(&self, w: &World, number: i32) -> Option<Preview> {
		previews::find_for_pr(&self.db, w.app.id, number)
			.await
			.unwrap()
	}

	async fn jobs(&self) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind = 'preview'")
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	/// Preview jobs of the previews `w`'s application has now.
	async fn jobs_for(&self, w: &World) -> i64 {
		sqlx::query_scalar(
			"SELECT COUNT(*) FROM jobs j JOIN previews p ON p.id = j.target_id
			WHERE j.kind = 'preview' AND p.application_id = $1",
		)
		.bind(w.app.id)
		.fetch_one(&self.db)
		.await
		.unwrap()
	}

	/// The variables of the preview's application, decrypted.
	async fn vars_of(&self, p: &Preview) -> Vec<(String, String)> {
		env_vars::list(&self.db, p.preview_application_id.unwrap())
			.await
			.unwrap()
			.into_iter()
			.map(|v| (v.key, self.st.secrets.decrypt(&v.value).unwrap()))
			.collect()
	}

	/// Runs the preview job for `id`, with the database purge the agent would do.
	async fn run_job(&self, id: PreviewId) -> Result<(), crate::jobs::JobFailure> {
		let job = Job {
			id: JobId(9_000_000 + id.0 % 1000),
			kind: JobKind::Preview,
			server_id: ServerId(self.place.1),
			target_id: id.0,
			lock_key: format!("preview:{id}"),
			status: JobStatus::Running,
			error: None,
		};
		let cancel = CancellationToken::new();
		let mut log = self.st.logs.open(job.id).await.unwrap();
		let db = self.db.clone();
		let agent = tokio::spawn(async move {
			loop {
				sqlx::query("DELETE FROM databases WHERE status = 'deleting'")
					.execute(&db)
					.await
					.unwrap();
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		});
		let result = run::run(&self.st, &job, &cancel, &mut log).await;
		agent.abort();
		result
	}
}

impl World {
	fn pulls(&self) -> &Pulls {
		match &self.stub {
			Stub::Gitea(s) => &s.state.pulls,
			Stub::Gitlab(s) => &s.state.pulls,
			Stub::Github(s) => &s.state.pulls,
		}
	}

	/// `(repository@sha, state, context)` of every status posted.
	fn statuses(&self) -> Vec<(String, String, String)> {
		let posted = match &self.stub {
			Stub::Gitea(s) => s.state.commits.posted.lock().unwrap().clone(),
			Stub::Gitlab(s) => s.state.commits.posted.lock().unwrap().clone(),
			Stub::Github(s) => s.state.commits.posted.lock().unwrap().clone(),
		};
		posted
			.into_iter()
			.map(|(k, v)| {
				let context = v["context"]
					.as_str()
					.or_else(|| v["name"].as_str())
					.unwrap_or_default();
				(
					k,
					v["state"].as_str().unwrap_or_default().to_owned(),
					context.to_owned(),
				)
			})
			.filter(|(_, _, c)| c == "podkit/preview")
			.collect()
	}

	fn pr_stub(&self, number: i64, head_repo: &str, sha: &str) -> PrStub {
		PrStub {
			number,
			open: true,
			merged: false,
			title: "Add things".to_owned(),
			sha: sha.to_owned(),
			head_ref: "feature".to_owned(),
			head_repo: head_repo.to_owned(),
			base_ref: "main".to_owned(),
			base_repo: self.repo.to_owned(),
		}
	}

	/// The delivery of a pull request event, as the provider's webhook sends it.
	async fn send(
		&self,
		r: &Rig,
		ev: Ev,
		n: i64,
		sha: &str,
		head_repo: Option<&str>,
	) -> (StatusCode, Value) {
		let head_repo = head_repo.unwrap_or(self.repo);
		let delivery = format!("d{}", DELIVERY.fetch_add(1, Ordering::SeqCst));
		let (body, event, extra): (Value, &str, &str) = match self.prov {
			Prov::Gitlab => {
				let action = match ev {
					Ev::Open => "open",
					Ev::Sync => "update",
					Ev::Close => "close",
					Ev::Merge => "merge",
				};
				let mut v = json!({
					"object_kind": "merge_request",
					"project": { "path_with_namespace": self.repo },
					"object_attributes": {
						"iid": n, "title": "Add things", "source_branch": "feature", "target_branch": "main",
						"action": action, "last_commit": { "id": sha },
						"source": { "path_with_namespace": head_repo },
						"target": { "path_with_namespace": self.repo },
					},
				});
				if matches!(ev, Ev::Sync) {
					v["object_attributes"]["oldrev"] = json!(format!("{:040x}", 0));
				}
				(v, "Merge Request Hook", "")
			}
			Prov::Gitea | Prov::Github => {
				let action = match (ev, self.prov) {
					(Ev::Open, _) => "opened",
					(Ev::Sync, Prov::Github) => "synchronize",
					(Ev::Sync, _) => "synchronized",
					(Ev::Close | Ev::Merge, _) => "closed",
				};
				let v = json!({
					"action": action, "number": n,
					"pull_request": {
						"number": n, "title": "Add things", "merged": matches!(ev, Ev::Merge),
						"head": { "ref": "feature", "sha": sha, "repo": { "full_name": head_repo } },
						"base": { "ref": "main", "repo": { "full_name": self.repo } },
					},
					"repository": { "full_name": self.repo },
				});
				(v, "pull_request", "")
			}
		};
		let _ = extra;
		self.deliver(r, event, &delivery, body).await
	}

	/// A `/preview` comment from `login`.
	async fn comment(&self, r: &Rig, n: i64, text: &str, login: &str) -> (StatusCode, Value) {
		let delivery = format!("d{}", DELIVERY.fetch_add(1, Ordering::SeqCst));
		let (event, body) = match self.prov {
			Prov::Gitlab => (
				"Note Hook",
				json!({
					"object_kind": "note", "project": { "path_with_namespace": self.repo },
					"user": { "id": 5, "username": login },
					"object_attributes": { "note": text, "noteable_type": "MergeRequest" },
					"merge_request": { "iid": n },
				}),
			),
			_ => (
				"issue_comment",
				json!({
					"action": "created", "is_pull": true,
					"issue": { "number": n, "pull_request": {} },
					"comment": { "body": text, "user": { "login": login, "id": 5 } },
					"repository": { "full_name": self.repo },
				}),
			),
		};
		self.deliver(r, event, &delivery, body).await
	}

	async fn deliver(
		&self,
		r: &Rig,
		event: &str,
		delivery: &str,
		body: Value,
	) -> (StatusCode, Value) {
		let bytes = body.to_string().into_bytes();
		let (uri, headers): (String, Headers) = match self.prov {
			Prov::Gitea => (
				format!("/webhooks/deploy/{}", self.app.id),
				vec![
					("x-gitea-event", event.to_owned()),
					("x-gitea-delivery", delivery.to_owned()),
					(
						"x-gitea-signature",
						sign(HOOK_SECRET, &bytes)
							.trim_start_matches("sha256=")
							.to_owned(),
					),
				],
			),
			Prov::Gitlab => (
				format!("/webhooks/deploy/{}", self.app.id),
				vec![
					("x-gitlab-event", event.to_owned()),
					("x-gitlab-event-uuid", delivery.to_owned()),
					("x-gitlab-token", HOOK_SECRET.to_owned()),
				],
			),
			Prov::Github => (
				format!(
					"/git-providers/github/webhook/{}",
					self.app.git_provider_id.unwrap()
				),
				vec![
					("x-github-event", event.to_owned()),
					("x-github-delivery", delivery.to_owned()),
					("x-hub-signature-256", sign(GITHUB_WEBHOOK_SECRET, &bytes)),
				],
			),
		};
		post_raw(&r.app, &uri, &headers, &bytes).await
	}
}

// ---- event handling per provider --------------------------------------------------------

async fn lifecycle(db: PgPool, prov: Prov) {
	let r = rig(db).await;
	let w = r.world(prov).await;
	let environments = count(&r.db, "environments").await;

	// Opened: an environment, a fresh database, a job, a comment and a pending status.
	let (s, b) = w.send(&r, Ev::Open, 7, &sha(1), None).await;
	assert_eq!(s.as_u16(), 202, "{prov:?} {b}");
	let p = r.preview(&w, 7).await.expect("a preview");
	assert_eq!(p.state(), State::Queued);
	let env = p.environment_id.unwrap();
	let env_row = sqlx::query_scalar::<_, String>("SELECT slug FROM environments WHERE id = $1")
		.bind(env)
		.fetch_one(&r.db)
		.await
		.unwrap();
	assert_eq!(env_row, format!("pr-7-{}", w.app.slug));
	assert_eq!(count(&r.db, "environments").await, environments + 1);
	let preview_app = applications::find_by_id(&r.db, p.preview_application_id.unwrap())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(preview_app.slug, format!("pr-7-{}", w.app.slug));
	assert_eq!(preview_app.environment_id, env);
	assert_eq!(
		preview_app.git_ref, "feature",
		"built from the pull request's branch"
	);
	assert!(p.url.contains(&format!("pr-7-{}", w.app.slug)), "{}", p.url);
	assert_eq!(r.jobs().await, 1);
	let created: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM audit_events WHERE action = 'preview.create' AND actor_kind = 'system'",
	)
	.fetch_one(&r.db)
	.await
	.unwrap();
	assert_eq!(created, 1, "podkit made it, on the provider's say-so");
	let bodies = w.pulls().bodies(7);
	assert_eq!(bodies.len(), 1, "{bodies:?}");
	assert!(
		bodies[0].contains("Building") && bodies[0].contains(&sha(1)[..7]),
		"{}",
		bodies[0]
	);
	let statuses = w.statuses();
	assert_eq!(statuses.len(), 1, "{statuses:?}");
	assert_eq!(statuses[0].1, "pending");

	// A push to the branch: the same environment, redeployed, the comment edited in place.
	let (s, b) = w.send(&r, Ev::Sync, 7, &sha(2), None).await;
	assert_eq!(s.as_u16(), 202, "{b}");
	let p = r.preview(&w, 7).await.unwrap();
	assert_eq!(p.head_sha, sha(2));
	assert_eq!(p.environment_id, Some(env));
	assert_eq!(count(&r.db, "environments").await, environments + 1);
	assert_eq!(r.jobs().await, 2);
	let bodies = w.pulls().bodies(7);
	assert_eq!(bodies.len(), 1, "edited, not repeated: {bodies:?}");
	assert!(bodies[0].contains(&sha(2)[..7]), "{}", bodies[0]);

	// Closed: the preview is going; the job takes everything down.
	let (s, _) = w.send(&r, Ev::Close, 7, &sha(2), None).await;
	assert_eq!(s.as_u16(), 202);
	assert_eq!(r.preview(&w, 7).await.unwrap().state(), State::Removing);
	assert_eq!(r.jobs().await, 3);
	r.run_job(p.id).await.unwrap();
	assert!(r.preview(&w, 7).await.is_none());
	assert_eq!(count(&r.db, "environments").await, environments);
	assert!(
		applications::find_by_id(&r.db, preview_app.id)
			.await
			.unwrap()
			.is_none(),
		"the preview's application is gone"
	);
	assert!(
		applications::find_by_id(&r.db, w.app.id)
			.await
			.unwrap()
			.is_some(),
		"and the real one is not"
	);
	assert!(
		crate::databases::find_by_id(&r.db, w.prod_db)
			.await
			.unwrap()
			.is_some()
	);
	assert_eq!(
		count(&r.db, "databases").await,
		1,
		"the preview's database is gone"
	);
	let bodies = w.pulls().bodies(7);
	assert!(bodies[0].contains("Removed"), "{}", bodies[0]);
}

#[sqlx::test]
async fn previews_events_gitea_open_sync_close_and_teardown(db: PgPool) {
	lifecycle(db, Prov::Gitea).await;
}

#[sqlx::test]
async fn previews_events_gitlab_open_sync_close_and_teardown(db: PgPool) {
	lifecycle(db, Prov::Gitlab).await;
}

#[sqlx::test]
async fn previews_events_github_open_sync_close_and_teardown(db: PgPool) {
	lifecycle(db, Prov::Github).await;
}

#[sqlx::test]
async fn previews_events_a_merge_takes_the_preview_down_and_other_events_do_nothing(db: PgPool) {
	let r = rig(db).await;
	for (i, prov) in [Prov::Gitea, Prov::Gitlab].into_iter().enumerate() {
		let w = r.world(prov).await;
		let n = 20 + i64::try_from(i).unwrap();
		w.send(&r, Ev::Open, n, &sha(1), None).await;
		let id = r.preview(&w, i32::try_from(n).unwrap()).await.unwrap().id;
		let (s, _) = w.send(&r, Ev::Merge, n, &sha(1), None).await;
		assert_eq!(s.as_u16(), 202);
		assert_eq!(
			previews::find_by_id(&r.db, id)
				.await
				.unwrap()
				.unwrap()
				.state(),
			State::Removing
		);
		// A close for a pull request nobody made a preview of is nothing.
		let (s, b) = w.send(&r, Ev::Close, 99, &sha(1), None).await;
		assert_eq!(s.as_u16(), 200, "{b}");
		assert!(r.preview(&w, 99).await.is_none());
	}
	assert_eq!(
		r.jobs().await,
		4,
		"an open and a merge for each application"
	);
}

#[sqlx::test]
async fn previews_events_off_wrong_branch_wrong_repository_and_bad_signature(db: PgPool) {
	let r = rig(db).await;
	let off = r
		.world_with(
			Prov::Gitea,
			Settings {
				enabled: false,
				..Settings::default()
			},
		)
		.await;
	let (s, _) = off.send(&r, Ev::Open, 1, &sha(1), None).await;
	assert_eq!(s.as_u16(), 200, "previews are off");
	assert_eq!(count(&r.db, "previews").await, 0);

	let w = r.world(Prov::Gitlab).await;
	// A merge request into another branch is not this application's.
	let mut body = json!({
		"object_kind": "merge_request", "project": { "path_with_namespace": w.repo },
		"object_attributes": {
			"iid": 5, "title": "x", "source_branch": "f", "target_branch": "develop", "action": "open",
			"last_commit": { "id": sha(1) },
			"source": { "path_with_namespace": w.repo }, "target": { "path_with_namespace": w.repo },
		},
	});
	let headers = |token: &str| -> Headers {
		vec![
			("x-gitlab-event", "Merge Request Hook".to_owned()),
			(
				"x-gitlab-event-uuid",
				format!("u{}", DELIVERY.fetch_add(1, Ordering::SeqCst)),
			),
			("x-gitlab-token", token.to_owned()),
		]
	};
	let uri = format!("/webhooks/deploy/{}", w.app.id);
	let (s, _) = post_raw(
		&r.app,
		&uri,
		&headers(HOOK_SECRET),
		body.to_string().as_bytes(),
	)
	.await;
	assert_eq!(s.as_u16(), 200);
	assert_eq!(count(&r.db, "previews").await, 0, "another branch");
	// Another repository.
	body["object_attributes"]["target_branch"] = json!("main");
	body["object_attributes"]["target"]["path_with_namespace"] = json!("other/repo");
	body["project"]["path_with_namespace"] = json!("other/repo");
	post_raw(
		&r.app,
		&uri,
		&headers(HOOK_SECRET),
		body.to_string().as_bytes(),
	)
	.await;
	assert_eq!(count(&r.db, "previews").await, 0, "another repository");
	// Nobody who does not know the secret makes an environment.
	body["project"]["path_with_namespace"] = json!(w.repo);
	body["object_attributes"]["target"]["path_with_namespace"] = json!(w.repo);
	let (s, _) = post_raw(&r.app, &uri, &headers("wrong"), body.to_string().as_bytes()).await;
	assert_eq!(s.as_u16(), 403);
	assert_eq!(count(&r.db, "previews").await, 0);
	assert_eq!(count(&r.db, "environments").await, 1);
}

// ---- what a preview is made of ----------------------------------------------------------

#[sqlx::test]
async fn previews_environment_has_a_fresh_database_and_none_of_production(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	// A user network the production application and database share, and a volume.
	sqlx::query(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name)
		SELECT 8801, $1, $2, 'shared', 'shared', 'user', 'pk-shared'",
	)
	.bind(r.team)
	.bind(w.app.server_id)
	.execute(&r.db)
	.await
	.unwrap();
	for (kind, id) in [("application", w.app.id.0), ("database", w.prod_db.0)] {
		sqlx::query(
			"INSERT INTO resource_networks (network_id, resource_kind, resource_id) VALUES (8801, $1, $2)",
		)
		.bind(kind)
		.bind(id)
		.execute(&r.db)
		.await
		.unwrap();
	}
	crate::volumes::insert(&r.db, w.app.id, "/data")
		.await
		.unwrap();

	w.send(&r, Ev::Open, 3, &sha(1), None).await;
	let p = r.preview(&w, 3).await.unwrap();
	let env = p.environment_id.unwrap();
	let dbs = crate::databases::list_by_environment(&r.db, env)
		.await
		.unwrap();
	assert_eq!(
		dbs.len(),
		1,
		"one fresh database for the one the application uses"
	);
	let fresh = &dbs[0];
	assert_ne!(fresh.id, w.prod_db);
	assert_eq!(fresh.name, format!("orders-pr-3-{}", w.app.slug));
	assert_eq!(
		(fresh.engine, fresh.version.as_str()),
		(crate::databases::EngineKind::Postgres, "17")
	);
	assert!(fresh.public_port.is_none());
	assert_eq!(fresh.memory_limit_mb, Some(512), "half of 1024 MB");
	let creds = crate::databases::decrypt_credentials(&r.st.secrets, fresh).unwrap();
	assert_ne!(creds.password, PROD_PASSWORD, "new credentials");
	assert_eq!(count(&r.db, "databases").await, 2);
	// Nothing was dumped, archived or restored: no data moved.
	assert_eq!(count(&r.db, "backups").await, 0);
	assert_eq!(count(&r.db, "volume_backups").await, 0);
	assert_eq!(count(&r.db, "volume_restores").await, 0);
	let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM jobs")
		.fetch_all(&r.db)
		.await
		.unwrap();
	assert_eq!(kinds, ["preview"], "{kinds:?}");
	// The application's volumes exist again, empty: they have their own names.
	let prod_volumes = crate::volumes::list(&r.db, w.app.id).await.unwrap();
	let preview_volumes = crate::volumes::list(&r.db, p.preview_application_id.unwrap())
		.await
		.unwrap();
	assert_eq!(preview_volumes.len(), 1);
	assert_ne!(preview_volumes[0].name, prod_volumes[0].name);
	// No network of production's.
	for (kind, id) in [
		(
			crate::networks::ResourceKind::Application,
			p.preview_application_id.unwrap().0,
		),
		(crate::networks::ResourceKind::Database, fresh.id.0),
	] {
		let nets = crate::networks::memberships(&r.db, kind, id).await.unwrap();
		assert!(nets.iter().all(|(n, _)| !n.is_user()), "no user network");
		assert!(
			nets.iter()
				.all(|(n, _)| n.environment_id != Some(w.app.environment_id)),
			"not production's"
		);
	}
	// The preview is capped lower than the application.
	let preview_app = applications::find_by_id(&r.db, p.preview_application_id.unwrap())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(preview_app.memory_limit_mb, Some(512));
	assert_eq!(
		preview_app.webhook_secret, None,
		"a push to production does not deploy it"
	);
}

#[sqlx::test]
async fn previews_secret_variables_stay_out_unless_marked(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 4, &sha(1), None).await;
	let p = r.preview(&w, 4).await.unwrap();
	let vars = r.vars_of(&p).await;
	let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
	assert!(keys.contains(&"APP_MODE"), "marked: {keys:?}");
	assert!(!keys.contains(&"SECRET_KEY"), "{keys:?}");
	assert!(
		!keys.contains(&"DB_HOST"),
		"it names production's database host: {keys:?}"
	);
	// The connection variable is there, pointing at the preview's own database.
	let url = &vars.iter().find(|(k, _)| k == "DATABASE_URL").unwrap().1;
	assert_ne!(url, &w.prod_url);
	assert!(!url.contains(&format!("podkit-db-{}", w.prod_db)), "{url}");
	assert!(!url.contains(PROD_PASSWORD));
	assert!(
		vars.iter()
			.any(|(k, v)| k == "PODKIT_PREVIEW" && v == "true")
	);
	assert!(
		vars.iter()
			.any(|(k, v)| k == "PODKIT_PREVIEW_PR" && v == "4")
	);
	for (_, v) in &vars {
		assert!(
			!v.contains(PROD_SECRET) && !v.contains(PROD_PASSWORD),
			"{v}"
		);
	}
	// The marks are the application's: unmarking APP_MODE takes it out of the next preview.
	let var = env_vars::list(&r.db, w.app.id)
		.await
		.unwrap()
		.into_iter()
		.find(|v| v.key == "APP_MODE")
		.unwrap();
	service::set_variable(
		&r.st,
		r.alice,
		TeamId(r.team),
		r.project(),
		w.app.id,
		var.id,
		false,
	)
	.await
	.unwrap();
	w.send(&r, Ev::Open, 5, &sha(1), None).await;
	let later = r.vars_of(&r.preview(&w, 5).await.unwrap()).await;
	assert!(!later.iter().any(|(k, _)| k == "APP_MODE"));
	// A variable of another application is not there to mark.
	let other = service::set_variable(
		&r.st,
		r.alice,
		TeamId(r.team),
		r.project(),
		w.app.id,
		EnvVarId(1),
		true,
	)
	.await;
	assert!(matches!(other, Err(ServerError::EnvVarNotFound)));
}

#[sqlx::test]
async fn previews_shared_secrets_are_resolved_only_when_marked(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	let scope = Scope::Team(TeamId(r.team));
	for (key, secret) in [("PLAIN", false), ("SECRET", true), ("MARKED", true)] {
		shared_env::upsert(&r.db, scope, key, &r.enc("v"), secret, r.alice)
			.await
			.unwrap();
	}
	sqlx::query("UPDATE shared_env_vars SET preview = TRUE WHERE key = 'MARKED'")
		.execute(&r.db)
		.await
		.unwrap();
	w.send(&r, Ev::Open, 6, &sha(1), None).await;
	let p = r.preview(&w, 6).await.unwrap();
	let keys = |env| {
		let (db, secrets) = (r.db.clone(), r.st.secrets.clone());
		let (team, project) = (TeamId(r.team), r.project());
		async move {
			shared_env::resolve::shared(&db, &secrets, team, project, env)
				.await
				.unwrap()
				.into_iter()
				.map(|v| v.key)
				.collect::<Vec<_>>()
		}
	};
	assert_eq!(
		keys(w.app.environment_id).await,
		["MARKED", "PLAIN", "SECRET"],
		"production sees all"
	);
	assert_eq!(
		keys(p.environment_id.unwrap()).await,
		["MARKED", "PLAIN"],
		"a preview does not see SECRET"
	);
}

// ---- forks ------------------------------------------------------------------------------

#[sqlx::test]
async fn previews_fork_policy_waits_for_a_maintainer_and_gets_no_variables(db: PgPool) {
	let r = rig(db).await;
	for prov in [Prov::Gitea, Prov::Gitlab, Prov::Github] {
		let w = r.world(prov).await;
		let envs = count(&r.db, "environments").await;
		let head = "bob/fork";
		*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(1)));

		let (s, _) = w.send(&r, Ev::Open, 9, &sha(1), Some(head)).await;
		assert_eq!(s.as_u16(), 202);
		let p = r.preview(&w, 9).await.unwrap();
		assert_eq!(p.state(), State::AwaitingApproval, "{prov:?}");
		assert!(p.from_fork && !p.approved);
		assert!(
			p.environment_id.is_none(),
			"nothing is built from a fork nobody approved"
		);
		assert_eq!(count(&r.db, "environments").await, envs);
		assert_eq!(r.jobs_for(&w).await, 0);
		assert!(w.pulls().bodies(9)[0].contains("/preview"));

		// Anyone may comment; only someone with write access counts.
		*w.pulls().permission.lock().unwrap() = "read".to_owned();
		let (s, _) = w.comment(&r, 9, "/preview", "stranger").await;
		assert_eq!(s.as_u16(), 200, "{prov:?}");
		assert_eq!(
			r.preview(&w, 9).await.unwrap().state(),
			State::AwaitingApproval
		);
		assert_eq!(r.jobs_for(&w).await, 0);
		*w.pulls().permission.lock().unwrap() = "write".to_owned();
		let (s, _) = w.comment(&r, 9, "just a thought", "maint").await;
		assert_eq!(
			s.as_u16(),
			200,
			"a comment that is not the command is nothing"
		);
		assert_eq!(r.jobs_for(&w).await, 0);
		let (s, b) = w.comment(&r, 9, "/preview", "maint").await;
		assert_eq!(s.as_u16(), 202, "{prov:?} {b}");
		let p = r.preview(&w, 9).await.unwrap();
		assert!(p.approved && p.environment_id.is_some(), "{prov:?}");
		assert_eq!(p.state(), State::Queued);
		assert_eq!(r.jobs_for(&w).await, 1);

		// A fork's preview gets no variable of the application's, marked or not, and no
		// stored login: only the connection to its own empty database.
		let vars = r.vars_of(&p).await;
		let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
		assert!(
			!keys.contains(&"APP_MODE") && !keys.contains(&"SECRET_KEY"),
			"{keys:?}"
		);
		assert!(keys.contains(&"DATABASE_URL"));
		let preview_app = applications::find_by_id(&r.db, p.preview_application_id.unwrap())
			.await
			.unwrap()
			.unwrap();
		assert!(preview_app.git_token.is_none() && preview_app.registry_auth.is_none());
		assert_eq!(
			preview_app.git_repo.as_deref(),
			Some(head),
			"built from the fork's repository"
		);

		// New commits from the fork need a new approval; the old preview keeps running.
		w.send(&r, Ev::Sync, 9, &sha(2), Some(head)).await;
		let p = r.preview(&w, 9).await.unwrap();
		assert_eq!(p.state(), State::AwaitingApproval);
		assert!(!p.approved && p.environment_id.is_some());
		assert_eq!(r.jobs_for(&w).await, 1, "nothing was redeployed");
		*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(2)));
		w.comment(&r, 9, "/preview", "maint").await;
		assert_eq!(r.jobs_for(&w).await, 2);
		assert_eq!(r.preview(&w, 9).await.unwrap().head_sha, sha(2));
	}
}

#[sqlx::test]
async fn previews_fork_with_variables_allowed_gets_the_marked_ones_once_approved(db: PgPool) {
	let r = rig(db).await;
	let w = r
		.world_with(
			Prov::Gitea,
			Settings {
				enabled: true,
				fork_variables: true,
				..Settings::default()
			},
		)
		.await;
	*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(2, "bob/fork", &sha(1)));
	*w.pulls().permission.lock().unwrap() = "admin".to_owned();
	w.send(&r, Ev::Open, 2, &sha(1), Some("bob/fork")).await;
	w.comment(&r, 2, "/preview", "maint").await;
	let vars = r.vars_of(&r.preview(&w, 2).await.unwrap()).await;
	assert!(
		vars.iter().any(|(k, _)| k == "APP_MODE"),
		"marked, and the application allows forks"
	);
	assert!(
		!vars.iter().any(|(k, _)| k == "SECRET_KEY"),
		"unmarked stays out"
	);
	// A fork's preview of the same-repo pull request kind is the same branch of the rules for
	// the team's own: `/preview` there just redeploys.
	let own = r.preview(&w, 2).await.unwrap();
	assert!(own.from_fork);
}

#[sqlx::test]
async fn previews_a_deleted_fork_counts_as_a_fork(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	// The head repository is gone: the payload carries none.
	let (s, _) = w.send(&r, Ev::Open, 8, &sha(1), Some("")).await;
	assert_eq!(s.as_u16(), 202);
	let p = r.preview(&w, 8).await.unwrap();
	assert!(p.from_fork);
	assert_eq!(p.state(), State::AwaitingApproval);
}

// ---- limits, expiry ----------------------------------------------------------------------

#[sqlx::test]
async fn previews_limit_holds_the_next_pull_request_until_a_place_is_free(db: PgPool) {
	let r = rig(db).await;
	let w = r
		.world_with(
			Prov::Gitea,
			Settings {
				enabled: true,
				max_concurrent: 2,
				..Settings::default()
			},
		)
		.await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	w.send(&r, Ev::Open, 2, &sha(2), None).await;
	let (s, _) = w.send(&r, Ev::Open, 3, &sha(3), None).await;
	assert_eq!(s.as_u16(), 202);
	let third = r.preview(&w, 3).await.unwrap();
	assert_eq!(third.state(), State::Limited);
	assert!(third.environment_id.is_none());
	assert!(
		w.pulls().bodies(3)[0].contains("limit"),
		"{:?}",
		w.pulls().bodies(3)
	);
	assert_eq!(r.jobs().await, 2);
	assert_eq!(
		count(&r.db, "environments").await,
		3,
		"production and two previews"
	);

	// A place is free once the first is gone, and a push to the third then takes it.
	w.send(&r, Ev::Close, 1, &sha(1), None).await;
	let first = r.preview(&w, 1).await.unwrap();
	r.run_job(first.id).await.unwrap();
	let (s, _) = w.send(&r, Ev::Sync, 3, &sha(4), None).await;
	assert_eq!(s.as_u16(), 202);
	let third = r.preview(&w, 3).await.unwrap();
	assert_eq!(third.state(), State::Queued, "{}", third.detail);
	assert!(third.environment_id.is_some());
}

#[sqlx::test]
async fn previews_idle_previews_expire_after_the_application_s_days(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	w.send(&r, Ev::Open, 2, &sha(2), None).await;
	let (one, two) = (
		r.preview(&w, 1).await.unwrap(),
		r.preview(&w, 2).await.unwrap(),
	);
	let age = |id: PreviewId, days: i32| {
		let db = r.db.clone();
		async move {
			sqlx::query(
				"UPDATE previews SET last_activity_at = NOW() - make_interval(days => $2) WHERE id = $1",
			)
			.bind(id)
			.bind(days)
			.execute(&db)
			.await
			.unwrap();
		}
	};
	age(one.id, 8).await;
	age(two.id, 6).await;
	service::sweep(&r.st).await;
	assert_eq!(
		previews::find_by_id(&r.db, one.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Removing
	);
	assert_eq!(
		previews::find_by_id(&r.db, two.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Queued,
		"six days is under seven"
	);
	assert_eq!(r.jobs().await, 3, "two builds and one removal");

	// A longer allowance keeps the older one; a sweep never queues a removal twice.
	let mut longer = w_settings(&r, &w).await;
	longer.ttl_days = 10;
	previews::save_settings(&r.db, w.app.id, &longer)
		.await
		.unwrap();
	age(two.id, 9).await;
	service::sweep(&r.st).await;
	assert_eq!(
		previews::find_by_id(&r.db, two.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Queued
	);
	service::sweep(&r.st).await;
	assert_eq!(
		r.jobs().await,
		3,
		"the removal already queued is not queued again"
	);
	// A removal whose job is gone is queued again.
	sqlx::query("DELETE FROM jobs WHERE kind = 'preview' AND target_id = $1")
		.bind(one.id)
		.execute(&r.db)
		.await
		.unwrap();
	service::sweep(&r.st).await;
	assert_eq!(r.jobs().await, 2);
	// Activity keeps a preview: a push touches it.
	age(two.id, 9).await;
	w.send(&r, Ev::Sync, 2, &sha(5), None).await;
	service::sweep(&r.st).await;
	assert_eq!(
		previews::find_by_id(&r.db, two.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Queued
	);
}

async fn w_settings(r: &Rig, w: &World) -> Settings {
	previews::settings(&r.db, w.app.id).await.unwrap().unwrap()
}

// ---- the comment ------------------------------------------------------------------------

#[sqlx::test]
async fn previews_comment_is_made_once_edited_after_and_made_again_if_deleted(db: PgPool) {
	let r = rig(db).await;
	for prov in [Prov::Gitea, Prov::Gitlab, Prov::Github] {
		let w = r.world(prov).await;
		w.send(&r, Ev::Open, 5, &sha(1), None).await;
		let mut p = r.preview(&w, 5).await.unwrap();
		let first = p.comment_id.clone().expect("the comment's id is kept");
		assert_eq!(w.pulls().bodies(5).len(), 1);

		p.state = State::Ready.as_str().to_owned();
		p.url = "http://pr-5.example".to_owned();
		comment::publish(&r.st, &p, &w.app, 3, 7).await;
		let bodies = w.pulls().bodies(5);
		assert_eq!(bodies.len(), 1, "{prov:?}: edited in place");
		assert!(
			bodies[0].contains("Ready") && bodies[0].contains("http://pr-5.example"),
			"{}",
			bodies[0]
		);
		let again = r.preview(&w, 5).await.unwrap();
		assert_eq!(again.comment_id.as_deref(), Some(first.as_str()));
		let ready = w.statuses().last().cloned().unwrap();
		assert_eq!(ready.1, "success", "{prov:?}");

		// Someone deleted the comment: a new one is made and remembered.
		for c in w.pulls().comments.lock().unwrap().iter_mut() {
			c.deleted = true;
		}
		let mut p = r.preview(&w, 5).await.unwrap();
		p.state = State::Failed.as_str().to_owned();
		p.detail = "the migration failed".to_owned();
		comment::publish(&r.st, &p, &w.app, 3, 7).await;
		let bodies = w.pulls().bodies(5);
		assert_eq!(bodies.len(), 1, "{prov:?}");
		assert!(bodies[0].contains("Failed") && bodies[0].contains("the migration failed"));
		let moved = r.preview(&w, 5).await.unwrap();
		assert_ne!(moved.comment_id.as_deref(), Some(first.as_str()));
	}
}

#[test]
fn previews_comment_texts_say_what_is_going_on_and_clip_reasons() {
	let mut p = Preview {
		id: PreviewId(1),
		team_id: TeamId(1),
		project_id: ProjectId(1),
		application_id: ApplicationId(1),
		number: 3,
		title: String::new(),
		head_sha: sha(1),
		head_ref: "f".to_owned(),
		head_repo: String::new(),
		from_fork: false,
		approved: false,
		approved_sha: String::new(),
		state: State::Failed.as_str().to_owned(),
		detail: format!("{}\nsecond line that stays out", "x".repeat(400)),
		environment_id: None,
		preview_application_id: None,
		url: "http://u".to_owned(),
		comment_id: None,
		last_activity_at: time::OffsetDateTime::now_utc(),
		created_at: time::OffsetDateTime::now_utc(),
	};
	let text = comment::body(&p, 3, 7);
	assert!(
		text.contains('…') && !text.contains("second line"),
		"{text}"
	);
	for (state, word) in [
		(State::AwaitingApproval, "Waiting for approval"),
		(State::Limited, "3 previews"),
		(State::Queued, "Building"),
		(State::Building, "Building"),
		(State::Ready, "7 days"),
		(State::Removing, "Removed"),
	] {
		p.state = state.as_str().to_owned();
		assert!(comment::body(&p, 3, 7).contains(word), "{state:?}");
	}
	p.state = State::Ready.as_str().to_owned();
	assert_eq!(
		comment::status(&p).0,
		crate::git_providers::api::StatusState::Success
	);
}

#[sqlx::test]
async fn previews_provider_calls_read_pull_requests_and_who_may_write(db: PgPool) {
	let r = rig(db).await;
	for prov in [Prov::Gitea, Prov::Gitlab, Prov::Github] {
		let w = r.world(prov).await;
		*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(12, "bob/fork", &sha(7)));
		let provider = w.app.git_provider_id.unwrap();
		let pr = providers::pull_request(&r.st, provider, w.repo, 12)
			.await
			.unwrap();
		assert_eq!(
			(pr.number, pr.open, pr.merged),
			(12, true, false),
			"{prov:?}"
		);
		assert_eq!(
			(
				pr.head_sha.as_str(),
				pr.head_ref.as_str(),
				pr.head_repo.as_str()
			),
			(sha(7).as_str(), "feature", "bob/fork")
		);
		assert_eq!(pr.base_ref, "main");
		for (level, writes) in [
			("admin", true),
			("write", true),
			("read", false),
			("none", false),
		] {
			*w.pulls().permission.lock().unwrap() = level.to_owned();
			assert_eq!(
				providers::can_write(&r.st, provider, w.repo, "maint", Some(5))
					.await
					.unwrap(),
				writes,
				"{prov:?} {level}"
			);
		}
		w.pulls().pr.lock().unwrap().as_mut().unwrap().open = false;
		assert!(
			!providers::pull_request(&r.st, provider, w.repo, 12)
				.await
				.unwrap()
				.open
		);
		// Not a name that gets into a path.
		assert!(
			providers::can_write(&r.st, provider, w.repo, "a/../b", Some(5))
				.await
				.is_err() || prov == Prov::Gitlab
		);
		assert!(
			providers::pull_request(&r.st, provider, "../x", 1)
				.await
				.is_err()
		);
	}
}

// ---- failure ----------------------------------------------------------------------------

#[sqlx::test]
async fn previews_a_preview_that_cannot_be_made_says_so_and_notifies(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	// Something already holds the address `pr-1-<app>` on the server.
	sqlx::query(
		"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port)
		VALUES (7001, $1, $2, 'squatter', $3, 'https://example.com/r.git', 80)",
	)
	.bind(w.app.project_id)
	.bind(w.app.server_id)
	.bind(format!("pr-1-{}", w.app.slug))
	.execute(&r.db)
	.await
	.unwrap();
	let environments = count(&r.db, "environments").await;
	let (s, _) = w.send(&r, Ev::Open, 1, &sha(1), None).await;
	assert_eq!(s.as_u16(), 202);
	let p = r.preview(&w, 1).await.unwrap();
	assert_eq!(p.state(), State::Failed);
	assert!(p.detail.contains("taken"), "{}", p.detail);
	assert_eq!(
		count(&r.db, "environments").await,
		environments,
		"nothing half-made stays"
	);
	assert_eq!(count(&r.db, "databases").await, 1);
	assert_eq!(
		count(&r.db, "notification_events").await,
		1,
		"the team is told"
	);
	assert!(w.pulls().bodies(1)[0].contains("Failed"));
	assert_eq!(r.jobs().await, 0);
}

#[sqlx::test]
async fn previews_a_failed_job_is_recorded_on_the_preview(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	let p = r.preview(&w, 1).await.unwrap();
	run::fail(
		&r.st,
		p.id.0,
		&crate::jobs::JobFailure::Failed("the build broke".to_owned()),
	)
	.await
	.unwrap();
	let p = previews::find_by_id(&r.db, p.id).await.unwrap().unwrap();
	assert_eq!(
		(p.state(), p.detail.as_str()),
		(State::Failed, "the build broke")
	);
	// A preview that was being removed stays so: the sweep tries again.
	service::remove(&r.st, r.alice, TeamId(r.team), r.project(), w.app.id, p.id)
		.await
		.unwrap();
	run::fail(&r.st, p.id.0, &crate::jobs::JobFailure::Cancelled)
		.await
		.unwrap();
	assert_eq!(
		previews::find_by_id(&r.db, p.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Removing
	);
}

// ---- the page's use cases ---------------------------------------------------------------

#[sqlx::test]
async fn previews_settings_are_validated_saved_audited_and_turn_the_webhook_on(db: PgPool) {
	let r = rig(db).await;
	let w = r.world_with(Prov::Gitea, Settings::default()).await;
	let audit0 = count(&r.db, "audit_events").await;
	let (team, project) = (TeamId(r.team), r.project());
	let save = |f: &dyn Fn(&mut SettingsInput)| {
		let mut input = SettingsInput {
			enabled: true,
			max_concurrent: "3".to_owned(),
			ttl_days: "7".to_owned(),
			..SettingsInput::default()
		};
		f(&mut input);
		let (st, alice, app) = (r.st.clone(), r.alice, w.app.id);
		async move { service::save(&st, alice, team, project, app, &input).await }
	};
	for bad in [
		save(&|i| i.max_concurrent = "0".to_owned()).await,
		save(&|i| i.max_concurrent = "21".to_owned()).await,
		save(&|i| i.ttl_days = "0".to_owned()).await,
		save(&|i| i.ttl_days = "91".to_owned()).await,
		save(&|i| i.memory_limit_mb = "8".to_owned()).await,
		save(&|i| i.cpu_limit = "-1".to_owned()).await,
		save(&|i| i.max_concurrent = "many".to_owned()).await,
		save(&|i| i.command = "x".repeat(501)).await,
	] {
		assert!(matches!(bad, Err(ServerError::Validation(_))), "{bad:?}");
	}
	assert_eq!(
		count(&r.db, "audit_events").await,
		audit0,
		"a refused form is not recorded"
	);
	let saved = save(&|i| {
		i.command = "  ./manage migrate && ./manage seed  ".to_owned();
		i.memory_limit_mb = "256".to_owned();
		i.cpu_limit = "0.5".to_owned();
		i.max_concurrent = "2".to_owned();
	})
	.await
	.unwrap();
	assert_eq!(saved.command, "./manage migrate && ./manage seed");
	assert_eq!(
		(saved.max_concurrent, saved.memory_limit_mb),
		(2, Some(256))
	);
	let view = service::get(&r.st, r.alice, team, project, w.app.id)
		.await
		.unwrap();
	assert!(view.settings.enabled && view.has_provider);
	assert_eq!(view.shared_variables, 1, "APP_MODE is marked");
	assert_eq!(count(&r.db, "audit_events").await, audit0 + 1);
	// Turning previews on makes the application's webhook at the provider again, so it also
	// sends pull request events.
	let Stub::Gitea(ge) = &w.stub else {
		unreachable!()
	};
	let made = ge.state.hooks_made.lock().unwrap().clone();
	assert_eq!(
		made.len(),
		2,
		"one when the application was made, one when previews were turned on"
	);
	assert!(
		made[1].1["events"]
			.as_array()
			.unwrap()
			.iter()
			.any(|e| e == "pull_request")
	);
	assert_eq!(
		ge.state.hooks_deleted.lock().unwrap().len(),
		1,
		"the old one was taken down"
	);
	// An application that is not cloned through a provider cannot have previews.
	sqlx::query("UPDATE applications SET git_provider_id = NULL, git_repo = NULL WHERE id = $1")
		.bind(w.app.id)
		.execute(&r.db)
		.await
		.unwrap();
	assert!(matches!(
		save(&|_| {}).await,
		Err(ServerError::Validation(_))
	));
}

#[sqlx::test]
async fn previews_removing_by_hand_takes_the_preview_down(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	w.send(&r, Ev::Open, 2, &sha(1), Some("bob/fork")).await;
	let (team, project) = (TeamId(r.team), r.project());
	let one = r.preview(&w, 1).await.unwrap();
	service::remove(&r.st, r.alice, team, project, w.app.id, one.id)
		.await
		.unwrap();
	assert_eq!(
		previews::find_by_id(&r.db, one.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Removing
	);
	assert_eq!(r.jobs().await, 2);
	// One that never got an environment is simply forgotten.
	let two = r.preview(&w, 2).await.unwrap();
	service::remove(&r.st, r.alice, team, project, w.app.id, two.id)
		.await
		.unwrap();
	assert!(previews::find_by_id(&r.db, two.id).await.unwrap().is_none());
	let removed: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM audit_events WHERE action = 'preview.remove' AND actor_kind = 'user'",
	)
	.fetch_one(&r.db)
	.await
	.unwrap();
	assert_eq!(removed, 2);
}

// ---- the rules every slice follows ------------------------------------------------------

#[sqlx::test]
async fn previews_other_teams_get_not_found(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	let p = r.preview(&w, 1).await.unwrap();

	let (_, bob_team) = user_with_team(&r.app, "bob").await;
	let bob = UserId(user_id(&r.db, "bob").await);
	let (team, project) = (TeamId(bob_team), r.project());
	let not_found = |e: ServerError| matches!(e, ServerError::ApplicationNotFound);
	assert!(not_found(
		service::get(&r.st, bob, team, project, w.app.id)
			.await
			.unwrap_err()
	));
	assert!(not_found(
		service::save(
			&r.st,
			bob,
			team,
			project,
			w.app.id,
			&SettingsInput {
				enabled: false,
				..SettingsInput::default()
			}
		)
		.await
		.unwrap_err()
	));
	assert!(not_found(
		service::remove(&r.st, bob, team, project, w.app.id, p.id)
			.await
			.unwrap_err()
	));
	let var = env_vars::list(&r.db, w.app.id).await.unwrap().remove(0);
	assert!(not_found(
		service::set_variable(&r.st, bob, team, project, w.app.id, var.id, true)
			.await
			.unwrap_err()
	));
	// The scoped lookup never resolves another team's preview.
	assert!(
		previews::find_scoped(&r.db, team, project, p.id)
			.await
			.unwrap()
			.is_none()
	);
	assert!(
		previews::find_scoped(&r.db, TeamId(r.team), project, p.id)
			.await
			.unwrap()
			.is_some()
	);
	assert_eq!(
		previews::find_by_id(&r.db, p.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Queued
	);

	// A preview of another application is not found through this one either.
	let other = r
		.app_on(w.app.git_provider_id.unwrap(), "ops/app-03", "second")
		.await;
	let alice = (TeamId(r.team), r.project());
	assert!(not_found(
		service::remove(&r.st, r.alice, alice.0, alice.1, other.id, p.id)
			.await
			.unwrap_err()
	));
}

#[sqlx::test]
async fn previews_a_role_without_the_permission_is_refused(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 1, &sha(1), None).await;
	let p = r.preview(&w, 1).await.unwrap();
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
		service::get(&r.st, carol, team, project, w.app.id)
			.await
			.unwrap_err()
	));
	assert!(forbidden(
		service::save(
			&r.st,
			carol,
			team,
			project,
			w.app.id,
			&SettingsInput {
				enabled: true,
				..SettingsInput::default()
			}
		)
		.await
		.unwrap_err()
	));
	assert!(forbidden(
		service::remove(&r.st, carol, team, project, w.app.id, p.id)
			.await
			.unwrap_err()
	));
	let var = env_vars::list(&r.db, w.app.id).await.unwrap().remove(0);
	assert!(forbidden(
		service::set_variable(&r.st, carol, team, project, w.app.id, var.id, true)
			.await
			.unwrap_err()
	));
	assert_eq!(
		previews::find_by_id(&r.db, p.id)
			.await
			.unwrap()
			.unwrap()
			.state(),
		State::Queued
	);
}

#[sqlx::test]
async fn previews_the_variable_toggles_follow_the_service_rules(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	let (team, project) = (TeamId(r.team), r.project());

	// The application's keys that previews may read.
	let keys = service::available_keys(&r.st, r.alice, team, project, w.app.id)
		.await
		.unwrap();
	assert!(
		keys.contains("APP_MODE") && !keys.contains("SECRET_KEY"),
		"{keys:?}"
	);

	// A shared secret is marked through the service, and only at its own scope.
	let scope = Scope::Team(team);
	let row = shared_env::upsert(&r.db, scope, "TOKEN", &r.enc("v"), true, r.alice)
		.await
		.unwrap();
	shared_env::service::set_preview(&r.st, r.alice, scope, row.id, true)
		.await
		.unwrap();
	let list = shared_env::service::list(&r.st, r.alice, scope)
		.await
		.unwrap();
	assert!(list.iter().any(|v| v.key == "TOKEN" && v.preview));
	let wrong_scope = Scope::Project(team, project);
	assert!(matches!(
		shared_env::service::set_preview(&r.st, r.alice, wrong_scope, row.id, false).await,
		Err(ServerError::EnvVarNotFound)
	));

	// Another team sees none of it: the scope is not theirs.
	let (_, bob_team) = user_with_team(&r.app, "bob").await;
	let bob = UserId(user_id(&r.db, "bob").await);
	assert!(
		shared_env::service::set_preview(&r.st, bob, scope, row.id, false)
			.await
			.is_err()
	);
	assert!(
		service::available_keys(&r.st, bob, TeamId(bob_team), project, w.app.id)
			.await
			.is_err()
	);
	assert!(
		previews::environment_ids(&r.db, TeamId(bob_team), project)
			.await
			.unwrap()
			.is_empty()
	);
}

#[sqlx::test]
async fn previews_a_preview_environment_is_listed_for_the_badge(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	w.send(&r, Ev::Open, 7, &sha(1), None).await;
	let p = r.preview(&w, 7).await.unwrap();
	let ids = service::environment_ids(&r.st, TeamId(r.team), r.project())
		.await
		.unwrap();
	assert_eq!(ids.len(), 1);
	assert!(ids.contains(&p.environment_id.unwrap()));
}

// ---- pinned to the approved commit ------------------------------------------------------

/// What the agent would be told to build for `p`: the source of its application's next deploy.
async fn built_source(r: &Rig, p: &Preview) -> Result<proto::Source, crate::jobs::JobFailure> {
	let app = build::preview_app(&r.st, p).await.unwrap().unwrap();
	let d = crate::deployments::insert(&r.db, app.id, None, None, None)
		.await
		.unwrap();
	crate::deployments::run::app_source(&r.st, &app, &d).await
}

fn commit_of(source: proto::Source) -> Option<String> {
	match source {
		proto::Source::Git { commit, .. } => commit,
		proto::Source::Image { .. } => panic!("a git source"),
	}
}

#[sqlx::test]
async fn previews_approval_stores_the_sha_and_the_build_request_carries_it(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	let head = "bob/fork";
	*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(1)));
	w.send(&r, Ev::Open, 9, &sha(1), Some(head)).await;
	let p = r.preview(&w, 9).await.unwrap();
	assert_eq!(p.approved_sha, "", "nothing approved yet");
	assert_eq!(p.pin(), previews::Pin::Unapproved);

	*w.pulls().permission.lock().unwrap() = "write".to_owned();
	let (s, b) = w.comment(&r, 9, "/preview", "maint").await;
	assert_eq!(s.as_u16(), 202, "{b}");
	let p = r.preview(&w, 9).await.unwrap();
	assert!(p.approved);
	assert_eq!(
		p.approved_sha,
		sha(1),
		"the head at the time of the comment"
	);

	// The deploy is told the commit, and the build server's request has the same field.
	let source = built_source(&r, &p).await.unwrap_or_else(|_| panic!());
	let wire = serde_json::to_value(&source).unwrap();
	assert_eq!(wire["commit"], sha(1).as_str(), "{wire}");
	assert_eq!(commit_of(source), Some(sha(1)));
}

#[sqlx::test]
async fn previews_a_push_after_approval_revokes_it_and_a_redeploy_is_refused(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Gitea).await;
	let head = "bob/fork";
	*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(1)));
	*w.pulls().permission.lock().unwrap() = "write".to_owned();
	w.send(&r, Ev::Open, 9, &sha(1), Some(head)).await;
	w.comment(&r, 9, "/preview", "maint").await;
	let p = r.preview(&w, 9).await.unwrap();
	assert_eq!(p.approved_sha, sha(1));
	let jobs = r.jobs_for(&w).await;

	// The fork's author pushes something else.
	w.send(&r, Ev::Sync, 9, &sha(2), Some(head)).await;
	let p = r.preview(&w, 9).await.unwrap();
	assert!(!p.approved);
	assert_eq!(
		p.approved_sha,
		sha(1),
		"what may still be running is remembered"
	);
	assert_eq!(p.head_sha, sha(2));
	assert_eq!(
		p.pin(),
		previews::Pin::Unapproved,
		"but it is not a pin any more"
	);
	assert_eq!(p.state(), State::AwaitingApproval);
	assert_eq!(
		r.jobs_for(&w).await,
		jobs,
		"nothing was built from the push"
	);
	let revoked: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = 'preview.revoke'")
			.fetch_one(&r.db)
			.await
			.unwrap();
	assert_eq!(revoked, 1);

	// The comment and the status say what to do.
	let bodies = w.pulls().bodies(9);
	assert!(
		bodies[0].contains("New commits need a new `/preview`")
			&& bodies[0].contains(&sha(2)[..7])
			&& bodies[0].contains(&sha(1)[..7]),
		"{}",
		bodies[0]
	);
	let (_, description) = comment::status(&p);
	assert_eq!(description, "New commits need a new /preview");
	assert!(
		w.statuses().iter().any(|s| s.1 == "pending"),
		"{:?}",
		w.statuses()
	);

	// A manual redeploy of the preview's application, or a job that was already queued,
	// builds nothing.
	assert!(
		built_source(&r, &p).await.is_err(),
		"an unapproved fork is not built"
	);

	// A new /preview approves the new head, and only that.
	*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(2)));
	w.comment(&r, 9, "/preview", "maint").await;
	let p = r.preview(&w, 9).await.unwrap();
	assert_eq!(p.approved_sha, sha(2));
	assert_eq!(
		commit_of(built_source(&r, &p).await.unwrap_or_else(|_| panic!())),
		Some(sha(2))
	);
}

#[sqlx::test]
async fn previews_same_repository_pull_requests_use_the_sha_of_the_event(db: PgPool) {
	let r = rig(db).await;
	let w = r.world(Prov::Github).await;
	w.send(&r, Ev::Open, 7, &sha(1), None).await;
	let p = r.preview(&w, 7).await.unwrap();
	assert!(!p.from_fork);
	assert_eq!(
		commit_of(built_source(&r, &p).await.unwrap_or_else(|_| panic!())),
		Some(sha(1))
	);

	// A push moves the pin with it, with no approval involved.
	w.send(&r, Ev::Sync, 7, &sha(2), None).await;
	let p = r.preview(&w, 7).await.unwrap();
	assert_eq!(
		commit_of(built_source(&r, &p).await.unwrap_or_else(|_| panic!())),
		Some(sha(2))
	);
}

#[sqlx::test]
async fn previews_an_old_agent_cannot_build_a_fork_preview(db: PgPool) {
	use std::sync::Arc;
	use std::sync::atomic::AtomicBool;

	let r = rig(db).await;
	// An agent that answers `/v1/health` like the first release: no `features`, or with
	// `commit-pin` once `upgraded` is set.
	let upgraded = Arc::new(AtomicBool::new(false));
	let flag = upgraded.clone();
	let router = axum::Router::new().route(
		"/v1/health",
		axum::routing::get(move || {
			let features = if flag.load(Ordering::SeqCst) {
				json!(["commit-pin"])
			} else {
				Value::Null
			};
			async move {
				let mut v = json!({ "version": "0.0.9", "podman_version": "5" });
				if !features.is_null() {
					v["features"] = features;
				}
				axum::Json(v)
			}
		}),
	);
	let listener = tokio::net::UnixListener::bind(&r.st.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

	let w = r.world(Prov::Gitea).await;
	let head = "bob/fork";
	*w.pulls().pr.lock().unwrap() = Some(w.pr_stub(9, head, &sha(1)));
	*w.pulls().permission.lock().unwrap() = "write".to_owned();
	w.send(&r, Ev::Open, 9, &sha(1), Some(head)).await;
	w.comment(&r, 9, "/preview", "maint").await;
	let fork = r.preview(&w, 9).await.unwrap();
	w.send(&r, Ev::Open, 7, &sha(1), None).await;
	let same = r.preview(&w, 7).await.unwrap();

	let check = |p: Preview| {
		let r = &r;
		async move {
			let app = build::preview_app(&r.st, &p).await.unwrap().unwrap();
			let server = crate::scope::server(&r.db, p.team_id, app.server_id)
				.await
				.unwrap();
			let client = r.st.agents.client(&r.st, &server).await.unwrap();
			crate::deployments::run::require_commit_pin(&r.st, &app, &client, Some(&sha(1))).await
		}
	};
	let refused = check(fork.clone()).await;
	let Err(crate::jobs::JobFailure::Failed(why)) = refused else {
		panic!("a fork's preview must not build on an old agent");
	};
	assert!(
		why.contains("update podkit-agent") && why.contains("0.0.9"),
		"{why}"
	);
	assert!(
		check(same).await.is_ok(),
		"a same-repository preview only loses the pin, as before"
	);

	upgraded.store(true, Ordering::SeqCst);
	assert!(
		check(fork).await.is_ok(),
		"an agent that reports it may build"
	);
}
