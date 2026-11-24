//! Coolify v4 to a `podkit.toml`.
//!
//! The API is documented by Coolify's own OpenAPI file,
//! <https://github.com/coollabsio/coolify/blob/v4.x/openapi.json> (served at
//! `/docs/api-reference/` of a Coolify instance), and its controllers under
//! `app/Http/Controllers/Api/` in the same repository. Authentication is a bearer token
//! (`Authorization: Bearer <token>`); the API is under `/api/v1`. What is read, and where the
//! response shapes come from:
//!
//! | Request | Response | Source |
//! |---|---|---|
//! | `GET /projects` | array of `{uuid, name, description}` | `ProjectController::projects` |
//! | `GET /projects/{uuid}` | the project with `environments: [{id, name, …}]` | `ProjectController::project_by_uuid` loads `environments` |
//! | `GET /applications` | array of `Application` (`environment_id`, `build_pack`, `fqdn`, `git_*`, `docker_registry_image_*`, `ports_exposes`, `limits_*`, `docker_compose_*`, `settings`, …) | `ApplicationsController::applications`, schema `Application` |
//! | `GET /applications/{uuid}/envs` | array of `{key, value, real_value, is_preview, is_buildtime, is_runtime, is_shown_once, …}` | `ApplicationsController::envs`; values need a token with `read:sensitive` |
//! | `GET /applications/{uuid}/storages` | `{persistent_storages: [{name, mount_path, host_path}], file_storages: […]}` | `ApplicationsController::storages`, model `LocalPersistentVolume` |
//! | `GET /applications/{uuid}/scheduled-tasks` | array of `{name, command, frequency, container, timeout, enabled}` | schema `ScheduledTask`; `frequency` is a cron expression or one of `VALID_CRON_STRINGS` (`bootstrap/helpers/constants.php`) |
//! | `GET /databases` | array of `Database` (`database_type` such as `standalone-postgresql`, `image`, `environment_id`, `is_public`, `public_port`, `limits_*`) | `DatabasesController::databases`, schema `Database` |
//! | `GET /services` | array of `Service` (`environment_id`, `docker_compose_raw`, `name`) | schema `Service` |
//! | `GET /services/{uuid}/envs` | as application variables | `ServicesController::envs` |

use serde_json::Value;

use super::http::Client;
use super::{
	AppSpec, Builder, Imported, Options, Place, Snapshot, Source, StackSpec, array, flag,
	managed_database, megabytes, port_of, repo_url, text,
};
use crate::error::ServerError;
use crate::iac::doc::{AppDoc, CronDoc, StackDoc, StackDomainDoc};

const API: &str = "/api/v1";

/// Reads everything the mapping needs.
///
/// # Errors
/// `Import` when Coolify cannot be reached or refuses the token.
pub async fn fetch(client: &Client) -> Result<Snapshot, ServerError> {
	let mut snap = Snapshot::default();
	let projects = client.get(&format!("{API}/projects")).await?;
	for project in projects.as_array().map_or(&[][..], Vec::as_slice) {
		if let Some(uuid) = text(project, "uuid") {
			let path = format!("{API}/projects/{uuid}");
			let one = client.get(&path).await?;
			snap.0.insert(path, one);
		}
	}
	snap.0.insert(format!("{API}/projects"), projects);

	let apps = client.get(&format!("{API}/applications")).await?;
	for app in apps.as_array().map_or(&[][..], Vec::as_slice) {
		let Some(uuid) = text(app, "uuid") else {
			continue;
		};
		for rest in ["envs", "storages", "scheduled-tasks"] {
			let path = format!("{API}/applications/{uuid}/{rest}");
			if let Some(v) = client.get_opt(&path).await? {
				snap.0.insert(path, v);
			}
		}
	}
	snap.0.insert(format!("{API}/applications"), apps);

	let databases = client.get(&format!("{API}/databases")).await?;
	snap.0.insert(format!("{API}/databases"), databases);

	let services = client.get(&format!("{API}/services")).await?;
	for service in services.as_array().map_or(&[][..], Vec::as_slice) {
		if let Some(uuid) = text(service, "uuid") {
			let path = format!("{API}/services/{uuid}/envs");
			if let Some(v) = client.get_opt(&path).await? {
				snap.0.insert(path, v);
			}
		}
	}
	snap.0.insert(format!("{API}/services"), services);
	Ok(snap)
}

