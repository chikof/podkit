//! Dokploy to a `podkit.toml`.
//!
//! Dokploy's API is documented by its OpenAPI file,
//! <https://github.com/Dokploy/dokploy/blob/canary/openapi.json> (served by an instance at
//! `/swagger`; the docs are at <https://docs.dokploy.com/docs/api>). Calls are `GET`s of
//! `{url}/api/<router>.<procedure>?<input>` with the header `x-api-key: <key>` (the file's
//! `securitySchemes.apiKey`; a key is made under Settings, Profile, API/CLI). The OpenAPI file
//! does not describe the responses, so their shapes come from the source of the server:
//!
//! | Request | Response | Source |
//! |---|---|---|
//! | `GET /api/project.all` | projects, each with `environments: [{environmentId, name, isDefault, applications: [{applicationId, name}], compose: [{composeId, name}], postgres: [{postgresId, name}], mysql, mariadb, mongo, redis, libsql}]` | `apps/dokploy/server/api/routers/project.ts` (`all`) |
//! | `GET /api/application.one?applicationId=` | the application row with `domains`, `mounts`, `ports`, `redirects`, `security`, `server` | `packages/server/src/services/application.ts` (`findApplicationById`), columns in `packages/server/src/db/schema/application.ts` |
//! | `GET /api/compose.one?composeId=` | the compose row with `domains`, `mounts` | `services/compose.ts`, `schema/compose.ts` |
//! | `GET /api/postgres.one?postgresId=` (also `mysql`, `mariadb`, `mongo`, `redis`) | the database row with `mounts` | `services/postgres.ts` etc., `schema/postgres.ts` etc. |
//!
//! Domains are `{host, https, port, path, serviceName, enabled}` (`schema/domain.ts`), mounts
//! `{type: bind|volume|file, mountPath, volumeName, hostPath, filePath, content}`
//! (`schema/mount.ts`). An application's `env` is one text of `KEY=value` lines. Memory is in
//! bytes and CPU in units of 10^-9 CPUs (`components/dashboard/application/advanced/show-resources.tsx`).

use serde_json::Value;

use super::http::Client;
use super::{
	AppSpec, Builder, Imported, Options, Place, Snapshot, Source, StackSpec, array, flag,
	managed_database, megabytes, port_of, repo_url, text,
};
use crate::env_vars::service::parse_dotenv;
use crate::error::ServerError;
use crate::iac::doc::{AppDoc, StackDoc, StackDomainDoc};

/// The database routers: the key of the list in `project.all`, and the request.
const DATABASES: [(&str, &str, &str); 5] = [
	("postgres", "postgres", "postgresId"),
	("mysql", "mysql", "mysqlId"),
	("mariadb", "mariadb", "mariadbId"),
	("mongo", "mongo", "mongoId"),
	("redis", "redis", "redisId"),
];

fn one(router: &str, id_name: &str, id: &str) -> String {
	format!("/api/{router}.one?{id_name}={id}")
}

/// Reads everything the mapping needs.
///
/// # Errors
/// `Import` when Dokploy cannot be reached or refuses the key.
pub async fn fetch(client: &Client) -> Result<Snapshot, ServerError> {
	let mut snap = Snapshot::default();
	let projects = client.get("/api/project.all").await?;
	for project in projects.as_array().map_or(&[][..], Vec::as_slice) {
		for env in array(project, "environments") {
			for app in array(env, "applications") {
				if let Some(id) = text(app, "applicationId") {
					let path = one("application", "applicationId", id);
					let v = client.get(&path).await?;
					snap.0.insert(path, v);
				}
			}
			for c in array(env, "compose") {
				if let Some(id) = text(c, "composeId") {
					let path = one("compose", "composeId", id);
					let v = client.get(&path).await?;
					snap.0.insert(path, v);
				}
			}
			for (list, router, id_name) in DATABASES {
				for db in array(env, list) {
					if let Some(id) = text(db, id_name) {
						let path = one(router, id_name, id);
						let v = client.get(&path).await?;
						snap.0.insert(path, v);
					}
				}
			}
		}
	}
	snap.0.insert("/api/project.all".to_owned(), projects);
	Ok(snap)
}

