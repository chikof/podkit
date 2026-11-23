//! MCP servers as resources, and the links that let applications reach them.
//!
//! An MCP server is an application (an image, a git repository or a template) that
//! also has a row in `mcp_servers`: how its tools are reached. A server that speaks
//! streamable HTTP is reached as it is; one that speaks stdio runs behind
//! `podkit-agent --mcp-bridge` (`docs/mcp-resources.md`). It lives on its
//! environment's network under the alias `mcp-<slug>` and is never routed publicly.
//!
//! An application links to a server like it links to a model: the gateway
//! (`docs/gateway.md`) serves `http://llm.podkit.internal:8088/mcp/<slug>` to the
//! application's own key for that link, strips the credentials, forwards the request and
//! counts it. The application's deploys get `PODKIT_MCP_SERVERS` and `MCP_<NAME>_URL` /
//! `MCP_<NAME>_KEY`.
//!
//! Routes live under `/api/v1/teams/{team}/projects/{project}/mcp-servers` and
//! `.../applications/{app}/mcp`.

mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use proto::{GATEWAY_HOST, GATEWAY_PORT, GatewayMcp, GatewayMcpKey};
use serde::Serialize;
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId};
use crate::links::hash_key;
use crate::state::AppState;

pub use routes::{app_router, router};

/// How the server's tools are reached inside its container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Transport {
	/// Streamable HTTP, served by the image itself.
	Http,
	/// Standard input and output, wrapped by podkit's bridge.
	Stdio,
}

impl Transport {
	/// The lowercase form used for storage and display.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Http => "http",
			Self::Stdio => "stdio",
		}
	}

	/// Parses the stored or submitted form.
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"http" => Some(Self::Http),
			"stdio" => Some(Self::Stdio),
			_ => None,
		}
	}
}

/// Row of `mcp_servers`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct McpServer {
	#[allow(dead_code)]
	pub application_id: ApplicationId,
	pub transport: Transport,
	pub path: String,
	/// stdio only: one argument per line; `None` runs the image's own command.
	pub command: Option<String>,
	#[allow(dead_code)]
	pub created_at: OffsetDateTime,
}

impl McpServer {
	/// The command line as arguments.
	#[must_use]
	pub fn args(&self) -> Vec<String> {
		args_of(self.command.as_deref())
	}
}

/// Splits the stored command into its arguments.
#[must_use]
pub fn args_of(command: Option<&str>) -> Vec<String> {
	command
		.unwrap_or_default()
		.lines()
		.filter(|l| !l.is_empty())
		.map(str::to_owned)
		.collect()
}

/// The DNS name of the server on its environment network, which the gateway reaches it by.
#[must_use]
pub fn alias(slug: &str) -> String {
	format!("mcp-{slug}")
}

/// `http://llm.podkit.internal:8088/mcp/<slug>`: what an application calls.
#[must_use]
pub fn gateway_url(slug: &str) -> String {
	format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/mcp/{slug}")
}

/// `MCP_<NAME>`: the slug as an environment variable name.
#[must_use]
pub fn env_name(slug: &str) -> String {
	format!(
		"MCP_{}",
		slug.chars()
			.map(|c| if c == '-' {
				'_'
			} else {
				c.to_ascii_uppercase()
			})
			.collect::<String>()
	)
}

pub async fn find(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<McpServer>, sqlx::Error> {
	sqlx::query_as(
		"SELECT application_id, transport, path, command, created_at FROM mcp_servers
		WHERE application_id = $1",
	)
	.bind(app)
	.fetch_optional(db)
	.await
}

pub async fn insert(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	transport: Transport,
	path: &str,
	command: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO mcp_servers (application_id, transport, path, command) VALUES ($1, $2, $3, $4)",
	)
	.bind(app)
	.bind(transport)
	.bind(path)
	.bind(command)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn update(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	path: &str,
	command: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE mcp_servers SET path = $2, command = $3 WHERE application_id = $1")
		.bind(app)
		.bind(path)
		.bind(command)
		.execute(db)
		.await?;
	Ok(())
}

/// An MCP server of a project, for lists.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Listed {
	pub application_id: ApplicationId,
	pub name: String,
	pub slug: String,
	pub transport: Transport,
	pub server_id: ServerId,
	pub links: i64,
}

