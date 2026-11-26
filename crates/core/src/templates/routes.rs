//! `/api/v1/templates` and the deploy route.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::expr;
use super::registry::{Loaded, search};
use super::service::{self, DeployInput, Deployed};
use super::{Template, Variable};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, ModelId, ProjectId, RegistryId, ServerId, TeamId};
use crate::state::AppState;

/// The gallery: `GET /`, `GET /{id}`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list))
		.route("/refresh", post(refresh))
		.route("/{template_id}", get(detail))
}

/// `POST /refresh`: fetch the catalog again. The catalog is the whole instance's, so only
/// its administrator may.
async fn refresh(
	State(s): State<AppState>,
	user: AuthUser,
) -> AppResult<Json<super::catalog::Refreshed>> {
	if !crate::quotas::service::is_instance_admin(&s.db, user.id).await? {
		return Err(ServerError::Forbidden);
	}
	Ok(Json(super::catalog::refresh(&s).await?))
}

/// `POST /{id}/deploy`, nested under a project.
pub fn deploy_router() -> Router<AppState> {
	Router::new()
		.route("/{template_id}/deploy", post(deploy))
		.route("/import", post(import))
		.route("/import/preview", post(import_preview))
}

#[derive(Serialize)]
struct Summary {
	id: String,
	name: String,
	description: String,
	category: String,
	tags: Vec<String>,
	version: String,
	logo_url: String,
	website: Option<String>,
	/// `application` or `stack`.
	kind: &'static str,
	services: usize,
	ram_mb: Option<u32>,
	gateway: bool,
	/// `ok`, `broken` or `unmaintained`, with the catalog's reason for the last two.
	status: super::Status,
	status_note: Option<String>,
	maintainers: Vec<String>,
}

fn kind(l: &Loaded) -> &'static str {
	if l.plain_app { "application" } else { "stack" }
}

fn summary(l: &Loaded) -> Summary {
	let t = &l.template;
	Summary {
		id: t.id.clone(),
		name: t.name.clone(),
		description: t.description.clone(),
		category: t.category.clone(),
		tags: t.tags.clone(),
		version: t.version.clone(),
		logo_url: crate::logos::template_url(l),
		website: t.website.clone(),
		kind: kind(l),
		services: l.services.len(),
		ram_mb: t.requires.ram_mb,
		gateway: t.gateway,
		status: t.status,
		status_note: t.status_note.clone(),
		maintainers: t.maintainers.clone(),
	}
}

#[derive(Deserialize)]
struct ListQuery {
	q: Option<String>,
	category: Option<String>,
}

#[derive(Serialize)]
struct ListResponse {
	templates: Vec<Summary>,
	categories: Vec<CategoryCount>,
}

#[derive(Serialize)]
struct CategoryCount {
	category: &'static str,
	count: usize,
}

/// Lists templates; `?q=` matches name, id, description and tags,
/// `?category=` narrows to one category. `categories` counts the matches of
/// `q` alone, so a gallery can show every category with its size.
async fn list(_user: AuthUser, Query(query): Query<ListQuery>) -> AppResult<Json<ListResponse>> {
	let q = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
	let category = query.category.as_deref().filter(|c| !c.is_empty());
	if let Some(c) = category
		&& !super::CATEGORIES.contains(&c)
	{
		return Err(ServerError::Validation(format!("unknown category {c:?}")));
	}
	let categories = super::CATEGORIES
		.iter()
		.map(|c| CategoryCount {
			category: c,
			count: search(q, Some(c)).len(),
		})
		.filter(|c| c.count > 0)
		.collect();
	Ok(Json(ListResponse {
		templates: search(q, category).iter().map(|l| summary(l)).collect(),
		categories,
	}))
}

#[derive(Serialize)]
pub struct VariableInfo {
	pub name: String,
	/// `input` the user may answer, `domain` a hostname the user may replace,
	/// `generated` made at deploy time and never shown.
	pub kind: &'static str,
	pub label: Option<String>,
	pub description: Option<String>,
	/// Shown for inputs that are not secret.
	pub default: Option<String>,
	pub required: bool,
	pub secret: bool,
	/// Generated, and its value is shown once in the deploy response.
	pub revealed: bool,
}

#[derive(Serialize)]
struct ServiceInfo {
	name: String,
	image: String,
	domain_port: Option<u16>,
	volumes: bool,
	depends_on: Vec<String>,
}

#[derive(Serialize)]
struct Detail {
	#[serde(flatten)]
	summary: Summary,
	docs: Option<String>,
	source: Option<String>,
	upstream: Option<String>,
	variables: Vec<VariableInfo>,
	services: Vec<ServiceInfo>,
	/// What the translation ignores or approximates.
	warnings: Vec<String>,
}

fn is_secret_expr(text: &str, template: &Template) -> bool {
	expr::scan(text).unwrap_or_default().iter().any(|e| match e {
		expr::Expr::Helper(h) => h.is_secret(),
		expr::Expr::Name(n) => matches!(template.variables.get(n), Some(Variable::Expr(t)) if t != text && is_secret_expr(t, template)),
	})
}

