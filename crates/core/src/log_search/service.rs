//! Log use cases shared by the JSON API and the web UI: authorize, then scope to the team, then
//! validate, then touch the database or the agents.

use std::collections::{BTreeMap, HashMap};

use proto::{LogLevel, LogSearchRequest};
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{
	DEFAULT_LIMIT, Export, ExportKind, MAX_LIMIT, MAX_RETENTION_DAYS, MAX_TEXT_LEN, MIN_MAX_MB,
	Merged, Settings, merge,
};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{self, Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId, UserId};
use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;

const MAX_URL_LEN: usize = 500;
const MAX_TENANT_LEN: usize = 200;
const MAX_AUTHORIZATION_LEN: usize = 2000;

fn invalid(what: &str) -> ServerError {
	ServerError::Validation(format!("invalid {what}"))
}

// --- settings ------------------------------------------------------------------

/// The columns of a `log_settings` row.
#[derive(sqlx::FromRow)]
struct Row {
	retention_days: i32,
	max_mb: i32,
	export_kind: Option<String>,
	export_url: Option<String>,
	export_tenant: Option<String>,
	export_authorization: Option<Vec<u8>>,
}

const ROW: &str =
	"SELECT retention_days, max_mb, export_kind, export_url, export_tenant, export_authorization
	FROM log_settings WHERE team_id = $1";

impl Row {
	fn settings(&self) -> Settings {
		let export = match (
			self.export_kind.as_deref().and_then(ExportKind::parse),
			&self.export_url,
		) {
			(Some(kind), Some(url)) => Some(Export {
				kind,
				url: url.clone(),
				tenant: self.export_tenant.clone(),
				has_authorization: self.export_authorization.is_some(),
			}),
			_ => None,
		};
		Settings {
			retention_days: self.retention_days,
			max_mb: self.max_mb,
			export,
		}
	}
}

/// The team's settings (the defaults when it never chose). Needs `read logs`.
pub async fn settings(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Settings> {
	authorize(db, user, team, Action::Read, Resource::Logs).await?;
	stored(db, team).await
}

/// [`settings`] without the permission check, for the sampler and the writer.
pub(super) async fn stored(db: &PgPool, team: TeamId) -> AppResult<Settings> {
	Ok(sqlx::query_as::<_, Row>(ROW)
		.bind(team)
		.fetch_optional(db)
		.await?
		.map_or_else(Settings::default, |r| r.settings()))
}

/// The sealed header of the team, for the agent's configuration only.
pub(super) async fn sealed_authorization(db: &PgPool, team: TeamId) -> AppResult<Option<Vec<u8>>> {
	Ok(sqlx::query_as::<_, Row>(ROW)
		.bind(team)
		.fetch_optional(db)
		.await?
		.and_then(|r| r.export_authorization))
}

/// What a save sets. The `Authorization` value is write-only: empty keeps the one saved.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsInput {
	pub retention_days: i32,
	pub max_mb: i32,
	/// `loki`, `otlp`, or empty for no forwarding.
	#[serde(default)]
	pub export_kind: String,
	#[serde(default)]
	pub export_url: String,
	#[serde(default)]
	pub export_tenant: String,
	/// The value of the `Authorization` header (`Bearer ...`, `Basic ...`). Empty keeps what is
	/// saved.
	#[serde(default)]
	pub export_authorization: String,
	/// Forget the saved header.
	#[serde(default)]
	pub clear_authorization: bool,
}

/// An input that passed [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valid {
	pub retention_days: i32,
	pub max_mb: i32,
	pub export: Option<(ExportKind, String, Option<String>)>,
	pub authorization: Option<String>,
	pub clear_authorization: bool,
}

