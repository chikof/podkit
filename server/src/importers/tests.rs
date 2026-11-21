//! The importers, from recorded responses: what each source maps to, what is reported as not
//! supported, that no secret ever reaches the file, and that the result plans cleanly against
//! an empty team. `cargo test -p server --all-features -- importers`
//!
//! The fixtures under `server/tests/fixtures/importers/` are the responses of each platform's
//! API, to the shapes its own schemas and source give (see `_provenance` in each). A stub HTTP
//! server serves them, so the fetching is tested as well as the mapping, and the tests can
//! check that an import only ever reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::http::Client;
use super::{Credentials, Imported, Level, Options, Snapshot, Source};
use super::{caprover, coolify, dokploy};
use crate::iac::doc::{self, ProjectDoc};
use crate::tests::{
	PgPool, app, call, count, db_test, member_role, revoke_permission, user_with_team,
};

const COOLIFY: &str = include_str!("../../tests/fixtures/importers/coolify.json");
const DOKPLOY: &str = include_str!("../../tests/fixtures/importers/dokploy.json");
const CAPROVER: &str = include_str!("../../tests/fixtures/importers/caprover.json");

/// The secret values of each fixture that an import carries, and the ones it must leave
/// behind (preview variables, basic-auth passwords, webhook tokens, database passwords that
/// podkit generates anew).
const COOLIFY_SECRETS: [&str; 5] = [
	"coolify-secret-DB-9f3a",
	"coolify-secret-KEY-77b1",
	"coolify-secret-API-5c2d",
	"coolify-secret-WIKI-aa10",
	"https://stats.example.com",
];
const COOLIFY_LEFT: [&str; 3] = [
	"preview-secret-0001",
	"coolify-secret-PGPASS-31",
	"coolify-secret-REDIS-77",
];
const DOKPLOY_SECRETS: [&str; 6] = [
	"dokploy-secret-DB-1e7c",
	"dokploy-secret-KEY-42ab",
	"dokploy-secret-REG-77ff",
	"dokploy-secret-UMAMI-5d",
	"dokploy-secret-GRAFANA-3b",
	"robot",
];
const DOKPLOY_LEFT: [&str; 4] = [
	"dokploy-secret-BASIC-9",
	"dokploy-secret-PGPASS-8a",
	"dokploy-secret-MARIA-1",
	"dokploy-secret-REDIS-6",
];
const CAPROVER_SECRETS: [&str; 3] = [
	"caprover-secret-DB-6d2e",
	"caprover-secret-KEY-91",
	"caprover-secret-GIT-00ee",
];
const CAPROVER_LEFT: [&str; 2] = ["caprover-secret-HOOK-ab", "caprover-secret-UP-1"];

fn fixture(text: &str) -> Snapshot {
	let all: BTreeMap<String, Value> = serde_json::from_str(text).unwrap();
	Snapshot(
		all.into_iter()
			.filter(|(k, _)| !k.starts_with('_'))
			.collect(),
	)
}

fn options() -> Options {
	Options {
		server: "main".to_owned(),
	}
}

fn project<'a>(imported: &'a Imported, slug: &str) -> &'a ProjectDoc {
	imported
		.doc
		.projects
		.get(slug)
		.unwrap_or_else(|| panic!("no project {slug} in {:?}", imported.doc.projects.keys()))
}

fn said(imported: &Imported, level: Level, needle: &str) -> bool {
	imported
		.report
		.notes
		.iter()
		.any(|n| n.level == level && (n.message.contains(needle) || n.at.contains(needle)))
}

#[track_caller]
fn assert_said(imported: &Imported, level: Level, needle: &str) {
	assert!(
		said(imported, level, needle),
		"{level:?} \"{needle}\" is not in the report:\n{}",
		imported.report.lines(imported.source).join("\n")
	);
}

/// The file is text a person commits to git: it holds no secret, and every secret the source
/// has is in the secrets with the value it had.
#[track_caller]
fn assert_secrets_stay_out(imported: &Imported, carried: &[&str], left: &[&str]) {
	let file = doc::render(&imported.doc).unwrap();
	let values: Vec<&String> = imported.secrets.0.values().collect();
	for secret in carried {
		assert!(!file.contains(secret), "{secret} is in the file:\n{file}");
		assert!(
			values.iter().any(|v| v.contains(secret)),
			"{secret} was not carried into the secrets"
		);
	}
	for secret in left {
		assert!(!file.contains(secret), "{secret} is in the file");
		assert!(
			!values.iter().any(|v| v.contains(secret)),
			"{secret} was imported but should not have been"
		);
	}
	let report = serde_json::to_string(&imported.report).unwrap();
	for secret in carried.iter().chain(left) {
		assert!(!report.contains(secret), "{secret} is in the report");
	}
	assert!(file.contains("${secret:"), "variables are placeholders");
	// Round trip: what podkit writes, podkit reads.
	assert_eq!(doc::parse(&file).unwrap(), imported.doc);
}

