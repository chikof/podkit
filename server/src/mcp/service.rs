//! The MCP tools, resources and prompts. Each tool is one or a few calls to podkit's JSON
//! API under the caller's own token, so scope, RBAC and audit are decided in one place.

use std::fmt::Write as _;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::auth::tokens::{Grant, Scope};
use crate::ids::TeamId;

/// Longest tool result, in characters. A model's context is the scarce thing, not ours.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Most log lines one `get_logs` call returns.
pub const MAX_TAIL: u32 = 500;
const DEFAULT_TAIL: u32 = 100;
/// Longest single log line kept.
const MAX_LINE: usize = 2000;
/// Most metric points one `get_metrics` call returns (the newest).
const MAX_POINTS: usize = 60;
/// Most bytes read back from one internal call.
const MAX_BODY: usize = 4 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MASK: &str = "•••";

/// The kinds of resource a project holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Application,
	Database,
	Model,
	Stack,
}

impl Kind {
	const ALL: [Self; 4] = [Self::Application, Self::Database, Self::Model, Self::Stack];

	fn parse(raw: &str) -> Option<Self> {
		Self::ALL
			.into_iter()
			.find(|k| k.name() == raw || k.segment() == raw)
	}

	const fn name(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
		}
	}

	/// The collection's path segment.
	const fn segment(self) -> &'static str {
		match self {
			Self::Application => "applications",
			Self::Database => "databases",
			Self::Model => "models",
			Self::Stack => "stacks",
		}
	}
}

/// One caller's view of the API: their token, their team.
pub struct Session {
	api: Router,
	authorization: String,
	grant: Grant,
}

type ToolResult = Result<String, String>;

impl Session {
	pub const fn new(api: Router, authorization: String, grant: Grant) -> Self {
		Self {
			api,
			authorization,
			grant,
		}
	}

	const fn team(&self) -> TeamId {
		self.grant.team
	}

	fn project_path(&self, project: i64) -> String {
		format!("/teams/{}/projects/{project}", self.team().0)
	}

	/// One request to the JSON API as the caller. Returns the status and body, the body
	/// cut at [`MAX_BODY`].
	async fn send(
		&self,
		method: Method,
		path: &str,
		body: Option<Value>,
	) -> Result<(StatusCode, Vec<u8>), String> {
		let mut request = Request::builder()
			.method(method)
			.uri(path)
			.header(header::AUTHORIZATION, &self.authorization)
			.header(header::USER_AGENT, "podkit-mcp");
		let body = match body {
			Some(b) => {
				request = request.header(header::CONTENT_TYPE, "application/json");
				Body::from(b.to_string())
			}
			None => Body::empty(),
		};
		let request = request.body(body).map_err(|e| e.to_string())?;
		let work = async {
			let response = self
				.api
				.clone()
				.oneshot(request)
				.await
				.map_err(|e| e.to_string())?;
			let status = response.status();
			let mut body = response.into_body();
			let mut bytes = Vec::new();
			while let Some(frame) = body.frame().await {
				let frame = frame.map_err(|e| e.to_string())?;
				if let Some(data) = frame.data_ref() {
					bytes.extend_from_slice(data);
					if bytes.len() >= MAX_BODY {
						break;
					}
				}
			}
			Ok::<_, String>((status, bytes))
		};
		tokio::time::timeout(CALL_TIMEOUT, work)
			.await
			.map_err(|_| "podkit took too long to answer".to_owned())?
	}

	/// A call whose success is JSON.
	async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, String> {
		let (status, bytes) = self.send(method, path, body).await?;
		let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
		if status.is_success() {
			Ok(value)
		} else {
			Err(refusal(status, &value))
		}
	}

	async fn get(&self, path: &str) -> Result<Value, String> {
		self.json(Method::GET, path, None).await
	}

	pub async fn call(&self, name: &str, args: &Value) -> ToolResult {
		let value = self.run(name, args).await?;
		Ok(limit(match value {
			Value::String(text) => scrub_text(&text),
			mut other => {
				scrub(&mut other);
				serde_json::to_string_pretty(&other).unwrap_or_default()
			}
		}))
	}

