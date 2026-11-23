//! Audit log: who did what to which resource, from where. Every service function that
//! changes something calls [`record`] once it has succeeded.
//!
//! The actor is not threaded through the services as an argument. [`layer`] runs around
//! every request, notes the caller's address and user agent, and the token gate notes
//! which API token (if any) made the call; [`by`] reads that back. Anything that runs
//! outside a request (a push webhook, a schedule) is the `system` actor, see [`system`].
//!
//! The log is append-only and secrets stay out of it: a [`Diff`] holds names, ids and
//! settings, never values, and masks the value of any key that looks like a secret to
//! `•••` even if a caller slips one in.

mod routes;
pub mod service;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Request};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{ApiTokenId, AuditEventId, ProjectId, TeamId, UserId};

pub use routes::router;

/// What replaces a secret wherever the log would show one.
pub const MASK: &str = "•••";

/// How the actor reached podkit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
	/// A person with a session.
	User,
	/// An API token, acting as the user who made it.
	Token,
	/// podkit itself: a push webhook, a schedule, a background job.
	System,
	/// The MCP server, on behalf of a user.
	Mcp,
}

impl ActorKind {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::User => "user",
			Self::Token => "token",
			Self::System => "system",
			Self::Mcp => "mcp",
		}
	}

	pub fn parse(raw: &str) -> Option<Self> {
		[Self::User, Self::Token, Self::System, Self::Mcp]
			.into_iter()
			.find(|k| k.as_str() == raw)
	}
}

// --- the request the actor is making -----------------------------------------

#[derive(Debug, Default)]
struct RequestMeta {
	ip: Option<String>,
	user_agent: Option<String>,
	token: Mutex<Option<ApiTokenId>>,
}

tokio::task_local! {
	static REQUEST: Arc<RequestMeta>;
	static QUIET: ();
	static MCP: ();
}

/// Runs `work` as the MCP server: calls it makes with an API token are logged with actor
/// kind `mcp` instead of `token`.
pub async fn as_mcp<F: std::future::Future>(work: F) -> F::Output {
	MCP.scope((), work).await
}

/// Runs `work` without recording anything. A flow that makes many changes (applying a
/// plan, deploying a template) wraps the pieces in this and records itself once.
pub async fn quiet<F: std::future::Future>(work: F) -> F::Output {
	QUIET.scope((), work).await
}

/// [`record`] for the caller of the current request: `user`, or the token acting as them.
pub async fn log(
	db: impl PgExecutor<'_>,
	user: UserId,
	team: TeamId,
	action: &str,
	target: Target,
	diff: Diff,
) {
	record(db, &by(user, team), action, target, diff).await;
}

const MAX_USER_AGENT: usize = 300;

/// Whether `peer` is a reverse proxy we should believe about the client's address: one on
/// this machine or this private network (Traefik in front of podkit).
fn is_proxy(peer: IpAddr) -> bool {
	match peer {
		IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
		IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
	}
}

fn client_ip(req: &Request) -> Option<String> {
	let peer = req
		.extensions()
		.get::<ConnectInfo<SocketAddr>>()
		.map(|c| c.0.ip());
	client_addr(peer, req.headers()).map(|ip| ip.to_string())
}

/// The address of whoever made a request: the socket's peer, or when that is a proxy on this
/// machine or network (Traefik in front of podkit), the first `X-Forwarded-For` entry.
pub fn client_addr(peer: Option<IpAddr>, headers: &axum::http::HeaderMap) -> Option<IpAddr> {
	let forwarded = || {
		headers
			.get("x-forwarded-for")
			.and_then(|v| v.to_str().ok())
			.and_then(|v| v.split(',').next())
			.and_then(|v| v.trim().parse::<IpAddr>().ok())
	};
	// Without a socket (in-process tests) there is nobody to distrust.
	match peer {
		Some(peer) if !is_proxy(peer) => Some(peer),
		_ => forwarded().or(peer),
	}
}

fn user_agent(req: &Request) -> Option<String> {
	let raw = req.headers().get(header::USER_AGENT)?.to_str().ok()?;
	let clean: String = raw
		.chars()
		.filter(|c| !c.is_control())
		.take(MAX_USER_AGENT)
		.collect();
	(!clean.is_empty()).then_some(clean)
}

/// Middleware around the whole app: remembers who is calling for [`by`].
pub async fn layer(req: Request, next: Next) -> Response {
	let meta = Arc::new(RequestMeta {
		ip: client_ip(&req),
		user_agent: user_agent(&req),
		token: Mutex::new(None),
	});
	REQUEST.scope(meta, next.run(req)).await
}

/// Called by the token gate: the rest of this request is made by `token`.
pub fn note_token(token: ApiTokenId) {
	let _ = REQUEST.try_with(|meta| {
		if let Ok(mut slot) = meta.token.lock() {
			*slot = Some(token);
		}
	});
}

/// Who is acting, for one [`record`].
#[derive(Debug, Clone)]
pub struct Ctx {
	pub team: TeamId,
	pub user: Option<UserId>,
	pub token: Option<ApiTokenId>,
	pub kind: ActorKind,
	pub ip: Option<String>,
	pub user_agent: Option<String>,
}