/// The cron expression of a Coolify task frequency.
fn schedule(frequency: &str) -> Option<String> {
	let f = frequency.trim();
	// `VALID_CRON_STRINGS` of bootstrap/helpers/constants.php.
	let named = match f {
		"every_minute" => "* * * * *",
		"hourly" | "@hourly" => "0 * * * *",
		"daily" | "@daily" => "0 0 * * *",
		"weekly" | "@weekly" => "0 0 * * 0",
		"monthly" | "@monthly" => "0 0 1 * *",
		"yearly" | "@yearly" => "0 0 1 1 *",
		other => other,
	};
	(named.split_whitespace().count() == 5).then(|| named.to_owned())
}

/// A URL list (`https://a.com,https://b.com:3000/path`) as hostnames, and the first explicit
/// port.
fn hosts(list: &str) -> (Vec<String>, Option<i32>, bool) {
	let mut out = Vec::new();
	let mut port = None;
	let mut path = false;
	for raw in list.split(',') {
		let raw = raw.trim();
		if raw.is_empty() {
			continue;
		}
		let with_scheme = if raw.contains("://") {
			raw.to_owned()
		} else {
			format!("https://{raw}")
		};
		let Ok(url) = url::Url::parse(&with_scheme) else {
			continue;
		};
		if let Some(host) = url.host_str() {
			out.push(host.to_ascii_lowercase());
		}
		port = port.or_else(|| url.port().map(i32::from));
		path |= url.path() != "/" && !url.path().is_empty();
	}
	(out, port, path)
}

/// The variables of an `envs` response. Values need `read:sensitive`; without it the field is
/// missing, and the count is returned so the report can say so once.
#[allow(
	clippy::struct_field_names,
	reason = "`vars` is the list; `hidden` and `preview` count the rest"
)]
struct Vars {
	vars: Vec<(String, String)>,
	hidden: usize,
	preview: usize,
	build_only: usize,
}

fn vars(envs: &[Value]) -> Vars {
	let mut out = Vars {
		vars: Vec::new(),
		hidden: 0,
		preview: 0,
		build_only: 0,
	};
	for env in envs {
		let Some(key) = text(env, "key") else {
			continue;
		};
		if flag(env, "is_preview") {
			out.preview += 1;
			continue;
		}
		if flag(env, "is_shown_once") {
			out.hidden += 1;
			continue;
		}
		// `real_value` is the value with shared variables resolved; `value` the stored one.
		let value = env
			.get("real_value")
			.and_then(Value::as_str)
			.or_else(|| env.get("value").and_then(Value::as_str));
		let Some(value) = value else {
			out.hidden += 1;
			continue;
		};
		if flag(env, "is_buildtime") && !flag(env, "is_runtime") {
			out.build_only += 1;
		}
		out.vars.push((key.to_owned(), value.to_owned()));
	}
	out
}

fn report_vars(b: &mut Builder, at: &str, v: &Vars) {
	if v.hidden > 0 {
		b.skipped(
			at,
			format!(
				"{} variable(s) have no readable value (locked, or the token lacks read:sensitive) and were left out",
				v.hidden
			),
		);
	}
	if v.preview > 0 {
		b.unmapped(
			at,
			format!(
				"{} preview-deployment variable(s): preview deployments are not supported",
				v.preview
			),
		);
	}
	if v.build_only > 0 {
		b.check(
			at,
			format!(
				"{} build-time variable(s) are set at runtime too: podkit has one set of variables",
				v.build_only
			),
		);
	}
}