// ----- Coolify

#[test]
fn coolify_maps_projects_environments_apps_databases_and_stacks() {
	let imported = coolify::map(&fixture(COOLIFY), &options());
	assert_eq!(
		imported.doc.projects.keys().collect::<Vec<_>>(),
		["shop"],
		"a project with nothing in it is not made"
	);
	let shop = project(&imported, "shop");
	assert_eq!(shop.name, "Shop");
	assert_eq!(
		shop.environments.keys().collect::<Vec<_>>(),
		["production", "staging"]
	);

	let production = &shop.environments["production"];
	let web = &production.apps["web"];
	assert_eq!(web.server, "main");
	assert_eq!(web.slug, "web");
	assert_eq!(
		web.repo.as_deref(),
		Some("https://github.com/coollabsio/coolify-examples")
	);
	assert_eq!(web.git_ref.as_deref(), Some("v4.x"));
	assert_eq!(web.build.as_deref(), Some("nixpacks"));
	assert_eq!(web.port, 3000);
	assert_eq!(web.memory_mb, Some(512));
	assert_eq!(web.cpu, Some(1.5));
	assert_eq!(web.domains, ["shop.example.com", "www.shop.example.com"]);
	assert_eq!(web.volumes, ["/app/uploads"]);
	assert_eq!(
		web.env.keys().collect::<Vec<_>>(),
		["DATABASE_URL", "NEXT_PUBLIC_API", "SESSION_KEY"],
		"preview, locked and badly named variables are left out"
	);
	assert!(
		web.env
			.values()
			.all(|v| v.starts_with("${secret:shop/production/web/"))
	);

	let billing = &production.apps["billing"];
	assert_eq!(
		billing.repo.as_deref(),
		Some("git@github.com:acme/billing.git")
	);
	assert_eq!(billing.build.as_deref(), Some("dockerfile"));
	assert_eq!(billing.dockerfile.as_deref(), Some("docker/Dockerfile"));
	assert_eq!(billing.port, 9000);

	let api = &shop.environments["staging"].apps["api"];
	assert_eq!(api.image.as_deref(), Some("ghcr.io/acme/api:1.4.2"));
	assert!(api.repo.is_none());
	assert_eq!(api.port, 8080);
	assert_eq!(api.domains, ["api.staging.example.com"]);
	assert_eq!(api.memory_mb, None, "0 is no limit");
	assert_eq!(api.cpu, None);

	let pg = &production.databases["shop-db"];
	assert_eq!(
		(pg.engine.as_str(), pg.public_port, pg.memory_mb),
		("postgres", Some(5433), Some(1024))
	);
	assert_eq!(pg.version, None, "16 is not offered: the default is used");
	let cache = &production.databases["cache"];
	assert_eq!(cache.engine, "redis");
	assert_eq!(cache.public_port, None);

	let wiki = &production.stacks["wiki"];
	assert!(wiki.compose.contains("ghcr.io/requarks/wiki:2"));
	assert_eq!(wiki.domains["wiki"].host, "wiki.example.com");
	assert_eq!(wiki.env.len(), 1);
	assert!(production.stacks.contains_key("plausible"));

	let counts = &imported.report.counts;
	assert_eq!((counts.projects, counts.environments), (1, 2));
	assert_eq!((counts.apps, counts.databases, counts.stacks), (3, 2, 2));
}

#[test]
fn coolify_maps_scheduled_tasks_to_cron() {
	let imported = coolify::map(&fixture(COOLIFY), &options());
	let cron = &project(&imported, "shop").environments["production"].cron;
	assert_eq!(cron.len(), 1, "{cron:?}");
	let task = cron.values().next().unwrap();
	assert_eq!(task.kind, "exec");
	assert_eq!(task.app.as_deref(), Some("web"));
	assert_eq!(task.schedule, "0 0 * * *");
	assert_eq!(task.timeout, Some(120));
	assert_said(&imported, Level::Skipped, "every other tuesday");
}

