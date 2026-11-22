//! CapRover to a `podkit.toml`.
//!
//! CapRover's API has no published reference of its responses; the source is the reference:
//! <https://github.com/caprover/caprover>. What is used, with the file it comes from:
//!
//! - `POST /api/v2/login` with `{"password": …}` and the header `x-namespace: captain` answers
//!   `{"status": 100, "description": …, "data": {"token": …}}`; later calls send the token as
//!   `x-captain-auth` (`src/routes/login/LoginRouter.ts`, `src/utils/CaptainConstants.ts`,
//!   `src/app.ts` for the `/api/v2` prefix).
//! - `GET /api/v2/user/apps/appDefinitions` answers
//!   `{"status": 100, "data": {"appDefinitions": [...], "rootDomain": …}}`, each definition as in
//!   `src/models/AppDefinition.ts` (`IAppDef`: `appName`, `projectId`, `envVars`, `volumes`,
//!   `ports`, `customDomain`, `containerHttpPort`, `instanceCount`, `versions`,
//!   `deployedVersion`, `appPushWebhook.repoInfo`, …;
//!   `src/handlers/users/apps/appdefinition/AppDefinitionHandler.ts`).
//! - `GET /api/v2/user/projects` answers `{"data": {"projects": [{id, name, description,
//!   parentProjectId}]}}` (`src/routes/user/ProjectsRouter.ts`,
//!   `src/models/ProjectDefinition.ts`).
//!
//! CapRover stores no image for an app built from uploaded source or a captain-definition: the
//! image it builds lives in its own registry. An app is therefore imported when it has a git
//! repository (`appPushWebhook.repoInfo`) or was deployed from an external image, and reported
//! as skipped otherwise.

use serde_json::Value;

use super::http::Client;
use super::{
	AppSpec, Builder, Imported, Options, Place, Snapshot, Source, array, flag, port_of, repo_url,
	text,
};
use crate::error::ServerError;
use crate::iac::doc::AppDoc;

const APPS: &str = "/api/v2/user/apps/appDefinitions";
const PROJECTS: &str = "/api/v2/user/projects";