/// Settings of an application that podkit has no field for. Reported when they are set.
fn unmapped_settings(b: &mut Builder, at: &str, app: &Value) {
	let labels = [
		("custom_docker_run_options", "custom docker run options"),
		("pre_deployment_command", "pre-deployment command"),
		("post_deployment_command", "post-deployment command"),
		("install_command", "install command"),
		("build_command", "build command"),
		("start_command", "start command"),
		("publish_directory", "publish directory"),
		("ports_mappings", "host port mappings"),
		("custom_network_aliases", "custom network aliases"),
		("custom_nginx_configuration", "custom nginx configuration"),
		("dockerfile_target_build", "Dockerfile build target"),
		("watch_paths", "watch paths"),
	];
	for (key, label) in labels {
		if text(app, key).is_some() {
			b.unmapped(at, format!("{label} is not supported"));
		}
	}
	if let Some(base) = text(app, "base_directory")
		&& base != "/"
	{
		b.unmapped(at, "base directory is not supported");
	}
	for (key, label) in [
		("is_http_basic_auth_enabled", "HTTP basic auth"),
		("health_check_enabled", "custom health check"),
	] {
		if flag(app, key) {
			b.unmapped(at, format!("{label} is not supported"));
		}
	}
	let settings = app.get("settings").unwrap_or(&Value::Null);
	if flag(settings, "is_preview_deployments_enabled") {
		b.unmapped(at, "preview deployments are not supported");
	}
	if flag(settings, "is_auto_deploy_enabled") {
		b.unmapped(
			at,
			"auto deploy on push is not part of the file; set up a webhook",
		);
	}
	if settings.get("is_container_label_readonly_enabled") == Some(&Value::Bool(false))
		&& text(app, "custom_labels").is_some()
	{
		b.unmapped(at, "custom labels are not supported");
	}
	if app
		.get("swarm_replicas")
		.and_then(Value::as_i64)
		.is_some_and(|n| n > 1)
	{
		b.unmapped(at, "replicas are not part of the file");
	}
	if app.get("private_key_id").is_some_and(|v| !v.is_null()) {
		b.check(
			at,
			"clones with a deploy key: add the key to podkit's SSH keys and name it in deploy_key",
		);
	}
	if text(app, "git_https_username").is_some() {
		b.check(
			at,
			"clones over https with a username and token: the token is not readable through the API, add it as git_token",
		);
	}
}

/// An image reference from a name and a tag, with a digest taken as one.
fn image_ref(name: &str, tag: Option<&str>) -> String {
	match tag {
		Some(t) if t.starts_with("sha256:") => format!("{name}@{t}"),
		Some(t) if !name.contains('@') && !name.rsplit('/').next().unwrap_or("").contains(':') => {
			format!("{name}:{t}")
		}
		_ => name.to_owned(),
	}
}

fn place_of<'a>(places: &'a [(i64, Place)], env_id: &Value) -> Option<&'a Place> {
	let id = env_id.as_i64()?;
	places.iter().find(|(i, _)| *i == id).map(|(_, p)| p)
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

fn compose_domains(raw: &Value) -> Vec<(String, String)> {
	let parsed: Value = match raw {
		Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
		other => other.clone(),
	};
	let items: Vec<&Value> = match &parsed {
		Value::Array(a) => a.iter().collect(),
		Value::Object(o) => o.values().collect(),
		_ => Vec::new(),
	};
	items
		.into_iter()
		.filter_map(|i| Some((text(i, "name")?.to_owned(), text(i, "domain")?.to_owned())))
		.collect()
}