#[test]
fn coolify_reports_what_podkit_has_no_field_for() {
	let imported = coolify::map(&fixture(COOLIFY), &options());
	for (level, needle) in [
		(Level::Unmapped, "custom labels are not supported"),
		(Level::Unmapped, "pre-deployment command is not supported"),
		(Level::Unmapped, "install command is not supported"),
		(Level::Unmapped, "base directory is not supported"),
		(Level::Unmapped, "custom health check is not supported"),
		(Level::Unmapped, "preview deployments are not supported"),
		(Level::Unmapped, "auto deploy on push"),
		(Level::Unmapped, "bind mount /var/log/web"),
		(Level::Unmapped, "file mounts are not supported"),
		(Level::Unmapped, "1 preview-deployment variable"),
		(Level::Unmapped, "a domain with a path"),
		(Level::Unmapped, "replicas are not part of the file"),
		(Level::Skipped, "1 variable(s) have no readable value"),
		(Level::Skipped, "\"not a name\" is not a valid name"),
		(Level::Skipped, "mariadb databases are not supported"),
		(
			Level::Skipped,
			"its environment is not in a project this token can read",
		),
		(Level::Skipped, "without a compose file"),
		(Level::Check, "1 build-time variable(s)"),
		(Level::Check, "deploy key"),
		(Level::Check, "taken to be on github.com"),
		(Level::Check, "runs postgres 16-alpine"),
		(Level::Check, "SERVICE_FQDN_*"),
		(Level::Check, "the data is not copied"),
	] {
		assert_said(&imported, level, needle);
	}
	let lines = imported.report.lines(Source::Coolify);
	assert!(
		lines
			.iter()
			.any(|l| l.starts_with("Coolify: custom labels are not supported [not supported]")),
		"{lines:?}"
	);
}

#[test]
fn coolify_secrets_never_reach_the_file() {
	let imported = coolify::map(&fixture(COOLIFY), &options());
	assert_secrets_stay_out(&imported, &COOLIFY_SECRETS, &COOLIFY_LEFT);
}

// ----- Dokploy

#[test]
fn dokploy_maps_applications_compose_and_databases() {
	let imported = dokploy::map(&fixture(DOKPLOY), &options());
	assert_eq!(
		imported.doc.projects.keys().collect::<Vec<_>>(),
		["blog-stack"]
	);
	let blog_stack = project(&imported, "blog-stack");
	assert_eq!(
		blog_stack.environments.keys().collect::<Vec<_>>(),
		["production"],
		"the empty preview environment is not made"
	);
	let env = &blog_stack.environments["production"];

	let blog = &env.apps["blog"];
	assert_eq!(blog.repo.as_deref(), Some("https://github.com/acme/blog"));
	assert_eq!(blog.git_ref.as_deref(), Some("main"));
	assert_eq!(
		blog.build.as_deref(),
		Some("nixpacks"),
		"railpack becomes nixpacks"
	);
	assert_eq!(blog.port, 3000);
	assert_eq!(
		blog.domains,
		["blog.example.com", "blog.example.org"],
		"a disabled domain is left out"
	);
	assert_eq!(blog.memory_mb, Some(512));
	assert_eq!(blog.cpu, Some(2.0));
	assert_eq!(blog.volumes, ["/app/data"]);
	assert_eq!(
		blog.env.keys().collect::<Vec<_>>(),
		["DATABASE_URL", "SECRET_KEY"]
	);

	let umami = &env.apps["umami"];
	assert_eq!(
		umami.image.as_deref(),
		Some("ghcr.io/umami-software/umami:postgresql-latest")
	);
	assert!(
		umami
			.registry_username
			.as_deref()
			.unwrap()
			.starts_with("${secret:")
	);
	assert!(
		umami
			.registry_password
			.as_deref()
			.unwrap()
			.starts_with("${secret:")
	);

	let docs = &env.apps["docs"];
	assert_eq!(
		docs.repo.as_deref(),
		Some("git@git.example.com:acme/docs.git")
	);
	assert_eq!(docs.git_ref.as_deref(), Some("stable"));
	assert_eq!(docs.build.as_deref(), Some("dockerfile"));
	assert_eq!(docs.dockerfile.as_deref(), Some("Dockerfile.prod"));
	assert!(docs.env.is_empty());
	assert!(
		!env.apps.contains_key("static-zip"),
		"an uploaded archive is not imported"
	);

	let grafana = &env.stacks["grafana"];
	assert!(grafana.compose.contains("grafana/grafana:11"));
	assert_eq!(grafana.domains["grafana"].host, "grafana.example.com");
	assert_eq!(grafana.domains["grafana"].port, Some(3000));
	assert_eq!(
		grafana.env.keys().collect::<Vec<_>>(),
		["GF_SECURITY_ADMIN_PASSWORD"]
	);

	let pg = &env.databases["blog-db"];
	assert_eq!(
		(pg.engine.as_str(), pg.version.as_deref(), pg.public_port),
		("postgres", Some("17"), Some(5434))
	);
	assert_eq!(pg.memory_mb, Some(1024));
	let sessions = &env.databases["sessions"];
	assert_eq!(
		(sessions.engine.as_str(), sessions.version.as_deref()),
		("redis", Some("8"))
	);
	assert!(!env.databases.contains_key("old-db"));
}