/// Checks and normalizes an input.
///
/// # Errors
/// `Validation` naming the first field that is wrong.
pub fn validate(input: &SettingsInput) -> AppResult<Valid> {
	if !(1..=MAX_RETENTION_DAYS).contains(&input.retention_days) {
		return Err(ServerError::Validation(format!(
			"keep the logs for 1 to {MAX_RETENTION_DAYS} days"
		)));
	}
	if !(MIN_MAX_MB..=1_048_576).contains(&input.max_mb) {
		return Err(ServerError::Validation(format!(
			"the size cap is {MIN_MAX_MB} MB or more per server"
		)));
	}
	let kind = input.export_kind.trim();
	let export = if kind.is_empty() {
		None
	} else {
		let kind = ExportKind::parse(kind).ok_or_else(|| invalid("export type"))?;
		let url = input.export_url.trim();
		let parsed = url::Url::parse(url).ok();
		if url.len() > MAX_URL_LEN
			|| url.chars().any(|c| c.is_control() || c.is_whitespace())
			|| !parsed.is_some_and(|u| {
				matches!(u.scheme(), "http" | "https")
					&& u.host_str().is_some()
					&& u.username().is_empty()
					&& u.password().is_none()
			}) {
			return Err(ServerError::Validation(
				"the export address is an http:// or https:// URL without a user name or password in it"
					.to_owned(),
			));
		}
		let tenant = input.export_tenant.trim();
		if tenant.chars().count() > MAX_TENANT_LEN || tenant.chars().any(char::is_control) {
			return Err(invalid("tenant"));
		}
		Some((
			kind,
			url.to_owned(),
			(!tenant.is_empty()).then(|| tenant.to_owned()),
		))
	};
	let authorization = input.export_authorization.trim();
	if authorization.len() > MAX_AUTHORIZATION_LEN || authorization.chars().any(char::is_control) {
		return Err(invalid("authorization header"));
	}
	Ok(Valid {
		retention_days: input.retention_days,
		max_mb: input.max_mb,
		authorization: (export.is_some() && !authorization.is_empty())
			.then(|| authorization.to_owned()),
		clear_authorization: input.clear_authorization || export.is_none(),
		export,
	})
}

/// Saves the team's settings and hands them to its agents. Needs `update logs`.
pub async fn update_settings(
	s: &AppState,
	user: UserId,
	team: TeamId,
	input: &SettingsInput,
) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Update, Resource::Logs).await?;
	let v = validate(input)?;
	if let Some((_, url, _)) = &v.export {
		// Resolved now, so an address podkit will not send to is refused when it is typed;
		// the agent applies the same policy at every batch.
		s.outbound
			.vet_url(url)
			.await
			.map_err(|_| crate::net::outbound::refused())?;
	}
	let before = stored(&s.db, team).await?;
	let sealed = match &v.authorization {
		Some(value) => Some(s.secrets.encrypt(value).map_err(|e| {
			tracing::error!("failed to encrypt the log export header: {e}");
			ServerError::Internal
		})?),
		None => None,
	};
	let (kind, url, tenant) = match &v.export {
		Some((kind, url, tenant)) => (Some(kind.as_str()), Some(url.as_str()), tenant.as_deref()),
		None => (None, None, None),
	};
	// A new header replaces the old one; "forget" or no forwarding at all drops it; otherwise
	// the saved one stays.
	sqlx::query(
		"INSERT INTO log_settings (team_id, retention_days, max_mb, export_kind, export_url,
			export_tenant, export_authorization)
		VALUES ($1, $2, $3, $4, $5, $6, $7)
		ON CONFLICT (team_id) DO UPDATE SET
			retention_days = EXCLUDED.retention_days,
			max_mb = EXCLUDED.max_mb,
			export_kind = EXCLUDED.export_kind,
			export_url = EXCLUDED.export_url,
			export_tenant = EXCLUDED.export_tenant,
			export_authorization = CASE
				WHEN $7::BYTEA IS NOT NULL THEN $7
				WHEN $8 THEN NULL
				ELSE log_settings.export_authorization END,
			updated_at = NOW()",
	)
	.bind(team)
	.bind(v.retention_days)
	.bind(v.max_mb)
	.bind(kind)
	.bind(url)
	.bind(tenant)
	.bind(&sealed)
	.bind(v.clear_authorization)
	.execute(&s.db)
	.await?;
	let after = stored(&s.db, team).await?;

	let mut diff = Diff::new()
		.changed(
			"retention_days",
			i64::from(before.retention_days),
			i64::from(after.retention_days),
		)
		.changed("max_mb", i64::from(before.max_mb), i64::from(after.max_mb))
		.changed(
			"export_kind",
			before.export.as_ref().map_or("none", |e| e.kind.as_str()),
			after.export.as_ref().map_or("none", |e| e.kind.as_str()),
		);
	if let Some(export) = &after.export {
		diff = diff.set("export_url", audit::scrub_url(&export.url));
	}
	if v.authorization.is_some() {
		diff = diff.secret("export_authorization");
	} else if v.clear_authorization && before.export.as_ref().is_some_and(|e| e.has_authorization) {
		diff = diff.set("export_authorization", "removed");
	}
	audit::log(
		&s.db,
		user,
		team,
		"log_settings.update",
		Target::new("log_settings", team).named("log retention"),
		diff,
	)
	.await;

	super::sync::push_team(s, team).await;
	Ok(after)
}