fn stack_domains(
	b: &mut Builder,
	at: &str,
	list: &[(String, String)],
) -> std::collections::BTreeMap<String, StackDomainDoc> {
	let mut out = std::collections::BTreeMap::new();
	for (service, domains) in list {
		let (found, port, _) = hosts(domains);
		if found.len() > 1 {
			b.unmapped(
				at,
				format!(
					"service {service} has {} domains; only the first is imported",
					found.len()
				),
			);
		}
		if let Some(host) = found.into_iter().next() {
			out.insert(
				service.clone(),
				StackDomainDoc {
					host,
					port: port.and_then(|p| u16::try_from(p).ok()),
				},
			);
		}
	}
	out
}

fn cron_of(b: &mut Builder, at: &Place, app_key: &str, shown: &str, tasks: &[Value]) {
	for task in tasks {
		let (Some(name), Some(command), Some(frequency)) = (
			text(task, "name"),
			text(task, "command"),
			text(task, "frequency"),
		) else {
			continue;
		};
		let Some(cron) = schedule(frequency) else {
			b.skipped(
				shown,
				format!(
					"scheduled task \"{name}\" runs on \"{frequency}\", which is not a five-field cron schedule"
				),
			);
			continue;
		};
		if text(task, "container").is_some() {
			b.unmapped(
				shown,
				format!(
					"scheduled task \"{name}\" names a container; it runs in the application's"
				),
			);
		}
		b.cron(
			at,
			&format!("{app_key}-{name}"),
			CronDoc {
				kind: "exec".to_owned(),
				app: Some(app_key.to_owned()),
				server: None,
				image: None,
				command: command.to_owned(),
				schedule: cron,
				timezone: None,
				timeout: task
					.get("timeout")
					.and_then(Value::as_i64)
					.and_then(|t| i32::try_from(t).ok())
					.filter(|t| *t > 0),
				concurrency: None,
				retries: None,
				retry_delay: None,
				enabled: (!flag(task, "enabled")).then_some(false),
			},
		);
	}
}