#[test]
fn dokploy_reports_what_podkit_has_no_field_for() {
	let imported = dokploy::map(&fixture(DOKPLOY), &options());
	for (level, needle) in [
		(Level::Unmapped, "replicas are not part of the file"),
		(Level::Unmapped, "redirects are not supported"),
		(Level::Unmapped, "basic auth is not supported"),
		(Level::Unmapped, "1 published port(s)"),
		(Level::Unmapped, "preview deployments are not supported"),
		(Level::Unmapped, "auto deploy on push"),
		(Level::Unmapped, "build arguments are not supported"),
		(Level::Unmapped, "a custom start command"),
		(Level::Unmapped, "bind mount /app/host"),
		(Level::Unmapped, "file mount /app/config.yml"),
		(Level::Unmapped, "domain middlewares"),
		(Level::Unmapped, "a domain with a path"),
		(Level::Unmapped, "domains for more than one container port"),
		(Level::Unmapped, "a build path inside the repository"),
		(
			Level::Unmapped,
			"a Docker build context other than the repository root",
		),
		(Level::Unmapped, "randomized names"),
		(
			Level::Unmapped,
			"custom environment and start command of a database",
		),
		(Level::Skipped, "an uploaded archive"),
		(Level::Skipped, "mariadb databases are not supported"),
		(
			Level::Skipped,
			"1 line(s) of the variables could not be read",
		),
		(Level::Check, "railpack on Dokploy"),
		(Level::Check, "edge-1"),
		(Level::Check, "no domain to say its port"),
		(Level::Check, "SSH key"),
		(Level::Check, "Docker Swarm stack"),
		(Level::Check, "the data is not copied"),
	] {
		assert_said(&imported, level, needle);
	}
}

#[test]
fn dokploy_secrets_never_reach_the_file() {
	let imported = dokploy::map(&fixture(DOKPLOY), &options());
	assert_secrets_stay_out(&imported, &DOKPLOY_SECRETS, &DOKPLOY_LEFT);
}

// ----- CapRover

#[test]
fn caprover_maps_apps_by_project() {
	let imported = caprover::map(&fixture(CAPROVER), &options());
	assert_eq!(
		imported.doc.projects.keys().collect::<Vec<_>>(),
		["main-site"],
		"the project of CapRover is the project here; the apps no one can import leave no project behind"
	);
	let env = &project(&imported, "main-site").environments["production"];

	let web = &env.apps["web"];
	assert_eq!(web.repo.as_deref(), Some("https://github.com/acme/web"));
	assert_eq!(web.git_ref.as_deref(), Some("main"));
	assert_eq!(web.build.as_deref(), Some("dockerfile"));
	assert_eq!(web.port, 3000);
	assert_eq!(web.domains, ["app.example.com"]);
	assert_eq!(web.volumes, ["/data"]);
	assert_eq!(
		web.env.keys().collect::<Vec<_>>(),
		["DATABASE_URL", "SECRET"]
	);
	assert!(web.git_token.as_deref().unwrap().starts_with("${secret:"));

	let redis = &env.apps["redis"];
	assert_eq!(redis.image.as_deref(), Some("redis:7-alpine"));
	assert_eq!(redis.port, 6379);
	assert_eq!(redis.volumes, ["/data"]);
	assert_eq!(env.apps.len(), 2);
}

#[test]
fn caprover_reports_what_podkit_has_no_field_for() {
	let imported = caprover::map(&fixture(CAPROVER), &options());
	for (level, needle) in [
		(Level::Unmapped, "1 published port(s)"),
		(Level::Unmapped, "replicas are not part of the file"),
		(Level::Unmapped, "a custom nginx configuration"),
		(Level::Unmapped, "HTTP basic auth"),
		(Level::Unmapped, "attached networks"),
		(Level::Unmapped, "app deploy token"),
		(Level::Unmapped, "bind mount /host"),
		(Level::Skipped, "uploaded"),
		(Level::Skipped, "blank"),
		(Level::Check, "captain-definition"),
		(Level::Check, "was not exposed as a web app"),
	] {
		assert_said(&imported, level, needle);
	}
}