	async fn run(&self, name: &str, args: &Value) -> Result<Value, String> {
		match name {
			"list_projects" => {
				self.get(&format!("/teams/{}/projects", self.team().0))
					.await
			}
			"list_resources" => self.list_resources(int(args, "project_id")?).await,
			"get_resource" => {
				let (project, kind, id) = target(args)?;
				self.resource(project, kind, id).await
			}
			"get_logs" => self.logs(args).await,
			"get_metrics" => self.metrics(args).await,
			"get_scan" => {
				let (project, kind, id) = target(args)?;
				self.get(&format!(
					"/teams/{}/security/projects/{project}/{}/{id}",
					self.team().0,
					kind.segment()
				))
				.await
			}
			"list_templates" => self.templates(args).await,
			"deploy" => self.deploy(args).await,
			"restart" => self.verb(args, "restart").await,
			"stop" => self.verb(args, "stop").await,
			"set_env_var" => self.set_env_var(args).await,
			"create_from_template" => self.template_deploy(args).await,
			"run_cron_task" => self.run_cron_task(args).await,
			"delete_resource" => self.delete(args).await,
			other => Err(format!("unknown tool {other}")),
		}
	}

	async fn list_resources(&self, project: i64) -> Result<Value, String> {
		let base = self.project_path(project);
		let mut out = Map::new();
		for kind in Kind::ALL {
			let listed = self.get(&format!("{base}/{}", kind.segment())).await;
			// A project whose id is wrong or hidden is 404 for every kind; say so once.
			if let (Err(e), Kind::Application) = (&listed, kind) {
				return Err(e.clone());
			}
			out.insert(
				kind.segment().to_owned(),
				listed.unwrap_or_else(|e| json!({ "error": e })),
			);
		}
		Ok(Value::Object(out))
	}

	async fn resource(&self, project: i64, kind: Kind, id: i64) -> Result<Value, String> {
		let base = self.project_path(project);
		if kind != Kind::Application {
			return self.get(&format!("{base}/{}/{id}", kind.segment())).await;
		}
		// Applications have no single-item route; the list has them.
		let list = self.get(&format!("{base}/applications")).await?;
		let mut found = list
			.as_array()
			.and_then(|all| all.iter().find(|a| a["id"].as_i64() == Some(id)))
			.cloned()
			.ok_or_else(|| "404: application not found".to_owned())?;
		// What `diagnose_failing_deploy` needs first: how the latest deploys ended.
		if let Ok(recent) = self
			.get(&format!("{base}/applications/{id}/deployments?limit=3"))
			.await
		{
			found["recent_deployments"] = recent;
		}
		Ok(found)
	}

	async fn name_of(&self, project: i64, kind: Kind, id: i64) -> Result<String, String> {
		self.resource(project, kind, id).await?["name"]
			.as_str()
			.map(str::to_owned)
			.ok_or_else(|| "the resource has no name".to_owned())
	}

	async fn logs(&self, args: &Value) -> Result<Value, String> {
		let (project, kind, id) = target(args)?;
		let tail = tail_of(args);
		let base = self.project_path(project);
		let path = if kind == Kind::Stack {
			let service = word(args, "service")?
				.ok_or_else(|| "a stack's logs need a `service`".to_owned())?;
			format!("{base}/stacks/{id}/services/{service}/logs?tail={tail}&follow=false")
		} else {
			format!(
				"{base}/{}/{id}/logs?tail={tail}&follow=false",
				kind.segment()
			)
		};
		let (status, bytes) = self.send(Method::GET, &path, None).await?;
		if !status.is_success() {
			let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
			return Err(refusal(status, &value));
		}
		Ok(Value::String(log_text(
			&String::from_utf8_lossy(&bytes),
			tail,
		)))
	}