#[allow(clippy::too_many_lines, reason = "one block per setting of an app")]
fn application(b: &mut Builder, snap: &Snapshot, places: &[(i64, Place)], app: &Value) {
	let name = text(app, "name").unwrap_or("application").to_owned();
	let Some(at) = place_of(places, &app["environment_id"]).cloned() else {
		b.skipped(
			&name,
			"its environment is not in a project this token can read",
		);
		return;
	};
	let shown = at.path(&name);
	let uuid = text(app, "uuid").unwrap_or_default();
	let envs = snap.list(&format!("{API}/applications/{uuid}/envs"));
	let v = vars(envs);
	let build_pack = text(app, "build_pack").unwrap_or("nixpacks");

	if build_pack == "dockercompose" {
		let compose = text(app, "docker_compose_raw").or_else(|| text(app, "docker_compose"));
		let Some(compose) = compose else {
			b.skipped(
				&shown,
				"a Docker Compose application without a compose file the API returns",
			);
			return;
		};
		b.check(
			&shown,
			"imported as a stack with the compose file Coolify has stored; it is not rebuilt from the repository",
		);
		let list = compose_domains(app.get("docker_compose_domains").unwrap_or(&Value::Null));
		let domains = stack_domains(b, &shown, &list);
		unmapped_settings(b, &shown, app);
		report_vars(b, &shown, &v);
		b.stack(
			&at,
			StackSpec {
				name,
				stack: StackDoc {
					server: String::new(),
					compose: compose.to_owned(),
					source: Some("raw".to_owned()),
					registry: None,
					env: std::collections::BTreeMap::new(),
					files: std::collections::BTreeMap::new(),
					domains,
				},
				vars: v.vars,
			},
		);
		return;
	}

	let (list, fqdn_port, path) = hosts(text(app, "fqdn").unwrap_or_default());
	if path {
		b.unmapped(&shown, "a domain with a path is routed by hostname only");
	}
	let exposes = text(app, "ports_exposes")
		.and_then(|p| p.split(',').next())
		.and_then(|p| port_of(&Value::String(p.to_owned())));
	let port = exposes.or(fqdn_port).unwrap_or_else(|| {
		b.check(&shown, "no exposed port is set; 3000 is assumed");
		3000
	});
	let mut doc = new_app(port);
	doc.domains = list;
	doc.memory_mb = text(app, "limits_memory").and_then(megabytes);
	doc.cpu = text(app, "limits_cpus")
		.and_then(|c| c.parse::<f64>().ok())
		.filter(|c| *c > 0.0);

	if build_pack == "dockerimage" {
		let Some(image) = text(app, "docker_registry_image_name") else {
			b.skipped(&shown, "an image application without an image");
			return;
		};
		doc.image = Some(image_ref(image, text(app, "docker_registry_image_tag")));
	} else {
		let repo = text(app, "git_full_url")
			.filter(|u| u.contains("://"))
			.or_else(|| text(app, "git_repository"));
		let Some(url) = repo.and_then(|r| repo_url(b, &shown, r)) else {
			b.skipped(
				&shown,
				"a git application whose repository podkit cannot clone",
			);
			return;
		};
		doc.repo = Some(url);
		doc.git_ref = text(app, "git_branch").map(str::to_owned);
		match build_pack {
			"dockerfile" => {
				doc.build = Some("dockerfile".to_owned());
				doc.dockerfile = text(app, "dockerfile_location")
					.map(|d| d.trim_start_matches('/').to_owned())
					.filter(|d| !d.is_empty());
			}
			"nixpacks" => doc.build = Some("nixpacks".to_owned()),
			other => {
				b.check(
					&shown,
					format!(
						"built with {other} on Coolify; podkit builds it with nixpacks, check the build"
					),
				);
				doc.build = Some("nixpacks".to_owned());
			}
		}
	}

	if let Some(storages) = snap.get(&format!("{API}/applications/{uuid}/storages")) {
		for s in array(storages, "persistent_storages") {
			if let Some(path) = text(s, "mount_path") {
				if text(s, "host_path").is_some() {
					b.unmapped(
						&shown,
						format!("bind mount {path} is not supported; only named volumes are"),
					);
				} else {
					doc.volumes.push(path.to_owned());
				}
			}
		}
		if !array(storages, "file_storages").is_empty() {
			b.unmapped(&shown, "file mounts are not supported on applications");
		}
	}
	unmapped_settings(b, &shown, app);
	report_vars(b, &shown, &v);
	let key = b.app(
		&at,
		AppSpec {
			name,
			app: doc,
			vars: v.vars,
			credentials: Vec::new(),
		},
	);
	if let Some(key) = key {
		let tasks = snap.list(&format!("{API}/applications/{uuid}/scheduled-tasks"));
		cron_of(b, &at, &key, &shown, tasks);
	}
}

fn database(b: &mut Builder, places: &[(i64, Place)], db: &Value) {
	let name = text(db, "name").unwrap_or("database").to_owned();
	let Some(at) = place_of(places, &db["environment_id"]).cloned() else {
		b.skipped(
			&name,
			"its environment is not in a project this token can read",
		);
		return;
	};
	let shown = at.path(&name);
	let kind = text(db, "database_type").unwrap_or_default();
	let image = text(db, "image").unwrap_or_default();
	let engine_hint = kind.strip_prefix("standalone-").unwrap_or(kind);
	// The image names the engine and version; the type decides when the image is missing.
	let probe = if image.is_empty() { engine_hint } else { image };
	let public_port = flag(db, "is_public")
		.then(|| db.get("public_port").and_then(port_of))
		.flatten();
	let Some(doc) = managed_database(b, &shown, probe, public_port) else {
		b.skipped(
			&shown,
			format!(
				"{engine_hint} databases are not supported; podkit manages postgres, mysql, mongodb and redis"
			),
		);
		return;
	};
	if text(db, "custom_docker_run_options").is_some()
		|| text(db, "postgres_conf").is_some()
		|| text(db, "redis_conf").is_some()
		|| text(db, "mysql_conf").is_some()
		|| text(db, "mongo_conf").is_some()
	{
		b.unmapped(&shown, "custom database configuration is not supported");
	}
	if db
		.get("init_scripts")
		.and_then(Value::as_array)
		.is_some_and(|a| !a.is_empty())
	{
		b.unmapped(&shown, "init scripts are not supported");
	}
	if flag(db, "enable_ssl") {
		b.unmapped(&shown, "database SSL is not supported");
	}
	let mut doc = doc;
	doc.memory_mb = text(db, "limits_memory").and_then(megabytes);
	doc.cpu = text(db, "limits_cpus")
		.and_then(|c| c.parse::<f64>().ok())
		.filter(|c| *c > 0.0);
	b.check(
		&shown,
		"podkit generates the credentials, and the data is not copied: dump it on Coolify and restore it here",
	);
	b.database(&at, &name, doc);
}