// --- what can be searched ---------------------------------------------------------

/// What kind of resource a log source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
	Application,
	Database,
	Model,
	Stack,
}

impl SourceKind {
	pub fn label(self) -> &'static str {
		match self {
			Self::Application => "Application",
			Self::Database => "Database",
			Self::Model => "Model",
			Self::Stack => "Stack",
		}
	}
}

/// One resource of the team whose logs are stored: the `podkit.service` the agent files them
/// under and what the page calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
	/// `app-7`, `db-3`, `model-2` or `stack-4` (a stack stands for all its services).
	pub service: String,
	pub kind: SourceKind,
	pub name: String,
	pub project: String,
	pub server: ServerId,
}

impl Source {
	/// `Project / name`.
	pub fn title(&self) -> String {
		format!("{} / {}", self.project, self.name)
	}

	/// Whether a stored line's service (`app-7`, `stack-4-web`) belongs to this source.
	pub fn owns(&self, svc: &str) -> bool {
		svc == self.service
			|| (self.kind == SourceKind::Stack
				&& svc
					.strip_prefix(&self.service)
					.is_some_and(|rest| rest.starts_with('-')))
	}
}

/// The team's resources the user may read, ordered by project and name. Needs `read logs`.
pub async fn sources(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Source>> {
	authorize(db, user, team, Action::Read, Resource::Logs).await?;
	let perms = permission::granted(db, user, team).await?;
	let mut out = Vec::new();
	for (kind, prefix, resource, table) in [
		(
			SourceKind::Application,
			"app",
			Resource::Application,
			"applications",
		),
		(SourceKind::Database, "db", Resource::Database, "databases"),
		(SourceKind::Model, "model", Resource::Model, "models"),
		(SourceKind::Stack, "stack", Resource::Stack, "stacks"),
	] {
		if !perms.can(Action::Read, resource) {
			continue;
		}
		let rows =
			sqlx::query_as::<_, (i64, String, String, ServerId)>(sqlx::AssertSqlSafe(format!(
				"SELECT r.id, r.name, p.name, r.server_id FROM {table} r
			JOIN projects p ON p.id = r.project_id
			WHERE p.team_id = $1 ORDER BY p.name, r.name, r.id"
			)))
			.bind(team)
			.fetch_all(db)
			.await?;
		out.extend(rows.into_iter().map(|(id, name, project, server)| Source {
			service: format!("{prefix}-{id}"),
			kind,
			name,
			project,
			server,
		}));
	}
	out.sort_by(|a, b| (&a.project, &a.name, &a.service).cmp(&(&b.project, &b.name, &b.service)));
	Ok(out)
}

// --- search ----------------------------------------------------------------------

/// What a search asks.
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
	/// Sources (`app-7`); empty searches all the team's.
	#[serde(default)]
	pub services: Vec<String>,
	/// Unix milliseconds, inclusive. Default: the last hour.
	#[serde(default)]
	pub since: Option<i64>,
	/// Unix milliseconds, exclusive.
	#[serde(default)]
	pub until: Option<i64>,
	#[serde(default)]
	pub text: Option<String>,
	#[serde(default)]
	pub regex: bool,
	#[serde(default)]
	pub levels: Vec<LogLevel>,
	#[serde(default)]
	pub deployment: Option<i64>,
	#[serde(default)]
	pub limit: Option<u32>,
	/// Keep the newest lines (the default) or the oldest when more match than `limit`.
	#[serde(default = "yes")]
	pub newest: bool,
}

fn yes() -> bool {
	true
}

impl Default for Query {
	fn default() -> Self {
		Self {
			services: Vec::new(),
			since: None,
			until: None,
			text: None,
			regex: false,
			levels: Vec::new(),
			deployment: None,
			limit: None,
			newest: true,
		}
	}
}