#[test]
fn caprover_secrets_never_reach_the_file() {
	let imported = caprover::map(&fixture(CAPROVER), &options());
	assert_secrets_stay_out(&imported, &CAPROVER_SECRETS, &CAPROVER_LEFT);
}

// ----- shared mapping rules

#[test]
fn names_slugs_and_hosts_are_made_unique_across_the_import() {
	let mut snap = fixture(DOKPLOY);
	// Two applications of the same name in two environments, one host claimed twice.
	snap.0.insert(
		"/api/project.all".into(),
		json!([{
			"name": "P", "environments": [
				{ "name": "production", "applications": [{ "applicationId": "x" }], "compose": [], "postgres": [], "mysql": [], "mariadb": [], "mongo": [], "redis": [] },
				{ "name": "Production", "applications": [{ "applicationId": "y" }], "compose": [], "postgres": [], "mysql": [], "mariadb": [], "mongo": [], "redis": [] }
			]
		}]),
	);
	for id in ["x", "y"] {
		snap.0.insert(
			format!("/api/application.one?applicationId={id}"),
			json!({ "name": "Web", "sourceType": "docker", "dockerImage": "nginx:1", "env": "A=1",
				"domains": [{ "host": "Same.Example.com.", "port": 80, "path": "/", "enabled": true }],
				"mounts": [], "ports": [], "redirects": [], "security": [] }),
		);
	}
	let imported = dokploy::map(&snap, &options());
	let p = project(&imported, "p");
	assert_eq!(
		p.environments.keys().collect::<Vec<_>>(),
		["production", "production-2"]
	);
	let a = &p.environments["production"].apps["web"];
	let b = &p.environments["production-2"].apps["web"];
	assert_eq!(
		(a.slug.as_str(), b.slug.as_str()),
		("web", "web-2"),
		"a slug is unique on its server"
	);
	assert_eq!(a.domains, ["same.example.com"]);
	assert!(b.domains.is_empty(), "a host belongs to one application");
	assert_said(
		&imported,
		Level::Skipped,
		"already used by another imported application",
	);
	let names: Vec<&String> = imported.secrets.0.keys().collect();
	assert_eq!(
		names.len(),
		2,
		"each application has its own secret: {names:?}"
	);
}

#[test]
fn memory_sizes_and_database_versions_are_read_as_docker_writes_them() {
	use super::{megabytes, supported_version};
	assert_eq!(megabytes("512m"), Some(512));
	assert_eq!(megabytes("1g"), Some(1024));
	assert_eq!(megabytes("1024M"), Some(1024));
	assert_eq!(megabytes("536870912"), Some(512));
	assert_eq!(megabytes("0"), None);
	assert_eq!(megabytes("lots"), None);
	assert_eq!(
		supported_version("postgres", "17-alpine").as_deref(),
		Some("17")
	);
	assert_eq!(supported_version("postgres", "17.2").as_deref(), Some("17"));
	assert_eq!(supported_version("postgres", "16"), None);
	assert_eq!(
		supported_version("mongodb", "8.0.4").as_deref(),
		Some("8.0")
	);
	assert_eq!(supported_version("mysql", "latest"), None);
}

#[test]
fn credentials_never_show_in_debug_output() {
	let c = Credentials {
		url: "https://c.example.com".into(),
		token: "tok-very-secret".into(),
	};
	assert!(!format!("{c:?}").contains("tok-very-secret"));
}

// ----- fetching, against a stub of each API