	async fn metrics(&self, args: &Value) -> Result<Value, String> {
		let (project, kind, id) = target(args)?;
		let range = match args.get("range").and_then(Value::as_str) {
			None => "1h",
			Some(r @ ("1h" | "24h" | "7d")) => r,
			Some(_) => return Err("range must be 1h, 24h or 7d".to_owned()),
		};
		let points = self
			.get(&format!(
				"{}/{}/{id}/metrics?range={range}",
				self.project_path(project),
				kind.segment()
			))
			.await?;
		Ok(match points {
			Value::Array(all) if all.len() > MAX_POINTS => {
				let total = all.len();
				json!({
					"range": range,
					"points_total": total,
					"points": all[total - MAX_POINTS..],
				})
			}
			Value::Array(all) => json!({ "range": range, "points": all }),
			other => other,
		})
	}

	async fn templates(&self, args: &Value) -> Result<Value, String> {
		let mut query = Vec::new();
		for key in ["q", "category"] {
			if let Some(v) = args.get(key).and_then(Value::as_str) {
				query.push(format!("{key}={}", encode(v)));
			}
		}
		let mut path = "/templates".to_owned();
		if !query.is_empty() {
			path = format!("{path}?{}", query.join("&"));
		}
		self.get(&path).await
	}

	async fn deploy(&self, args: &Value) -> Result<Value, String> {
		let (project, kind, id) = target(args)?;
		let base = self.project_path(project);
		match kind {
			Kind::Application => {
				self.json(
					Method::POST,
					&format!("{base}/applications/{id}/deployments"),
					None,
				)
				.await
			}
			Kind::Stack => {
				self.json(Method::POST, &format!("{base}/stacks/{id}/deploy"), None)
					.await
			}
			_ => Err("deploy works on an application or a stack".to_owned()),
		}
	}

	/// Starts a scheduled task now. The API queues the run and answers at once; the run
	/// itself (and its audit event, `cron.run` as `mcp`) is the same as pressing Run now.
	async fn run_cron_task(&self, args: &Value) -> Result<Value, String> {
		let project = int(args, "project_id")?;
		let task = int(args, "task_id")?;
		self.json(
			Method::POST,
			&format!("{}/cron/{task}/run", self.project_path(project)),
			None,
		)
		.await
	}

	/// `restart` and `stop`, which the API has for every kind (a stack restarts one
	/// service).
	async fn verb(&self, args: &Value, verb: &str) -> Result<Value, String> {
		let (project, kind, id) = target(args)?;
		let base = self.project_path(project);
		let path = match (kind, verb) {
			(Kind::Stack, "restart") => {
				let service = word(args, "service")?
					.ok_or_else(|| "restarting a stack needs a `service`".to_owned())?;
				format!("{base}/stacks/{id}/services/{service}/restart")
			}
			_ => format!("{base}/{}/{id}/{verb}", kind.segment()),
		};
		self.json(Method::POST, &path, None).await
	}

	async fn set_env_var(&self, args: &Value) -> Result<Value, String> {
		let project = int(args, "project_id")?;
		let application = int(args, "application_id")?;
		let key = text(args, "key")?;
		let value = args
			.get("value")
			.and_then(Value::as_str)
			.ok_or_else(|| "`value` is required".to_owned())?;
		// The value goes in and never comes back: the API answers with the key only.
		self.json(
			Method::POST,
			&format!(
				"{}/applications/{application}/env-vars",
				self.project_path(project)
			),
			Some(json!({ "key": key, "value": value })),
		)
		.await
	}