/// An application's `env`: `KEY=value` lines.
fn env_of(b: &mut Builder, at: &str, text_: Option<&str>) -> Vec<(String, String)> {
	let Some(text_) = text_ else {
		return Vec::new();
	};
	let (entries, errors) = parse_dotenv(text_);
	if !errors.is_empty() {
		b.skipped(
			at,
			format!(
				"{} line(s) of the variables could not be read and were left out",
				errors.len()
			),
		);
	}
	entries.into_iter().map(|(_, k, v)| (k, v)).collect()
}

fn new_app(port: i32) -> AppDoc {
	AppDoc {
		server: String::new(),
		slug: String::new(),
		repo: None,
		git_ref: None,
		build: None,
		dockerfile: None,
		image: None,
		port,
		memory_mb: None,
		cpu: None,
		deploy_key: None,
		registry: None,
		git_token: None,
		registry_username: None,
		registry_password: None,
		domains: Vec::new(),
		volumes: Vec::new(),
		env: std::collections::BTreeMap::new(),
		models: Vec::new(),
	}
}

/// Dokploy's CPU limit (10^-9 CPUs) in CPUs.
fn cpus(limit: &str) -> Option<f64> {
	let nano: f64 = limit.trim().parse().ok()?;
	let cpus = nano / 1_000_000_000.0;
	(cpus > 0.0).then_some(cpus)
}

/// Memory in bytes, as a number or a Docker size.
fn memory(limit: &str) -> Option<i32> {
	megabytes(limit.trim()).or_else(|| {
		let bytes: f64 = limit.trim().parse().ok()?;
		megabytes(&format!("{bytes}"))
	})
}

/// The domains of a row: enabled hosts, the first port, and whether any has a path.
struct Routes {
	hosts: Vec<String>,
	port: Option<i32>,
	extra_ports: bool,
}

fn routes(b: &mut Builder, at: &str, row: &Value) -> Routes {
	let mut out = Routes {
		hosts: Vec::new(),
		port: None,
		extra_ports: false,
	};
	for d in array(row, "domains") {
		let Some(host) = text(d, "host") else {
			continue;
		};
		if d.get("enabled") == Some(&Value::Bool(false)) {
			continue;
		}
		if text(d, "path").is_some_and(|p| p != "/") {
			b.unmapped(at, "a domain with a path is routed by hostname only");
		}
		if text(d, "customCertResolver").is_some() || text(d, "customEntrypoint").is_some() {
			b.unmapped(
				at,
				"custom certificate resolvers and entrypoints are not supported",
			);
		}
		if flag(d, "forwardAuthEnabled")
			|| array(d, "middlewares").iter().any(|m| m.as_str().is_some())
		{
			b.unmapped(at, "domain middlewares are not supported");
		}
		let port = d.get("port").and_then(port_of);
		match (out.port, port) {
			(None, Some(p)) => out.port = Some(p),
			(Some(first), Some(p)) if first != p => out.extra_ports = true,
			_ => {}
		}
		out.hosts.push(host.to_owned());
	}
	out
}

fn mounts(b: &mut Builder, at: &str, row: &Value) -> Vec<String> {
	let mut volumes = Vec::new();
	for m in array(row, "mounts") {
		let Some(path) = text(m, "mountPath") else {
			continue;
		};
		match text(m, "type").unwrap_or("volume") {
			"volume" => volumes.push(path.to_owned()),
			"bind" => b.unmapped(
				at,
				format!("bind mount {path} is not supported; only named volumes are"),
			),
			_ => b.unmapped(at, format!("file mount {path} is not supported")),
		}
	}
	volumes
}