/// What the stub of a platform expects and serves.
struct Stub {
	source: Source,
	responses: BTreeMap<String, Value>,
	/// `(header, value)` every request must carry.
	auth: (&'static str, String),
	/// Requests that were not a `GET` (CapRover's login is the one allowed `POST`).
	writes: AtomicUsize,
	requests: AtomicUsize,
}

async fn handle(State(stub): State<Arc<Stub>>, req: Request<Body>) -> Response {
	stub.requests.fetch_add(1, Ordering::Relaxed);
	let path = req
		.uri()
		.path_and_query()
		.map_or_else(String::new, ToString::to_string);
	let headers = req.headers().clone();
	let method = req.method().clone();
	let header = |name: &str| {
		headers
			.get(name)
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned()
	};
	if stub.source == Source::Caprover && path == "/api/v2/login" {
		assert_eq!(header("x-namespace"), "captain");
		let body = axum::body::to_bytes(req.into_body(), 4096).await.unwrap();
		let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
		return if sent["password"] == "right-password" {
			axum::Json(
				json!({ "status": 100, "description": "Login succeeded", "data": { "token": "cap-session" } }),
			)
			.into_response()
		} else {
			axum::Json(json!({ "status": 1106, "description": "Wrong password" })).into_response()
		};
	}
	if method != axum::http::Method::GET {
		stub.writes.fetch_add(1, Ordering::Relaxed);
		return StatusCode::METHOD_NOT_ALLOWED.into_response();
	}
	if header(stub.auth.0) != stub.auth.1 {
		return (StatusCode::UNAUTHORIZED, "token tok-in-error-body").into_response();
	}
	match stub.responses.get(&path) {
		Some(v) => axum::Json(v.clone()).into_response(),
		None => StatusCode::NOT_FOUND.into_response(),
	}
}

async fn serve(source: Source, fixture_text: &str) -> (String, Arc<Stub>) {
	let auth = match source {
		Source::Coolify => ("authorization", "Bearer right-token".to_owned()),
		Source::Dokploy => ("x-api-key", "right-token".to_owned()),
		Source::Caprover => ("x-captain-auth", "cap-session".to_owned()),
	};
	let stub = Arc::new(Stub {
		source,
		responses: fixture(fixture_text).0,
		auth,
		writes: AtomicUsize::new(0),
		requests: AtomicUsize::new(0),
	});
	let router = Router::new().fallback(handle).with_state(stub.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, router).await.unwrap();
	});
	(url, stub)
}

fn creds(source: Source, url: &str) -> Credentials {
	Credentials {
		url: url.to_owned(),
		token: if source == Source::Caprover {
			"right-password"
		} else {
			"right-token"
		}
		.to_owned(),
	}
}

async fn imported_from_stub(source: Source, text: &str) -> (Imported, Arc<Stub>) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let (url, stub) = serve(source, text).await;
	let request = super::service::Request {
		source,
		credentials: creds(source, &url),
		server: "main".to_owned(),
	};
	(super::service::read(&request).await.unwrap(), stub)
}

#[tokio::test]
async fn fetching_gives_the_same_import_as_mapping_the_fixture() {
	type Map = fn(&Snapshot, &Options) -> Imported;
	for (source, text, map) in [
		(Source::Coolify, COOLIFY, coolify::map as Map),
		(Source::Dokploy, DOKPLOY, dokploy::map as Map),
		(Source::Caprover, CAPROVER, caprover::map as Map),
	] {
		let (fetched, stub) = imported_from_stub(source, text).await;
		let direct = map(&fixture(text), &options());
		assert_eq!(
			doc::render(&fetched.doc).unwrap(),
			doc::render(&direct.doc).unwrap(),
			"{source:?}"
		);
		assert_eq!(fetched.secrets.0, direct.secrets.0, "{source:?}");
		assert_eq!(fetched.report.notes, direct.report.notes, "{source:?}");
		assert_eq!(
			stub.writes.load(Ordering::Relaxed),
			0,
			"{source:?}: an import only reads"
		);
		assert!(stub.requests.load(Ordering::Relaxed) > 1);
	}
}

#[tokio::test]
async fn a_refused_credential_is_an_error_that_does_not_echo_it() {
	let _ = rustls::crypto::ring::default_provider().install_default();
	for (source, text) in [
		(Source::Coolify, COOLIFY),
		(Source::Dokploy, DOKPLOY),
		(Source::Caprover, CAPROVER),
	] {
		let (url, _) = serve(source, text).await;
		let wrong = Credentials {
			url,
			token: "tok-in-error-body".to_owned(),
		};
		let message = match Client::connect(source, &wrong).await {
			Err(e) => e.to_string(),
			Ok(client) => client.get("/anything").await.unwrap_err().to_string(),
		};
		assert!(message.starts_with(source.label()), "{message}");
		assert!(!message.contains("tok-in-error-body"), "{message}");
		assert!(
			message.contains("Wrong password") || message.contains("401"),
			"{message}"
		);
	}
}

#[tokio::test]
async fn an_unreachable_or_wrong_address_is_a_clear_error() {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let unreachable = Credentials {
		url: "http://127.0.0.1:9".into(),
		token: "t".into(),
	};
	let err = Client::connect(Source::Coolify, &unreachable)
		.await
		.unwrap()
		.get("/api/v1/projects")
		.await
		.unwrap_err();
	assert!(err.to_string().contains("could not connect"), "{err}");
	let bad = Credentials {
		url: "ftp://x".into(),
		token: "t".into(),
	};
	assert!(Client::connect(Source::Dokploy, &bad).await.is_err());
	let blank = Credentials {
		url: "http://127.0.0.1:9".into(),
		token: " ".into(),
	};
	assert!(Client::connect(Source::Dokploy, &blank).await.is_err());
}