	async fn template_deploy(&self, args: &Value) -> Result<Value, String> {
		let project = int(args, "project_id")?;
		let template =
			word(args, "template")?.ok_or_else(|| "`template` is required".to_owned())?;
		let mut body = Map::new();
		body.insert("server_id".to_owned(), json!(int(args, "server_id")?));
		for key in ["environment_id", "name", "inputs"] {
			if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
				body.insert(key.to_owned(), v.clone());
			}
		}
		self.json(
			Method::POST,
			&format!("{}/templates/{template}/deploy", self.project_path(project)),
			Some(Value::Object(body)),
		)
		.await
	}

	/// Deleting needs the `delete` scope and the resource's exact name, typed again, so
	/// an agent cannot delete the wrong thing on a stray id.
	async fn delete(&self, args: &Value) -> Result<Value, String> {
		let (project, kind, id) = target(args)?;
		if !self.grant.allows(Scope::Delete) {
			return Err("403: this token lacks the `delete` scope".to_owned());
		}
		let confirm = args
			.get("confirm")
			.and_then(Value::as_str)
			.filter(|c| !c.is_empty())
			.ok_or_else(|| {
				"`confirm` is required: pass the resource's exact name to delete it".to_owned()
			})?;
		let name = self.name_of(project, kind, id).await?;
		if confirm != name {
			return Err(
				"`confirm` does not match the resource's name; nothing was deleted".to_owned(),
			);
		}
		let path = format!("{}/{}/{id}", self.project_path(project), kind.segment());
		self.json(Method::DELETE, &path, None).await
	}

	// --- resources -------------------------------------------------------------

	/// The project tree as MCP resources: the project list, and one tree per project.
	pub async fn resources(&self) -> Result<Value, String> {
		let projects = self
			.get(&format!("/teams/{}/projects", self.team().0))
			.await?;
		let mut list = vec![json!({
			"uri": "podkit://projects",
			"name": "projects",
			"description": "Every project this token can see",
			"mimeType": "application/json",
		})];
		for project in projects.as_array().into_iter().flatten().take(200) {
			let (Some(id), Some(name)) = (project["id"].as_i64(), project["name"].as_str()) else {
				continue;
			};
			list.push(json!({
				"uri": format!("podkit://projects/{id}/resources"),
				"name": format!("{name}: resources"),
				"description": "Applications, databases, models and stacks of the project",
				"mimeType": "application/json",
			}));
		}
		Ok(json!({ "resources": list }))
	}

	pub async fn read(&self, uri: &str) -> Result<Value, String> {
		let rest = uri
			.strip_prefix("podkit://projects")
			.ok_or_else(|| format!("unknown resource {uri}"))?;
		let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
		let value = match parts.as_slice() {
			[] => self.run("list_projects", &json!({})).await,
			[p, "resources"] => {
				self.run("list_resources", &json!({ "project_id": p }))
					.await
			}
			[p, kind, id] => {
				self.run(
					"get_resource",
					&json!({ "project_id": p, "kind": kind, "id": id }),
				)
				.await
			}
			_ => Err(format!("unknown resource {uri}")),
		}?;
		let mut value = value;
		scrub(&mut value);
		Ok(json!({ "contents": [{
			"uri": uri,
			"mimeType": "application/json",
			"text": limit(serde_json::to_string_pretty(&value).unwrap_or_default()),
		}] }))
	}
}

pub fn resource_templates() -> Value {
	json!({ "resourceTemplates": [
		{
			"uriTemplate": "podkit://projects/{project_id}/resources",
			"name": "project-resources",
			"description": "Applications, databases, models and stacks of one project",
			"mimeType": "application/json",
		},
		{
			"uriTemplate": "podkit://projects/{project_id}/{kind}/{id}",
			"name": "resource",
			"description": "One application, database, model or stack (kind: application, database, model, stack)",
			"mimeType": "application/json",
		},
	] })
}

// --- prompts -------------------------------------------------------------------------

pub fn prompts() -> Value {
	json!({ "prompts": [{
		"name": "diagnose_failing_deploy",
		"description": "Find out why an application's deploy fails and suggest a fix",
		"arguments": [
			{ "name": "project_id", "description": "Project id", "required": true },
			{ "name": "application_id", "description": "Application id", "required": true },
		],
	}] })
}