/// The MCP servers of `project`, by name.
pub async fn list_in_project(
	db: impl PgExecutor<'_>,
	project: ProjectId,
) -> Result<Vec<Listed>, sqlx::Error> {
	sqlx::query_as(
		"SELECT a.id AS application_id, a.name, a.slug, m.transport, a.server_id,
			(SELECT COUNT(*) FROM application_mcp_links l WHERE l.mcp_application_id = a.id) AS links
		FROM mcp_servers m JOIN applications a ON a.id = m.application_id
		WHERE a.project_id = $1 ORDER BY a.name, a.id",
	)
	.bind(project)
	.fetch_all(db)
	.await
}

/// An MCP server an application is linked to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Link {
	pub mcp_application_id: ApplicationId,
	pub project_id: ProjectId,
	pub name: String,
	pub slug: String,
	pub transport: Transport,
}

/// The servers `app` is linked to, by name.
pub async fn links_of_app(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<Link>, sqlx::Error> {
	sqlx::query_as(
		"SELECT l.mcp_application_id, a.project_id, a.name, a.slug, m.transport
		FROM application_mcp_links l
		JOIN applications a ON a.id = l.mcp_application_id
		JOIN mcp_servers m ON m.application_id = l.mcp_application_id
		WHERE l.application_id = $1 ORDER BY a.name, a.id",
	)
	.bind(app)
	.fetch_all(db)
	.await
}

/// An application linked to an MCP server.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LinkedApp {
	pub application_id: ApplicationId,
	pub project_id: ProjectId,
	pub name: String,
}

/// The applications linked to `server`, by name.
pub async fn apps_of_server(
	db: impl PgExecutor<'_>,
	server: ApplicationId,
) -> Result<Vec<LinkedApp>, sqlx::Error> {
	sqlx::query_as(
		"SELECT a.id AS application_id, a.project_id, a.name
		FROM application_mcp_links l JOIN applications a ON a.id = l.application_id
		WHERE l.mcp_application_id = $1 ORDER BY a.name, a.id",
	)
	.bind(server)
	.fetch_all(db)
	.await
}

/// How many applications are linked to `server`.
pub async fn count_links(
	db: impl PgExecutor<'_>,
	server: ApplicationId,
) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar("SELECT COUNT(*) FROM application_mcp_links WHERE mcp_application_id = $1")
		.bind(server)
		.fetch_one(db)
		.await
}

/// MCP servers of the team on `server` that `app` is not linked to yet.
pub async fn linkable(
	db: impl PgExecutor<'_>,
	team: crate::ids::TeamId,
	server: ServerId,
	app: ApplicationId,
) -> Result<Vec<(ApplicationId, String)>, sqlx::Error> {
	sqlx::query_as(
		"SELECT a.id, a.name FROM mcp_servers m
		JOIN applications a ON a.id = m.application_id
		JOIN projects p ON p.id = a.project_id
		WHERE p.team_id = $1 AND a.server_id = $2 AND a.id <> $3
			AND NOT EXISTS (SELECT 1 FROM application_mcp_links l
				WHERE l.application_id = $3 AND l.mcp_application_id = a.id)
		ORDER BY a.name, a.id",
	)
	.bind(team)
	.bind(server)
	.bind(app)
	.fetch_all(db)
	.await
}

fn seal(state: &AppState, key: &str) -> AppResult<Vec<u8>> {
	state.secrets.encrypt(key).map_err(|e| {
		tracing::error!("failed to encrypt an MCP key: {e}");
		ServerError::Internal
	})
}

fn unseal(state: &AppState, sealed: &[u8]) -> AppResult<String> {
	state.secrets.decrypt(sealed).map_err(|e| {
		tracing::error!("MCP key decrypt failed: {e}");
		ServerError::Internal
	})
}

/// Links `app` to `server` under a new key of its own. Linking again changes
/// nothing. `true` when the link is new.
pub async fn add_link(
	state: &AppState,
	app: ApplicationId,
	server: ApplicationId,
) -> AppResult<bool> {
	let key = format!("pk-mcp-{}", crypto::generate_token(32));
	let sealed = seal(state, &key)?;
	let inserted = sqlx::query(
		"INSERT INTO application_mcp_links (application_id, mcp_application_id, key_hash, key)
		VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
	)
	.bind(app)
	.bind(server)
	.bind(hash_key(&key))
	.bind(sealed)
	.execute(&state.db)
	.await?
	.rows_affected();
	Ok(inserted == 1)
}