/// The caller of the current request: `user`, or the token acting as them.
pub fn by(user: UserId, team: TeamId) -> Ctx {
	let (token, ip, user_agent) = REQUEST
		.try_with(|m| {
			(
				m.token.lock().ok().and_then(|t| *t),
				m.ip.clone(),
				m.user_agent.clone(),
			)
		})
		.unwrap_or_default();
	Ctx {
		team,
		user: Some(user),
		token,
		kind: if token.is_some() && MCP.try_with(|()| ()).is_ok() {
			ActorKind::Mcp
		} else if token.is_some() {
			ActorKind::Token
		} else {
			ActorKind::User
		},
		ip,
		user_agent,
	}
}

/// podkit acting on its own: a push webhook, a schedule.
pub fn system(team: TeamId) -> Ctx {
	let (ip, user_agent) = REQUEST
		.try_with(|m| (m.ip.clone(), m.user_agent.clone()))
		.unwrap_or_default();
	Ctx {
		team,
		user: None,
		token: None,
		kind: ActorKind::System,
		ip,
		user_agent,
	}
}

impl Ctx {
	/// The same actor, reported as the MCP server (roadmap A7).
	#[must_use]
	#[allow(dead_code, reason = "the MCP server (A7) acts through it")]
	pub fn via_mcp(mut self) -> Self {
		self.kind = ActorKind::Mcp;
		self
	}
}

/// [`system`] for work in a project, whose team is looked up. `None` when the project is
/// gone.
pub async fn system_in_project(db: &sqlx::PgPool, project: ProjectId) -> Option<Ctx> {
	let team = sqlx::query_scalar!(
		r#"SELECT team_id AS "team_id: TeamId" FROM projects WHERE id = $1"#,
		project as ProjectId
	)
	.fetch_optional(db)
	.await
	.ok()
	.flatten()?;
	Some(system(team))
}

// --- what was acted on ---------------------------------------------------------

/// The resource an event is about.
#[derive(Debug, Clone)]
pub struct Target {
	pub kind: &'static str,
	pub id: Option<i64>,
	pub name: Option<String>,
	pub project: Option<ProjectId>,
}

impl Target {
	pub fn new(kind: &'static str, id: impl Into<i64>) -> Self {
		Self {
			kind,
			id: Some(id.into()),
			name: None,
			project: None,
		}
	}

	/// A target with no id of its own: a setting of the team.
	pub const fn none(kind: &'static str) -> Self {
		Self {
			kind,
			id: None,
			name: None,
			project: None,
		}
	}

	/// What to call it in the summary: a name or a slug, never a secret.
	#[must_use]
	pub fn named(mut self, name: impl Into<String>) -> Self {
		self.name = Some(name.into());
		self
	}

	/// The project it lives in, so the log can link to it.
	#[must_use]
	pub fn in_project(mut self, project: ProjectId) -> Self {
		self.project = Some(project);
		self
	}
}

macro_rules! into_i64 {
	($($id:ident),+ $(,)?) => {$(
		impl From<crate::ids::$id> for i64 {
			fn from(id: crate::ids::$id) -> Self {
				id.0
			}
		}
	)+};
}
into_i64!(
	UserId,
	TeamId,
	ProjectId,
	EnvironmentId,
	ServerId,
	ApplicationId,
	DeploymentId,
	EnvVarId,
	DomainId,
	VolumeId,
	DatabaseId,
	BackupTargetId,
	BackupId,
	ModelFileId,
	ModelId,
	StackId,
	NetworkId,
	SshKeyId,
	RegistryId,
	ApiTokenId,
	GitProviderId,
	ScanId,
	ScanIgnoreId,
);

// --- what changed --------------------------------------------------------------

/// Words in a key that mark its value as a secret.
const SECRET_WORDS: [&str; 12] = [
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
];

fn is_secret_key(key: &str) -> bool {
	let key = key.to_ascii_lowercase();
	key == "value" || SECRET_WORDS.iter().any(|w| key.contains(w))
}

/// Masks the value of every secret-looking key, at any depth. Booleans and nulls stay: "a
/// password was set" is worth knowing and gives nothing away.
fn redact(value: &mut Value) {
	match value {
		Value::Object(map) => {
			for (key, v) in map.iter_mut() {
				if is_secret_key(key) && !matches!(v, Value::Bool(_) | Value::Null) {
					*v = Value::String(MASK.to_owned());
				} else {
					redact(v);
				}
			}
		}
		Value::Array(items) => items.iter_mut().for_each(redact),
		_ => {}
	}
}

/// `url` without any `user:password@` part, which may be a credential.
pub fn scrub_url(url: &str) -> String {
	let Some((scheme, rest)) = url.split_once("://") else {
		return url.to_owned();
	};
	let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
	let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
	if path.is_empty() && !rest.contains('/') {
		format!("{scheme}://{host}")
	} else {
		format!("{scheme}://{host}/{path}")
	}
}