/// Settings of an application that podkit has no field for. Reported when they are set.
fn unmapped_settings(b: &mut Builder, at: &str, app: &Value) {
	if text(app, "command").is_some() || !array(app, "args").is_empty() {
		b.unmapped(at, "a custom start command is not supported");
	}
	if app
		.get("replicas")
		.and_then(Value::as_i64)
		.is_some_and(|n| n > 1)
	{
		b.unmapped(at, "replicas are not part of the file");
	}
	if !array(app, "redirects").is_empty() {
		b.unmapped(at, "redirects are not supported");
	}
	if !array(app, "security").is_empty() {
		b.unmapped(at, "basic auth is not supported");
	}
	let published = array(app, "ports").len();
	if published > 0 {
		b.unmapped(
			at,
			format!("{published} published port(s) are not supported"),
		);
	}
	if flag(app, "isPreviewDeploymentsActive") {
		b.unmapped(at, "preview deployments are not supported");
	}
	if app.get("autoDeploy") == Some(&Value::Bool(true)) {
		b.unmapped(
			at,
			"auto deploy on push is not part of the file; set up a webhook",
		);
	}
	for (key, label) in [
		("buildArgs", "build arguments are not supported"),
		("buildSecrets", "build secrets are not supported"),
		("publishDirectory", "the publish directory is not supported"),
		(
			"dockerBuildStage",
			"the Dockerfile build target is not supported",
		),
	] {
		if text(app, key).is_some() {
			b.unmapped(at, label);
		}
	}
	for key in [
		"healthCheckSwarm",
		"restartPolicySwarm",
		"placementSwarm",
		"updateConfigSwarm",
		"rollbackConfigSwarm",
		"modeSwarm",
		"labelsSwarm",
		"networkSwarm",
		"endpointSpecSwarm",
		"ulimitsSwarm",
	] {
		if app.get(key).is_some_and(|v| !v.is_null()) {
			b.unmapped(at, "Docker Swarm service settings are not supported");
			break;
		}
	}
	if let Some(server) = app.get("server").filter(|s| !s.is_null()) {
		b.check(
			at,
			format!(
				"ran on the Dokploy server \"{}\"; it goes to the podkit server you chose",
				text(server, "name").unwrap_or("remote")
			),
		);
	}
}