/// Removes the link, and with it its key. `false` when there was none.
pub async fn remove_link(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	server: ApplicationId,
) -> Result<bool, sqlx::Error> {
	Ok(sqlx::query(
		"DELETE FROM application_mcp_links WHERE application_id = $1 AND mcp_application_id = $2",
	)
	.bind(app)
	.bind(server)
	.execute(db)
	.await?
	.rows_affected()
		== 1)
}

#[derive(sqlx::FromRow)]
struct KeyedLink {
	slug: String,
	key: Vec<u8>,
}

/// The variables a deploy of `app` gets for its MCP links: nothing when it has none.
///
/// `PODKIT_MCP_SERVERS` is a JSON list of `{name, url, key}`; each server also gets
/// `MCP_<NAME>_URL` and `MCP_<NAME>_KEY`.
///
/// # Errors
/// The database or a stored key is unreadable.
pub async fn env(state: &AppState, app: ApplicationId) -> AppResult<Vec<(String, String)>> {
	let rows: Vec<KeyedLink> = sqlx::query_as(
		"SELECT a.slug, l.key FROM application_mcp_links l
		JOIN applications a ON a.id = l.mcp_application_id
		WHERE l.application_id = $1 ORDER BY a.slug",
	)
	.bind(app)
	.fetch_all(&state.db)
	.await?;
	if rows.is_empty() {
		return Ok(Vec::new());
	}
	let mut list = Vec::with_capacity(rows.len());
	let mut vars = Vec::with_capacity(rows.len() * 2 + 1);
	for row in rows {
		let key = unseal(state, &row.key)?;
		let url = gateway_url(&row.slug);
		let prefix = env_name(&row.slug);
		list.push(serde_json::json!({ "name": row.slug, "url": url, "key": key }));
		vars.push((format!("{prefix}_URL"), url));
		vars.push((format!("{prefix}_KEY"), key));
	}
	let mut out = vec![(
		"PODKIT_MCP_SERVERS".to_owned(),
		serde_json::Value::Array(list).to_string(),
	)];
	out.append(&mut vars);
	Ok(out)
}

#[derive(sqlx::FromRow)]
struct KeyedServer {
	key_hash: String,
	server_id: ApplicationId,
	slug: String,
	container_port: i32,
	path: String,
}

impl KeyedServer {
	fn into_key(self) -> GatewayMcpKey {
		GatewayMcpKey {
			key_hash: self.key_hash,
			mcp: GatewayMcp {
				server_id: self.server_id.0,
				host: alias(&self.slug),
				name: self.slug,
				port: u16::try_from(self.container_port).unwrap_or(0),
				path: self.path,
			},
		}
	}
}

/// What the gateway of `server` needs for MCP: the keys of the links of applications on
/// it, the UI's own keys, the names of every MCP server on it, and the environments whose
/// networks it must be on.
pub struct GatewayParts {
	pub keys: Vec<GatewayMcpKey>,
	pub names: Vec<String>,
	pub environments: Vec<i64>,
}

/// # Errors
/// The database fails.
pub async fn gateway_parts(db: &PgPool, server: ServerId) -> Result<GatewayParts, sqlx::Error> {
	let rows: Vec<KeyedServer> = sqlx::query_as(
		"SELECT k.key_hash, m.application_id AS server_id, ms.slug, ms.container_port, m.path
		FROM application_mcp_links k
		JOIN applications a ON a.id = k.application_id
		JOIN mcp_servers m ON m.application_id = k.mcp_application_id
		JOIN applications ms ON ms.id = m.application_id
		WHERE a.server_id = $1 AND ms.server_id = $1
		UNION ALL
		SELECT m.probe_key_hash, m.application_id, ms.slug, ms.container_port, m.path
		FROM mcp_servers m JOIN applications ms ON ms.id = m.application_id
		WHERE ms.server_id = $1 AND m.probe_key_hash IS NOT NULL
		ORDER BY 1",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	let names = sqlx::query_scalar(
		"SELECT ms.slug FROM mcp_servers m JOIN applications ms ON ms.id = m.application_id
		WHERE ms.server_id = $1 ORDER BY ms.slug",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	let environments = sqlx::query_scalar(
		"SELECT a.environment_id FROM application_mcp_links k
			JOIN applications a ON a.id = k.application_id
			JOIN applications ms ON ms.id = k.mcp_application_id
			WHERE a.server_id = $1 AND ms.server_id = $1
		UNION
		SELECT ms.environment_id FROM mcp_servers m JOIN applications ms ON ms.id = m.application_id
			WHERE ms.server_id = $1 AND (m.probe_key_hash IS NOT NULL OR EXISTS (
				SELECT 1 FROM application_mcp_links k WHERE k.mcp_application_id = m.application_id))
		ORDER BY 1",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	Ok(GatewayParts {
		keys: rows.into_iter().map(KeyedServer::into_key).collect(),
		names,
		environments,
	})
}