pub fn prompt(params: &Value) -> Result<Value, String> {
	if params.get("name").and_then(Value::as_str) != Some("diagnose_failing_deploy") {
		return Err("unknown prompt".to_owned());
	}
	let argument = |key: &str| {
		let v = &params["arguments"][key];
		v.as_str()
			.map(str::to_owned)
			.or_else(|| v.as_i64().map(|n| n.to_string()))
			.filter(|s| s.parse::<i64>().is_ok())
			.ok_or_else(|| format!("`{key}` must be a number"))
	};
	let (project, application) = (argument("project_id")?, argument("application_id")?);
	Ok(json!({
		"description": "Diagnose a failing deploy",
		"messages": [{
			"role": "user",
			"content": { "type": "text", "text": format!(
				"Application {application} in project {project} is failing to deploy. Find out why.\n\n\
				1. Call get_resource (kind application, project_id {project}, id {application}) and read \
				recent_deployments: the status and error of the latest ones.\n\
				2. Call get_logs for the same application with tail 200 to see what the container printed.\n\
				3. If it never started, call get_scan to see whether a vulnerability or a missing setting is the cause.\n\
				4. Explain the most likely cause in plain words and what to change. Do not deploy, restart, \
				stop or delete anything until the user agrees; if an environment variable is missing, \
				ask for its value rather than guessing it."
			) },
		}],
	}))
}

// --- tool definitions ----------------------------------------------------------------

fn tool(
	name: &str,
	description: &str,
	properties: &Value,
	required: &[&str],
	hints: &Value,
) -> Value {
	json!({
		"name": name,
		"description": description,
		"inputSchema": {
			"type": "object",
			"properties": properties,
			"required": required,
			"additionalProperties": false,
		},
		"annotations": hints,
	})
}

fn read_only() -> Value {
	json!({ "readOnlyHint": true, "openWorldHint": false })
}

fn writes() -> Value {
	json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false })
}

fn destroys() -> Value {
	json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false })
}