#[allow(clippy::too_many_lines, reason = "one block per setting of an app")]
fn application(b: &mut Builder, snap: &Snapshot, at: &Place, id: &str) {
	let Some(app) = snap.get(&one("application", "applicationId", id)) else {
		b.skipped(id, "the application could not be read");
		return;
	};
	let name = text(app, "name").unwrap_or("application").to_owned();
	let shown = at.path(&name);
	let route = routes(b, &shown, app);
	let port = route.port.unwrap_or_else(|| {
		b.check(&shown, "it has no domain to say its port; 3000 is assumed");
		3000
	});
	if route.extra_ports {
		b.unmapped(
			&shown,
			"domains for more than one container port: only the first port is kept",
		);
	}
	let mut doc = new_app(port);
	doc.domains = route.hosts;
	doc.memory_mb = text(app, "memoryLimit").and_then(memory);
	doc.cpu = text(app, "cpuLimit").and_then(cpus);
	doc.volumes = mounts(b, &shown, app);
	let mut credentials = Vec::new();

	let source = text(app, "sourceType").unwrap_or("github");
	let git = |b: &mut Builder, url: String, branch: Option<&str>, doc: &mut AppDoc| {
		doc.repo = repo_url(b, &shown, &url);
		doc.git_ref = branch.map(str::to_owned);
	};
	match source {
		"docker" => {
			let Some(image) = text(app, "dockerImage") else {
				b.skipped(&shown, "an image application without an image");
				return;
			};
			doc.image = Some(image.to_owned());
			if let (Some(user), Some(pass)) = (text(app, "username"), text(app, "password")) {
				credentials.push(("registry_username", user.to_owned()));
				credentials.push(("registry_password", pass.to_owned()));
			}
			if text(app, "registryUrl").is_some() {
				b.check(
					&shown,
					"pulled from a registry with its own address: it is taken from the image name",
				);
			}
		}
		"github" => {
			let (Some(owner), Some(repo)) = (text(app, "owner"), text(app, "repository")) else {
				b.skipped(&shown, "a GitHub application without a repository");
				return;
			};
			git(
				b,
				format!("https://github.com/{owner}/{repo}"),
				text(app, "branch"),
				&mut doc,
			);
			b.check(
				&shown,
				"from a GitHub App on Dokploy: connect a git provider in podkit if the repository is private",
			);
		}
		"gitlab" => {
			let (Some(owner), Some(repo)) =
				(text(app, "gitlabOwner"), text(app, "gitlabRepository"))
			else {
				b.skipped(&shown, "a GitLab application without a repository");
				return;
			};
			git(
				b,
				format!("https://gitlab.com/{owner}/{repo}"),
				text(app, "gitlabBranch"),
				&mut doc,
			);
			b.check(
				&shown,
				"taken to be on gitlab.com: check the address, and connect a git provider if it is private",
			);
		}
		"bitbucket" => {
			let (Some(owner), Some(repo)) = (
				text(app, "bitbucketOwner"),
				text(app, "bitbucketRepository"),
			) else {
				b.skipped(&shown, "a Bitbucket application without a repository");
				return;
			};
			git(
				b,
				format!("https://bitbucket.org/{owner}/{repo}"),
				text(app, "bitbucketBranch"),
				&mut doc,
			);
			b.check(
				&shown,
				"taken to be on bitbucket.org: connect a git provider if it is private",
			);
		}
		"gitea" => {
			b.skipped(
				&shown,
				"a Gitea application: Dokploy does not say which Gitea host it is on, add it by hand",
			);
			return;
		}
		"git" => {
			let Some(url) = text(app, "customGitUrl") else {
				b.skipped(&shown, "a git application without an address");
				return;
			};
			git(b, url.to_owned(), text(app, "customGitBranch"), &mut doc);
			if text(app, "customGitSSHKeyId").is_some() {
				b.check(
					&shown,
					"clones with an SSH key: add the key to podkit's SSH keys and name it in deploy_key",
				);
			}
		}
		other => {
			b.skipped(
				&shown,
				format!("its source is {other} (an uploaded archive), which podkit cannot clone"),
			);
			return;
		}
	}
	if source != "docker" {
		if doc.repo.is_none() {
			b.skipped(
				&shown,
				"a git application whose repository podkit cannot clone",
			);
			return;
		}
		let build_path = match source {
			"git" => text(app, "customGitBuildPath"),
			"gitlab" => text(app, "gitlabBuildPath"),
			"gitea" => text(app, "giteaBuildPath"),
			"bitbucket" => text(app, "bitbucketBuildPath"),
			_ => text(app, "buildPath"),
		};
		if build_path.is_some_and(|p| p != "/") {
			b.unmapped(
				&shown,
				"a build path inside the repository is not supported",
			);
		}
		match text(app, "buildType").unwrap_or("nixpacks") {
			"dockerfile" => {
				doc.build = Some("dockerfile".to_owned());
				doc.dockerfile = text(app, "dockerfile")
					.map(|d| {
						d.trim_start_matches("./")
							.trim_start_matches('/')
							.to_owned()
					})
					.filter(|d| d != "Dockerfile");
				if text(app, "dockerContextPath").is_some_and(|c| c != "/" && c != ".") {
					b.unmapped(
						&shown,
						"a Docker build context other than the repository root is not supported",
					);
				}
			}
			"nixpacks" => doc.build = Some("nixpacks".to_owned()),
			other => {
				b.check(
					&shown,
					format!(
						"built with {other} on Dokploy; podkit builds it with nixpacks, check the build"
					),
				);
				doc.build = Some("nixpacks".to_owned());
			}
		}
	}
	unmapped_settings(b, &shown, app);
	let vars = env_of(b, &shown, text(app, "env"));
	b.app(
		at,
		AppSpec {
			name,
			app: doc,
			vars,
			credentials,
		},
	);
}