/// Whether `server` has any MCP key at all: when it has none, the gateway needs no sync.
pub async fn has_keys(db: impl PgExecutor<'_>, server: ServerId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM application_mcp_links k
			JOIN applications a ON a.id = k.application_id WHERE a.server_id = $1)
		OR EXISTS (SELECT 1 FROM mcp_servers m JOIN applications ms ON ms.id = m.application_id
			WHERE ms.server_id = $1 AND m.probe_key_hash IS NOT NULL)",
	)
	.bind(server)
	.fetch_one(db)
	.await
}

/// Adds what the gateway counted for one MCP key and hour to `mcp_usage`.
pub async fn record_usage(
	db: impl PgExecutor<'_>,
	row: &proto::GatewayUsage,
) -> Result<(), sqlx::Error> {
	let Ok(requests) = i64::try_from(row.requests) else {
		return Ok(());
	};
	sqlx::query(
		"INSERT INTO mcp_usage
			(key_hash, mcp_application_id, hour, application_id, team_id, requests)
		SELECT $1, $2, TIMESTAMPTZ 'epoch' + $3::bigint * INTERVAL '1 second', l.application_id,
			COALESCE(p.team_id, pp.team_id), $4
		FROM (SELECT 1) AS one
		LEFT JOIN application_mcp_links l ON l.key_hash = $1
		LEFT JOIN applications a ON a.id = l.application_id
		LEFT JOIN projects p ON p.id = a.project_id
		LEFT JOIN mcp_servers pm ON pm.probe_key_hash = $1
		LEFT JOIN applications pa ON pa.id = pm.application_id
		LEFT JOIN projects pp ON pp.id = pa.project_id
		ON CONFLICT (key_hash, mcp_application_id, hour) DO UPDATE SET
			requests = mcp_usage.requests + EXCLUDED.requests,
			application_id = COALESCE(mcp_usage.application_id, EXCLUDED.application_id),
			team_id = COALESCE(mcp_usage.team_id, EXCLUDED.team_id)",
	)
	.bind(&row.key_hash)
	.bind(row.model_id)
	.bind(row.hour)
	.bind(requests)
	.execute(db)
	.await?;
	Ok(())
}

/// What one key used of a server.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct KeyUsage {
	/// The application the key belongs to, `Console` (the UI's own key) or `Removed link`.
	pub key_label: String,
	pub requests_24h: i64,
	pub requests_7d: i64,
}

/// Usage of `server` over the last 7 days, per key, busiest first. Counters are kept per
/// hour, so "24 h" means the last 24 hourly buckets.
pub async fn usage_for_server(
	db: impl PgExecutor<'_>,
	server: ApplicationId,
) -> Result<Vec<KeyUsage>, sqlx::Error> {
	sqlx::query_as(
		"SELECT COALESCE(a.name, CASE WHEN m.probe_key_hash = mu.key_hash THEN 'Console'
				ELSE 'Removed link' END) AS key_label,
			COALESCE(SUM(mu.requests) FILTER (WHERE mu.hour >= NOW() - INTERVAL '24 hours'), 0)::BIGINT
				AS requests_24h,
			COALESCE(SUM(mu.requests), 0)::BIGINT AS requests_7d
		FROM mcp_usage mu
		LEFT JOIN applications a ON a.id = mu.application_id
		LEFT JOIN mcp_servers m ON m.application_id = mu.mcp_application_id
		WHERE mu.mcp_application_id = $1 AND mu.hour >= NOW() - INTERVAL '7 days'
		GROUP BY mu.key_hash, a.name, m.probe_key_hash
		ORDER BY requests_7d DESC, key_label",
	)
	.bind(server)
	.fetch_all(db)
	.await
}