#[allow(clippy::too_many_lines, reason = "one table of tool definitions")]
pub fn tools() -> Vec<Value> {
	let project = json!({ "type": "integer", "description": "Project id" });
	let kind = json!({
		"type": "string",
		"enum": ["application", "database", "model", "stack"],
		"description": "Kind of resource",
	});
	let id = json!({ "type": "integer", "description": "Resource id" });
	let on = json!({ "project_id": project, "kind": kind, "id": id });
	let on_app = json!({ "project_id": project, "kind": { "type": "string", "enum": ["application", "stack"], "description": "Kind of resource (default application)" }, "id": id });
	let service = json!({ "type": "string", "description": "Service name; needed for a stack's logs and restart" });
	vec![
		tool(
			"list_projects",
			"List the projects this token can see.",
			&json!({}),
			&[],
			&read_only(),
		),
		tool(
			"list_resources",
			"List a project's applications, databases, models and stacks.",
			&json!({ "project_id": project }),
			&["project_id"],
			&read_only(),
		),
		tool(
			"get_resource",
			"Show one resource. Applications include their latest deployments and any error. Secrets are never included.",
			&on,
			&["project_id", "kind", "id"],
			&read_only(),
		),
		tool(
			"get_logs",
			"Last lines of a resource's logs (at most 500, newest last).",
			&json!({
				"project_id": project, "kind": kind, "id": id, "service": service,
				"tail": { "type": "integer", "minimum": 1, "maximum": MAX_TAIL, "description": "Lines to return (default 100)" },
			}),
			&["project_id", "kind", "id"],
			&read_only(),
		),
		tool(
			"get_metrics",
			"CPU, memory and network of a resource over 1h, 24h or 7d (the newest 60 points).",
			&json!({
				"project_id": project, "kind": kind, "id": id,
				"range": { "type": "string", "enum": ["1h", "24h", "7d"] },
			}),
			&["project_id", "kind", "id"],
			&read_only(),
		),
		tool(
			"get_scan",
			"Security scan results and badge of a resource.",
			&on,
			&["project_id", "kind", "id"],
			&read_only(),
		),
		tool(
			"list_templates",
			"List one-click templates, optionally filtered.",
			&json!({ "q": { "type": "string" }, "category": { "type": "string" } }),
			&[],
			&read_only(),
		),
		tool(
			"deploy",
			"Deploy an application or a stack. Needs the `deploy` scope.",
			&on_app,
			&["project_id", "id"],
			&writes(),
		),
		tool(
			"restart",
			"Restart a resource (a stack restarts one service). Needs the `deploy` scope.",
			&json!({ "project_id": project, "kind": kind, "id": id, "service": service }),
			&["project_id", "kind", "id"],
			&writes(),
		),
		tool(
			"stop",
			"Stop a resource. Needs the `deploy` scope.",
			&on,
			&["project_id", "kind", "id"],
			&writes(),
		),
		tool(
			"set_env_var",
			"Set an application's environment variable. The value is write-only: it is stored encrypted and never shown again. Needs the `write` scope.",
			&json!({
				"project_id": project,
				"application_id": { "type": "integer" },
				"key": { "type": "string" },
				"value": { "type": "string" },
			}),
			&["project_id", "application_id", "key", "value"],
			&writes(),
		),
		tool(
			"create_from_template",
			"Deploy a template into a project. Needs the `deploy` scope.",
			&json!({
				"project_id": project,
				"template": { "type": "string", "description": "Template id from list_templates" },
				"server_id": { "type": "integer" },
				"environment_id": { "type": "integer" },
				"name": { "type": "string" },
				"inputs": { "type": "object", "additionalProperties": { "type": "string" } },
			}),
			&["project_id", "template", "server_id"],
			&writes(),
		),
		tool(
			"run_cron_task",
			"Run a scheduled task now, whatever its schedule says. It is queued and returns at once; the task's own concurrency setting still applies. Needs the `deploy` scope.",
			&json!({
				"project_id": project,
				"task_id": { "type": "integer", "description": "Scheduled task id" },
			}),
			&["project_id", "task_id"],
			&writes(),
		),
		tool(
			"delete_resource",
			"Delete an application, database, model or stack. Needs the `delete` scope, and `confirm` must be the resource's exact name.",
			&json!({
				"project_id": project, "kind": kind, "id": id,
				"confirm": { "type": "string", "description": "The resource's exact name" },
			}),
			&["project_id", "kind", "id", "confirm"],
			&destroys(),
		),
	]
}

// --- arguments -----------------------------------------------------------------------

fn int(args: &Value, key: &str) -> Result<i64, String> {
	let v = args.get(key);
	v.and_then(Value::as_i64)
		.or_else(|| v.and_then(Value::as_str).and_then(|s| s.parse().ok()))
		.ok_or_else(|| format!("`{key}` must be an integer"))
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
	args.get(key)
		.and_then(Value::as_str)
		.filter(|s| !s.is_empty())
		.ok_or_else(|| format!("`{key}` is required"))
}

/// A name that goes into a URL path: letters, digits and `-_.` only.
fn word<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
	match args.get(key).and_then(Value::as_str) {
		None | Some("") => Ok(None),
		Some(s)
			if s.chars()
				.all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) =>
		{
			Ok(Some(s))
		}
		Some(_) => Err(format!("`{key}` may only hold letters, digits and - _ .")),
	}
}

fn target(args: &Value) -> Result<(i64, Kind, i64), String> {
	let kind = match args.get("kind").and_then(Value::as_str) {
		None => Kind::Application,
		Some(raw) => Kind::parse(raw)
			.ok_or_else(|| "`kind` must be application, database, model or stack".to_owned())?,
	};
	Ok((int(args, "project_id")?, kind, int(args, "id")?))
}

/// How many log lines were asked for, kept between 1 and [`MAX_TAIL`].
pub fn tail_of(args: &Value) -> u32 {
	args.get("tail")
		.and_then(Value::as_u64)
		.map_or(DEFAULT_TAIL, |t| u32::try_from(t).unwrap_or(MAX_TAIL))
		.clamp(1, MAX_TAIL)
}