/// The `diff` of one event, built field by field.
#[derive(Debug, Clone, Default)]
pub struct Diff(Map<String, Value>);

impl Diff {
	pub fn new() -> Self {
		Self::default()
	}

	/// A field that was set to `value`. Do not pass secrets: a key that looks like one is
	/// masked anyway, but a value under an innocent key would not be.
	#[must_use]
	pub fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
		self.0.insert(key.to_owned(), value.into());
		self
	}

	/// A field that went from `from` to `to`.
	#[must_use]
	pub fn change(self, key: &str, from: impl Into<Value>, to: impl Into<Value>) -> Self {
		let mut pair = Map::new();
		pair.insert("from".to_owned(), from.into());
		pair.insert("to".to_owned(), to.into());
		self.set(key, Value::Object(pair))
	}

	/// A field that went from `from` to `to`; left out when it did not change.
	#[must_use]
	pub fn changed<T: PartialEq + Into<Value>>(self, key: &str, from: T, to: T) -> Self {
		if from == to {
			self
		} else {
			self.change(key, from, to)
		}
	}

	/// A secret field that was written or revealed: recorded as `•••`, never the value.
	#[must_use]
	pub fn secret(mut self, key: &str) -> Self {
		self.0
			.insert(key.to_owned(), Value::String(MASK.to_owned()));
		self
	}

	fn into_value(self) -> Value {
		let mut value = Value::Object(self.0);
		redact(&mut value);
		value
	}
}

// --- writing -------------------------------------------------------------------

fn past_tense(verb: &str) -> Option<&'static str> {
	Some(match verb {
		"create" => "Created",
		"update" => "Updated",
		"delete" | "remove" => "Deleted",
		"set" => "Set",
		"unset" | "clear" => "Cleared",
		"deploy" => "Deployed",
		"redeploy" => "Redeployed",
		"rollback" => "Rolled back",
		"cancel" => "Cancelled",
		"start" => "Started",
		"stop" => "Stopped",
		"restart" => "Restarted",
		"rotate" => "Rotated",
		"reveal" => "Revealed",
		"retry" => "Retried",
		"test" => "Tested",
		"link" => "Linked",
		"unlink" => "Unlinked",
		"attach" => "Attached",
		"detach" => "Detached",
		"restore" => "Restored",
		"rename" => "Renamed",
		"reorder" => "Reordered",
		"import" => "Imported",
		"apply" => "Applied",
		"bootstrap" => "Bootstrapped",
		"migrate" => "Migrated",
		"ignore" => "Ignored",
		"scan" => "Scanned",
		"revoke" => "Revoked",
		"add" => "Added",
		"upload" => "Uploaded",
		"disable" => "Disabled",
		"revert" => "Reverted",
		"install" => "Installed",
		_ => return None,
	})
}

/// "Deployed application web": what the log shows in its list.
fn summary(action: &str, target: &Target) -> String {
	let verb = action.rsplit('.').next().unwrap_or(action);
	let verb = past_tense(verb).map_or_else(|| verb.replace('_', " "), str::to_owned);
	let kind = target.kind.replace('_', " ");
	match &target.name {
		Some(name) => format!("{verb} {kind} {name}"),
		None => format!("{verb} {kind}"),
	}
}

/// Writes one audit event. Call it once the change has been made: a request that fails
/// leaves nothing behind. A failure to write is logged, not returned: the change is
/// already done and cannot be taken back by failing the call.
pub async fn record(db: impl PgExecutor<'_>, ctx: &Ctx, action: &str, target: Target, diff: Diff) {
	if QUIET.try_with(|()| ()).is_ok() {
		return;
	}
	let summary = summary(action, &target);
	let result = sqlx::query!(
		r#"
			INSERT INTO audit_events (
				id, team_id, actor_kind, actor_user_id, actor_token_id, action,
				target_kind, target_id, project_id, summary, diff, ip, user_agent
			)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
		"#,
		AuditEventId::generate() as AuditEventId,
		ctx.team as TeamId,
		ctx.kind.as_str(),
		ctx.user.map(|u| u.0),
		ctx.token.map(|t| t.0),
		action,
		target.kind,
		target.id,
		target.project.map(|p| p.0),
		summary,
		diff.into_value(),
		ctx.ip.as_deref(),
		ctx.user_agent.as_deref(),
	)
	.execute(db)
	.await;
	if let Err(e) = result {
		tracing::error!(action, team = %ctx.team, "failed to write audit event: {e}");
	}
}

/// One row of the log, with the actor's name where it is still known.
#[derive(Debug, Clone)]
pub struct Event {
	pub id: AuditEventId,
	pub actor_kind: ActorKind,
	pub actor_user: Option<UserId>,
	pub actor_name: Option<String>,
	pub actor_token: Option<ApiTokenId>,
	pub actor_token_name: Option<String>,
	pub action: String,
	pub target_kind: String,
	pub target_id: Option<i64>,
	pub project: Option<ProjectId>,
	pub summary: String,
	pub diff: Value,
	pub ip: Option<String>,
	pub user_agent: Option<String>,
	pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests;