fn compose(b: &mut Builder, snap: &Snapshot, at: &Place, id: &str) {
	let Some(row) = snap.get(&one("compose", "composeId", id)) else {
		b.skipped(id, "the compose application could not be read");
		return;
	};
	let name = text(row, "name").unwrap_or("compose").to_owned();
	let shown = at.path(&name);
	let Some(file) = text(row, "composeFile") else {
		b.skipped(
			&shown,
			"a compose application from a repository: Dokploy stores no compose file for it, add it by hand",
		);
		return;
	};
	if text(row, "composeType") == Some("stack") {
		b.check(
			&shown,
			"deployed as a Docker Swarm stack on Dokploy; podkit runs it as a plain compose stack",
		);
	}
	if flag(row, "randomize") || flag(row, "isolatedDeployment") {
		b.unmapped(
			&shown,
			"Dokploy's randomized names and isolated deployments are not supported",
		);
	}
	if let Some(src) = text(row, "sourceType").filter(|s| *s != "raw") {
		b.check(
			&shown,
			format!(
				"its compose file comes from a {src} repository; the copy Dokploy stored is imported"
			),
		);
	}
	let _routes = routes(b, &shown, row);
	let mut domains = std::collections::BTreeMap::new();
	for d in array(row, "domains") {
		if let (Some(service), Some(host)) = (text(d, "serviceName"), text(d, "host")) {
			if d.get("enabled") == Some(&Value::Bool(false)) {
				continue;
			}
			domains.entry(service.to_owned()).or_insert(StackDomainDoc {
				host: host.to_owned(),
				port: d
					.get("port")
					.and_then(port_of)
					.and_then(|p| u16::try_from(p).ok()),
			});
		}
	}
	let volumes = mounts(b, &shown, row);
	if !volumes.is_empty() {
		b.check(
			&shown,
			"its named volumes are in the compose file; the mounts listed in Dokploy are not copied",
		);
	}
	let vars = env_of(b, &shown, text(row, "env"));
	b.stack(
		at,
		StackSpec {
			name,
			stack: StackDoc {
				server: String::new(),
				compose: file.to_owned(),
				source: Some("raw".to_owned()),
				registry: None,
				env: std::collections::BTreeMap::new(),
				files: std::collections::BTreeMap::new(),
				domains,
			},
			vars,
		},
	);
}

fn database(b: &mut Builder, snap: &Snapshot, at: &Place, router: &str, id_name: &str, id: &str) {
	let Some(row) = snap.get(&one(router, id_name, id)) else {
		b.skipped(id, "the database could not be read");
		return;
	};
	let name = text(row, "name").unwrap_or("database").to_owned();
	let shown = at.path(&name);
	let image = text(row, "dockerImage").unwrap_or(router);
	let public = row.get("externalPort").and_then(port_of);
	let Some(mut doc) = managed_database(b, &shown, image, public) else {
		b.skipped(
			&shown,
			format!(
				"{router} databases are not supported; podkit manages postgres, mysql, mongodb and redis"
			),
		);
		return;
	};
	doc.memory_mb = text(row, "memoryLimit").and_then(memory);
	doc.cpu = text(row, "cpuLimit").and_then(cpus);
	if text(row, "env").is_some() || text(row, "command").is_some() {
		b.unmapped(
			&shown,
			"custom environment and start command of a database are not supported",
		);
	}
	b.check(
		&shown,
		"podkit generates the credentials, and the data is not copied: dump it on Dokploy and restore it here",
	);
	b.database(at, &name, doc);
}

/// Maps Dokploy's responses to a document.
#[must_use]
pub fn map(snap: &Snapshot, options: &Options) -> Imported {
	let mut b = Builder::new(Source::Dokploy, options);
	for project in snap.list("/api/project.all") {
		let Some(name) = text(project, "name") else {
			continue;
		};
		let slug = b.project(name);
		for env in array(project, "environments") {
			let env_name = text(env, "name").unwrap_or("production");
			let at = b.environment(&slug, env_name);
			for app in array(env, "applications") {
				if let Some(id) = text(app, "applicationId") {
					application(&mut b, snap, &at, id);
				}
			}
			for c in array(env, "compose") {
				if let Some(id) = text(c, "composeId") {
					compose(&mut b, snap, &at, id);
				}
			}
			for (list, router, id_name) in DATABASES {
				for db in array(env, list) {
					if let Some(id) = text(db, id_name) {
						database(&mut b, snap, &at, router, id_name, id);
					}
				}
			}
			if !array(env, "libsql").is_empty() {
				b.skipped(&at.path("libsql"), "libSQL databases are not supported");
			}
		}
	}
	b.finish()
}