fn service(b: &mut Builder, snap: &Snapshot, places: &[(i64, Place)], svc: &Value) {
	let name = text(svc, "name").unwrap_or("service").to_owned();
	let Some(at) = place_of(places, &svc["environment_id"]).cloned() else {
		b.skipped(
			&name,
			"its environment is not in a project this token can read",
		);
		return;
	};
	let shown = at.path(&name);
	let Some(compose) = text(svc, "docker_compose_raw") else {
		b.skipped(&shown, "a service without a compose file the API returns");
		return;
	};
	if compose.contains("SERVICE_FQDN_")
		|| compose.contains("SERVICE_URL_")
		|| compose.contains("SERVICE_PASSWORD_")
		|| compose.contains("SERVICE_USER_")
		|| compose.contains("SERVICE_BASE64_")
	{
		b.check(
			&shown,
			"the compose file uses Coolify's magic variables (SERVICE_FQDN_*, SERVICE_PASSWORD_* …); podkit does not make them, set them as variables",
		);
	}
	b.check(
		&shown,
		"domains of a service are not imported; add them to the stack",
	);
	let uuid = text(svc, "uuid").unwrap_or_default();
	let v = vars(snap.list(&format!("{API}/services/{uuid}/envs")));
	report_vars(b, &shown, &v);
	b.stack(
		&at,
		StackSpec {
			name,
			stack: StackDoc {
				server: String::new(),
				compose: compose.to_owned(),
				source: Some("raw".to_owned()),
				registry: None,
				env: std::collections::BTreeMap::new(),
				files: std::collections::BTreeMap::new(),
				domains: std::collections::BTreeMap::new(),
			},
			vars: v.vars,
		},
	);
}

/// Maps Coolify's responses to a document.
#[must_use]
pub fn map(snap: &Snapshot, options: &Options) -> Imported {
	let mut b = Builder::new(Source::Coolify, options);
	let mut places: Vec<(i64, Place)> = Vec::new();
	for project in snap.list(&format!("{API}/projects")) {
		let (Some(uuid), Some(name)) = (text(project, "uuid"), text(project, "name")) else {
			continue;
		};
		let slug = b.project(name);
		// The environments come from the project's own response; the list has none.
		let detail = snap
			.get(&format!("{API}/projects/{uuid}"))
			.unwrap_or(project);
		for env in array(detail, "environments") {
			if let (Some(id), Some(env_name)) =
				(env.get("id").and_then(Value::as_i64), text(env, "name"))
			{
				let place = b.environment(&slug, env_name);
				places.push((id, place));
			}
		}
	}
	for app in snap.list(&format!("{API}/applications")) {
		application(&mut b, snap, &places, app);
	}
	for db in snap.list(&format!("{API}/databases")) {
		database(&mut b, &places, db);
	}
	for svc in snap.list(&format!("{API}/services")) {
		service(&mut b, snap, &places, svc);
	}
	b.finish()
}