pub fn variables(t: &Template) -> Vec<VariableInfo> {
	t.variables
		.iter()
		.map(|(name, v)| match v {
			Variable::Input(input) => VariableInfo {
				name: name.clone(),
				kind: "input",
				label: input.label.clone(),
				description: input.description.clone(),
				default: input.default.clone().filter(|_| !input.secret),
				required: input.default.is_none(),
				secret: input.secret,
				revealed: false,
			},
			Variable::Expr(text) if text.trim() == "${domain}" => VariableInfo {
				name: name.clone(),
				kind: "domain",
				label: None,
				description: Some("Hostname; a free one is made when left empty".to_string()),
				default: None,
				required: false,
				secret: false,
				revealed: false,
			},
			Variable::Expr(text) => VariableInfo {
				name: name.clone(),
				kind: "generated",
				label: None,
				description: None,
				default: None,
				required: false,
				secret: is_secret_expr(text, t),
				revealed: t.reveal.contains(name),
			},
		})
		.collect()
}

/// One template with the variables it takes and the services it runs. Never
/// a generated value: those only exist once a deploy makes them.
async fn detail(_user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Detail>> {
	let l = super::find(&id).ok_or(ServerError::NotFound)?;
	Ok(Json(detail_of(&l)))
}

fn detail_of(l: &Loaded) -> Detail {
	let t = &l.template;
	Detail {
		summary: summary(l),
		docs: t.docs.clone(),
		source: t.source.clone(),
		upstream: t.upstream.clone(),
		variables: variables(t),
		services: l
			.services
			.iter()
			.map(|s| ServiceInfo {
				name: s.name.clone(),
				image: s.image.clone(),
				domain_port: s.domain_port,
				volumes: s.volumes,
				depends_on: s.depends_on.clone(),
			})
			.collect(),
		warnings: l.warnings.clone(),
	}
}

/// The two files of a template somebody hands in.
#[derive(Deserialize)]
struct ImportFiles {
	/// `template.toml`.
	template: String,
	/// `docker-compose.yml`.
	compose: String,
}

#[derive(Serialize)]
struct ImportPreview {
	/// Always `false`: an imported template is not the catalog's.
	official: bool,
	/// What the result will carry in place of `template:<id>@<version>`. Not `source`:
	/// that is the template's repository.
	origin: String,
	#[serde(flatten)]
	detail: Detail,
	/// The compose file as it will be run, to be read before it is.
	compose: String,
}

/// What an imported template is and would run, deploying nothing. For the page or the
/// command that shows it to the person before they confirm.
async fn import_preview(
	State(s): State<AppState>,
	_user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(files): Json<ImportFiles>,
) -> AppResult<Json<ImportPreview>> {
	crate::scope::project(&s.db, team, project).await?;
	let loaded = service::imported(&files.template, &files.compose)?;
	Ok(Json(ImportPreview {
		official: false,
		origin: service::import_source(&loaded, &files.template),
		detail: detail_of(&loaded),
		compose: files.compose,
	}))
}

#[derive(Deserialize)]
struct ImportRequest {
	#[serde(flatten)]
	files: ImportFiles,
	#[serde(flatten)]
	deploy: DeployRequest,
}

/// Deploys a template handed in as files; 202 with what was created. The result is marked
/// `import:` and never taken for a catalog template.
async fn import(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<ImportRequest>,
) -> AppResult<(StatusCode, Json<Deployed>)> {
	if body.deploy.model_id.is_some() {
		return Err(ServerError::Validation(
			"an imported template cannot be linked to a model".to_string(),
		));
	}
	let deployed = service::deploy_import(
		&s,
		user.id,
		team,
		project,
		body.deploy.environment_id,
		body.deploy.server_id,
		&body.files.template,
		&body.files.compose,
		DeployInput {
			name: body.deploy.name,
			inputs: body.deploy.inputs,
			model: None,
			registry: body.deploy.registry_id,
		},
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(deployed)))
}

#[derive(Deserialize)]
struct DeployRequest {
	server_id: ServerId,
	#[serde(default)]
	environment_id: Option<EnvironmentId>,
	#[serde(default)]
	name: Option<String>,
	/// Answers to `input` variables and replacement hostnames for `domain`
	/// variables.
	#[serde(default)]
	inputs: BTreeMap<String, String>,
	/// A model to link, for templates with `gateway`.
	#[serde(default)]
	model_id: Option<ModelId>,
	/// A saved registry to pull the images with.
	#[serde(default)]
	registry_id: Option<RegistryId>,
}

/// Deploys a template into the project; 202 with what was created. Caller
/// must hold `create` on the application or stack it becomes.
async fn deploy(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, String)>,
	Json(body): Json<DeployRequest>,
) -> AppResult<(StatusCode, Json<Deployed>)> {
	let deployed = service::deploy(
		&s,
		user.id,
		team,
		project,
		body.environment_id,
		body.server_id,
		&id,
		DeployInput {
			name: body.name,
			inputs: body.inputs,
			model: body.model_id,
			registry: body.registry_id,
		},
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(deployed)))
}