/// Reads everything the mapping needs.
///
/// # Errors
/// `Import` when CapRover cannot be reached or refuses the password.
pub async fn fetch(client: &Client) -> Result<Snapshot, ServerError> {
	let mut snap = Snapshot::default();
	for path in [APPS, PROJECTS] {
		let answer = client.get(path).await?;
		if answer["status"].as_i64() != Some(100) {
			let why = answer["description"]
				.as_str()
				.unwrap_or("it refused the request");
			return Err(ServerError::Import(format!("CapRover: {why}")));
		}
		snap.0.insert(path.to_owned(), answer);
	}
	Ok(snap)
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

/// The image the deployed version runs, when it is not one CapRover built itself.
fn external_image(app: &Value) -> Option<String> {
	let name = text(app, "appName")?;
	let deployed = app.get("deployedVersion").and_then(Value::as_i64)?;
	let image = array(app, "versions")
		.iter()
		.find(|v| v.get("version").and_then(Value::as_i64) == Some(deployed))
		.and_then(|v| text(v, "deployedImageName"))?;
	// `img-captain-<app>` is what ImageMaker names an image it builds (`src/user/ImageMaker.ts`).
	(!image.contains(&format!("img-captain-{name}"))).then(|| image.to_owned())
}

#[allow(clippy::too_many_lines, reason = "one block per setting of an app")]
fn application(b: &mut Builder, at: &Place, app: &Value) {
	let Some(name) = text(app, "appName") else {
		return;
	};
	let shown = at.path(name);
	let mut doc = new_app(app.get("containerHttpPort").and_then(port_of).unwrap_or(80));
	let mut credentials = Vec::new();

	let repo = app
		.get("appPushWebhook")
		.and_then(|w| w.get("repoInfo"))
		.filter(|r| text(r, "repo").is_some());
	if let Some(info) = repo {
		let url = text(info, "repo").unwrap_or_default();
		let Some(url) = repo_url(b, &shown, url) else {
			b.skipped(&shown, "its repository is not an address podkit can clone");
			return;
		};
		let https = url.starts_with("https://");
		doc.repo = Some(url);
		doc.git_ref = text(info, "branch").map(str::to_owned);
		doc.build = Some("dockerfile".to_owned());
		b.check(
			&shown,
			"CapRover builds from the captain-definition file; podkit builds the Dockerfile at the repository root (set dockerfile if it is elsewhere)",
		);
		if let Some(password) = text(info, "password") {
			if https {
				credentials.push(("git_token", password.to_owned()));
			} else {
				b.check(
					&shown,
					"has a password for an ssh repository, which podkit does not use",
				);
			}
		}
		if text(info, "sshKey").is_some() {
			b.check(
				&shown,
				"clones with an SSH key: add the key to podkit's SSH keys and name it in deploy_key",
			);
		}
	} else if let Some(image) = external_image(app) {
		doc.image = Some(image);
	} else {
		b.skipped(
			&shown,
			"it was deployed from uploaded source or the CLI, and the image CapRover built is only in its own registry: deploy it from git or an image first",
		);
		return;
	}

	for d in array(app, "customDomain") {
		if let Some(host) = text(d, "publicDomain") {
			doc.domains.push(host.to_owned());
		}
	}
	if flag(app, "notExposeAsWebApp") {
		b.check(
			&shown,
			"was not exposed as a web app; podkit gives every application its own subdomain",
		);
	}
	for v in array(app, "volumes") {
		let Some(path) = text(v, "containerPath") else {
			continue;
		};
		if text(v, "volumeName").is_some() {
			doc.volumes.push(path.to_owned());
		} else {
			b.unmapped(
				&shown,
				format!("bind mount {path} is not supported; only named volumes are"),
			);
		}
	}
	let vars = array(app, "envVars")
		.iter()
		.filter_map(|e| {
			Some((
				text(e, "key")?.to_owned(),
				e.get("value")
					.and_then(Value::as_str)
					.unwrap_or_default()
					.to_owned(),
			))
		})
		.collect();

	let ports = array(app, "ports").len();
	if ports > 0 {
		b.unmapped(
			&shown,
			format!("{ports} published port(s) are not supported"),
		);
	}
	if app
		.get("instanceCount")
		.and_then(Value::as_i64)
		.is_some_and(|n| n > 1)
	{
		b.unmapped(&shown, "replicas are not part of the file");
	}
	for (key, label) in [
		("customNginxConfig", "a custom nginx configuration"),
		("preDeployFunction", "a pre-deploy function"),
		("serviceUpdateOverride", "a Docker service update override"),
		("redirectDomain", "a redirect domain"),
	] {
		if text(app, key).is_some() {
			b.unmapped(&shown, format!("{label} is not supported"));
		}
	}
	if app.get("httpAuth").is_some_and(|v| !v.is_null()) {
		b.unmapped(&shown, "HTTP basic auth is not supported");
	}
	if array(app, "networks")
		.iter()
		.any(|n| n.as_str().is_some_and(|n| n != "captain-overlay-network"))
	{
		b.unmapped(
			&shown,
			"attached networks are not supported here; add them under Networks",
		);
	}
	if app
		.get("appDeployTokenConfig")
		.is_some_and(|c| flag(c, "enabled"))
	{
		b.unmapped(
			&shown,
			"the app deploy token is not supported; use a podkit deploy webhook",
		);
	}
	b.app(
		at,
		AppSpec {
			name: name.to_owned(),
			app: doc,
			vars,
			credentials,
		},
	);
}

/// Maps CapRover's responses to a document.
#[must_use]
pub fn map(snap: &Snapshot, options: &Options) -> Imported {
	let mut b = Builder::new(Source::Caprover, options);
	let projects: Vec<(String, String)> = snap
		.get(PROJECTS)
		.map_or(&[][..], |v| array(&v["data"], "projects"))
		.iter()
		.filter_map(|p| Some((text(p, "id")?.to_owned(), text(p, "name")?.to_owned())))
		.collect();
	let apps = snap
		.get(APPS)
		.map_or(&[][..], |v| array(&v["data"], "appDefinitions"));

	// A project of CapRover is a project here, with one environment; apps without a project
	// share "CapRover".
	let mut places: Vec<(Option<String>, Place)> = Vec::new();
	for app in apps {
		let id = text(app, "projectId").map(str::to_owned);
		if places.iter().any(|(i, _)| *i == id) {
			continue;
		}
		let name = id
			.as_deref()
			.and_then(|id| {
				projects
					.iter()
					.find(|(i, _)| i == id)
					.map(|(_, n)| n.as_str())
			})
			.unwrap_or("CapRover");
		let slug = b.project(name);
		let place = b.environment(&slug, "production");
		places.push((id, place));
	}
	for app in apps {
		let id = text(app, "projectId").map(str::to_owned);
		if let Some((_, place)) = places.iter().find(|(i, _)| *i == id) {
			let place = place.clone();
			application(&mut b, &place, app);
		}
	}
	b.finish()
}