// ----- planning and applying against a team

struct World {
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool, name: &str) -> Self {
		let _ = rustls::crypto::ring::default_provider().install_default();
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, name).await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		Self {
			app,
			db,
			token,
			team,
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	fn url(&self, source: &str, verb: &str) -> String {
		format!("/api/v1/teams/{}/importers/{source}/{verb}", self.team)
	}

	async fn post(&self, source: &str, verb: &str, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"POST",
			&self.url(source, verb),
			Some(&self.token),
			Some(body),
		)
		.await
	}
}

fn read_body(url: &str, server: &str, token: &str) -> Value {
	json!({ "url": url, "token": token, "server": server })
}

#[db_test]
async fn importers_a_plan_against_an_empty_team_makes_everything_and_leaks_nothing(db: PgPool) {
	let w = World::new(db, "alice").await;
	for (source, text, secret, token) in [
		("coolify", COOLIFY, "coolify-secret-DB-9f3a", "right-token"),
		("dokploy", DOKPLOY, "dokploy-secret-DB-1e7c", "right-token"),
		(
			"caprover",
			CAPROVER,
			"caprover-secret-DB-6d2e",
			"right-password",
		),
	] {
		let kind = Source::parse(source).unwrap();
		let (url, _) = serve(kind, text).await;
		let (s, plan) = w
			.post(source, "plan", read_body(&url, &w.server, token))
			.await;
		assert_eq!(s, StatusCode::OK, "{source}: {plan}");
		let projects = plan["projects"].as_array().unwrap();
		assert_eq!(projects.len(), 1, "{source}: {plan}");
		let p = &projects[0]["plan"];
		assert!(
			p["issues"].as_array().unwrap().is_empty(),
			"{source}: no issue stops an apply: {p}"
		);
		assert!(
			p["summary"]["create"].as_u64().unwrap() >= 2,
			"{source}: {p}"
		);
		assert_eq!(p["summary"]["update"], 0);
		assert_eq!(p["summary"]["delete"], 0);
		assert!(
			plan["report"]["notes"].as_array().unwrap().len() > 3,
			"{source}: the report comes with it"
		);
		let file = plan["file"].as_str().unwrap();
		assert!(file.contains("${secret:"), "{file}");
		assert!(
			!plan.to_string().contains(secret),
			"{source}: a secret is in the response"
		);
		assert!(
			!plan.to_string().contains("right-password"),
			"{source}: the credential is in the response"
		);
		// Planning changes nothing.
		assert_eq!(count(&w.db, "applications").await, 0);
		assert_eq!(count(&w.db, "projects").await, 0);
	}
}

#[db_test]
async fn importers_apply_makes_the_projects_and_stores_variables_encrypted(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, stub) = serve(Source::Coolify, COOLIFY).await;
	let (_, plan) = w
		.post("coolify", "plan", read_body(&url, &w.server, "right-token"))
		.await;
	let id = plan["id"].as_str().unwrap().to_owned();
	let reads = stub.requests.load(Ordering::Relaxed);

	let (s, done) = w.post("coolify", "apply", json!({ "id": id })).await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_eq!(
		stub.requests.load(Ordering::Relaxed),
		reads,
		"applying a preview does not read the source again"
	);
	assert_eq!(count(&w.db, "projects").await, 1);
	assert_eq!(count(&w.db, "applications").await, 3);
	assert_eq!(count(&w.db, "databases").await, 2);
	assert_eq!(
		count(&w.db, "env_vars").await,
		4,
		"web has 3 and api 1; stack variables live with the stack"
	);

	let stored = crate::tests::env_var_ciphertext(&w.db, "DATABASE_URL").await;
	assert!(!stored.is_empty());
	assert!(
		!String::from_utf8_lossy(&stored).contains("coolify-secret-DB-9f3a"),
		"the value is encrypted at rest"
	);
	assert!(!done.to_string().contains("coolify-secret"), "{done}");

	// A preview is used once.
	let (s, _) = w.post("coolify", "apply", json!({ "id": id })).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// And what it made is a project, so importing again is refused for the slug.
	let (s, again) = w
		.post(
			"coolify",
			"apply",
			read_body(&url, &w.server, "right-token"),
		)
		.await;
	assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{again}");
	assert!(again.to_string().contains("already exists"), "{again}");
	assert_eq!(count(&w.db, "projects").await, 1);
}