fn encode(raw: &str) -> String {
	let mut out = String::new();
	for b in raw.bytes() {
		if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
			out.push(char::from(b));
		} else {
			let _ = write!(out, "%{b:02X}");
		}
	}
	out
}

// --- output --------------------------------------------------------------------------

/// What a refused or failed API call says: the status and the API's own message.
fn refusal(status: StatusCode, body: &Value) -> String {
	let message = body["error"].as_str().unwrap_or("request failed");
	format!("{}: {message}", status.as_u16())
}

/// Keys whose value is never shown to an agent, wherever they appear.
const SECRET_WORDS: [&str; 14] = [
	"password",
	"passwd",
	"passphrase",
	"secret",
	"token",
	"private",
	"credential",
	"api_key",
	"apikey",
	"access_key",
	"authorization",
	"cookie",
	"connection_string",
	"dsn",
];

fn is_secret_key(key: &str) -> bool {
	let key = key.to_ascii_lowercase();
	SECRET_WORDS.iter().any(|w| key.contains(w))
}

/// Masks every secret-looking field, at any depth, and the password in any URL. The
/// token may carry `read:sensitive`; an agent still never sees secrets.
pub fn scrub(value: &mut Value) {
	match value {
		Value::Object(map) => {
			for (key, v) in map.iter_mut() {
				if is_secret_key(key) && !matches!(v, Value::Bool(_) | Value::Null) {
					*v = Value::String(MASK.to_owned());
				} else {
					scrub(v);
				}
			}
		}
		Value::Array(items) => items.iter_mut().for_each(scrub),
		Value::String(s) => *s = scrub_text(s),
		_ => {}
	}
}

/// `text` without the `user:password@` of any `scheme://` URL in it.
pub fn scrub_text(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(at) = rest.find("://") {
		let (head, tail) = rest.split_at(at + 3);
		out.push_str(head);
		let end = tail
			.find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'')
			.unwrap_or(tail.len());
		let (authority, after) = tail.split_at(end);
		out.push_str(
			authority
				.rsplit_once('@')
				.map_or(authority, |(_, host)| host),
		);
		rest = after;
	}
	out.push_str(rest);
	out
}

/// Cuts `text` to [`MAX_OUTPUT`] characters, saying so.
pub fn limit(text: String) -> String {
	if text.chars().count() <= MAX_OUTPUT {
		return text;
	}
	let mut cut: String = text.chars().take(MAX_OUTPUT).collect();
	cut.push_str("\n[output truncated]");
	cut
}

/// The lines of an SSE log stream (`event: log`, data `{"stream","line"}`), the last
/// `tail` of them, each bounded; stderr lines are marked.
pub fn log_text(sse: &str, tail: u32) -> String {
	let mut lines: Vec<String> = Vec::new();
	let mut event = "";
	for raw in sse.lines() {
		if let Some(name) = raw.strip_prefix("event:") {
			event = name.trim();
		} else if let Some(data) = raw.strip_prefix("data:")
			&& event == "log"
			&& let Ok(entry) = serde_json::from_str::<Value>(data.trim())
		{
			let line = entry["line"].as_str().unwrap_or_default();
			let line: String = line.chars().take(MAX_LINE).collect();
			let stderr = entry["stream"].as_str() == Some("stderr");
			lines.push(if stderr {
				format!("[stderr] {line}")
			} else {
				line
			});
		}
	}
	let keep = usize::try_from(tail).unwrap_or(usize::MAX);
	let from = lines.len().saturating_sub(keep);
	let mut text = lines[from..].join("\n");
	// Over the output limit, the newest lines matter most: drop from the front.
	let over = text.chars().count().saturating_sub(MAX_OUTPUT);
	if over > 0 {
		text = format!(
			"[older lines cut]\n{}",
			text.chars().skip(over).collect::<String>()
		);
	}
	if text.is_empty() {
		"(no log lines)".to_owned()
	} else {
		text
	}
}