/// The answer to a search, with what is needed to show it.
#[derive(Debug, Clone, Default)]
pub struct Answer {
	pub merged: Merged,
	/// The sources that were asked, in the order of the picker.
	pub sources: Vec<Source>,
	/// Servers that did not answer, by name: their lines are missing.
	pub unreachable: Vec<String>,
}

impl Answer {
	/// What to call a stored line's service: the resource's name, and the compose service of a
	/// stack's line.
	pub fn label(&self, svc: &str) -> String {
		match self.sources.iter().find(|s| s.owns(svc)) {
			Some(source) if source.service == svc => source.name.clone(),
			Some(source) => format!(
				"{} / {}",
				source.name,
				svc.strip_prefix(&source.service)
					.and_then(|r| r.strip_prefix('-'))
					.unwrap_or(svc)
			),
			None => svc.to_owned(),
		}
	}
}

fn now_ms() -> i64 {
	i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// Searches the stored logs of the team's resources on every server that holds some, and merges
/// the answers. Needs `read logs`.
///
/// # Errors
/// `Validation` for a query that cannot run (an expression that does not compile); `NotFound` for
/// a resource that is not the team's; a server that does not answer is listed in
/// [`Answer::unreachable`] instead.
pub async fn search(s: &AppState, user: UserId, team: TeamId, query: &Query) -> AppResult<Answer> {
	let all = sources(&s.db, user, team).await?;
	let wanted: Vec<Source> = if query.services.is_empty() {
		all
	} else {
		let mut picked = Vec::new();
		for service in &query.services {
			let source = all
				.iter()
				.find(|a| &a.service == service)
				.ok_or(ServerError::NotFound)?;
			if !picked.contains(source) {
				picked.push(source.clone());
			}
		}
		picked
	};
	if let Some(text) = &query.text
		&& text.chars().count() > MAX_TEXT_LEN
	{
		return Err(ServerError::Validation(format!(
			"the search text is {MAX_TEXT_LEN} characters at most"
		)));
	}
	let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
	let since = query.since.unwrap_or_else(|| now_ms() - 3_600_000);
	if query.until.is_some_and(|u| u <= since) {
		return Err(ServerError::Validation(
			"the end of the time range is before its start".to_owned(),
		));
	}
	// Nothing to ask about: an empty list would mean "everything the agent holds".
	if wanted.is_empty() {
		return Ok(Answer::default());
	}

	let mut by_server: BTreeMap<i64, Vec<String>> = BTreeMap::new();
	for source in &wanted {
		by_server
			.entry(source.server.0)
			.or_default()
			.push(source.service.clone());
	}
	let servers: HashMap<i64, Server> = servers::list_by_team(&s.db, team)
		.await?
		.into_iter()
		.map(|server| (server.id.0, server))
		.collect();

	let asks = by_server.into_iter().map(|(id, services)| {
		let server = servers.get(&id).cloned();
		let req = LogSearchRequest {
			services,
			since: Some(since),
			until: query.until,
			text: query.text.clone().filter(|t| !t.is_empty()),
			regex: query.regex,
			levels: query.levels.clone(),
			deployment: query.deployment,
			limit,
			newest: query.newest,
		};
		async move {
			let Some(server) = server else {
				return (
					ServerId(id),
					"a server".to_owned(),
					Err(ServerError::ServerNotFound),
				);
			};
			let name = server.name.clone();
			if server.status != ServerStatus::Active && !server.is_local {
				return (server.id, name, Err(ServerError::AgentUnavailable));
			}
			let result = async {
				let agent = s.agents.client(s, &server).await?;
				agent.log_search(&req).await
			}
			.await;
			(server.id, name, result)
		}
	});
	let mut parts = Vec::new();
	let mut unreachable = Vec::new();
	for (id, name, result) in futures_util::future::join_all(asks).await {
		match result {
			Ok(found) => parts.push((id, found)),
			// The agent refused the query: the same for every server, and the reader's to fix.
			Err(e @ ServerError::Validation(_)) => return Err(e),
			Err(e) => {
				tracing::debug!(server = %id, error = %e, "log search: a server did not answer");
				unreachable.push(name);
			}
		}
	}
	Ok(Answer {
		merged: merge(parts, limit as usize, query.newest),
		sources: wanted,
		unreachable,
	})
}