#[db_test]
async fn importers_apply_in_one_request_reads_plans_and_makes(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Caprover, CAPROVER).await;
	let (s, done) = w
		.post(
			"caprover",
			"apply",
			read_body(&url, &w.server, "right-password"),
		)
		.await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_eq!(count(&w.db, "applications").await, 2);
	assert!(
		done["report"]["notes"]
			.as_array()
			.is_some_and(|n| !n.is_empty())
	);
}

#[db_test]
async fn importers_an_unknown_server_name_is_an_issue_and_nothing_is_made(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Dokploy, DOKPLOY).await;
	let (s, out) = w
		.post(
			"dokploy",
			"apply",
			read_body(&url, "no-such-server", "right-token"),
		)
		.await;
	assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
	assert!(out.to_string().contains("no server named"), "{out}");
	assert_eq!(count(&w.db, "projects").await, 0);
}

#[db_test]
async fn importers_a_wrong_credential_or_source_is_refused_before_anything_is_made(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let (s, out) = w
		.post("coolify", "plan", read_body(&url, &w.server, "wrong-token"))
		.await;
	assert_eq!(s, StatusCode::BAD_GATEWAY, "{out}");
	assert!(!out.to_string().contains("wrong-token"), "{out}");
	let (s, _) = w
		.post("heroku", "plan", read_body(&url, &w.server, "x"))
		.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, out) = w
		.post("coolify", "plan", read_body("not a url", &w.server, "x"))
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
	let (s, _) = w
		.post("coolify", "plan", read_body(&url, "", "right-token"))
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
	assert_eq!(count(&w.db, "projects").await, 0);
}

#[db_test]
async fn importers_need_create_project_and_a_write_token(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let body = read_body(&url, &w.server, "right-token");

	let (_, token) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/api-tokens", w.team),
		Some(&w.token),
		Some(json!({ "name": "reader", "scopes": ["read"] })),
	)
	.await;
	let read = token["secret"].as_str().unwrap();
	let (s, out) = call(
		&w.app,
		"POST",
		&w.url("coolify", "plan"),
		Some(read),
		Some(body.clone()),
	)
	.await;
	assert_eq!(
		s,
		StatusCode::OK,
		"a plan changes nothing, so reading is enough: {out}"
	);
	let (s, out) = call(
		&w.app,
		"POST",
		&w.url("coolify", "apply"),
		Some(read),
		Some(body.clone()),
	)
	.await;
	assert_eq!(
		s,
		StatusCode::FORBIDDEN,
		"a read token cannot import: {out}"
	);

	let (_, token) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/api-tokens", w.team),
		Some(&w.token),
		Some(json!({ "name": "writer", "scopes": ["read", "write"] })),
	)
	.await;
	let write = token["secret"].as_str().unwrap();
	let (s, out) = call(
		&w.app,
		"POST",
		&w.url("coolify", "apply"),
		Some(write),
		Some(json!({ "url": url, "token": "right-token", "server": w.server, "deploy": true })),
	)
	.await;
	assert_eq!(
		s,
		StatusCode::FORBIDDEN,
		"deploying needs the deploy scope: {out}"
	);

	let role = member_role(&w.db, w.team, "alice").await;
	revoke_permission(&w.db, role, "create", "project").await;
	let (s, _) = w.post("coolify", "plan", body.clone()).await;
	assert_eq!(s, StatusCode::FORBIDDEN, "without create project");
	let (s, _) = w.post("coolify", "apply", body).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&w.db, "projects").await, 0);
}

#[db_test]
async fn importers_a_preview_belongs_to_its_user_and_team(db: PgPool) {
	let a = World::new(db.clone(), "alice").await;
	let b = World::new(db.clone(), "bob").await;
	let (url, _) = serve(Source::Dokploy, DOKPLOY).await;
	let (_, plan) = a
		.post("dokploy", "plan", read_body(&url, &a.server, "right-token"))
		.await;
	let id = plan["id"].as_str().unwrap();

	// Bob, in his own team, with the id.
	let (s, _) = b.post("dokploy", "apply", json!({ "id": id })).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// Bob, in Alice's team.
	let (s, _) = call(
		&b.app,
		"POST",
		&a.url("dokploy", "apply"),
		Some(&b.token),
		Some(json!({ "id": id })),
	)
	.await;
	assert!(
		s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
		"{s}"
	);
	// Alice, as another source.
	let (s, _) = a.post("coolify", "apply", json!({ "id": id })).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_eq!(count(&db, "projects").await, 0);
	// And it still works for Alice.
	let (s, out) = a.post("dokploy", "apply", json!({ "id": id })).await;
	assert_eq!(s, StatusCode::OK, "{out}");
	assert_eq!(count(&db, "projects").await, 1);
}
