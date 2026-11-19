//! Links between applications and models, and the gateway they go through.
//!
//! An application linked to at least one model has a gateway key. Its deploys
//! get `OPENAI_BASE_URL`, `OPENAI_API_KEY`, `ANTHROPIC_BASE_URL`,
//! `ANTHROPIC_API_KEY` (the same key) and `PODKIT_MODEL`; the URL is the same
//! whichever model backs it. The server keeps the key's hash and the models it
//! may use, and pushes both to the server's agent, whose gateway sidecar
//! enforces them and counts tokens (`docs/gateway.md`). The counters come back
//! into `model_usage`.
//!
//! Routes live under `/api/v1/teams/{team}/projects/{project}/applications/{app}/models`.

pub mod playground;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use axum::http::Method;
use proto::{GATEWAY_HOST, GATEWAY_PORT, GatewayConfig, GatewayKey, GatewayModel, GatewayUsage};
use sha2::{Digest, Sha256};
use sqlx::{PgExecutor, PgPool};

use crate::agents::AgentClient;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ModelId, ServerId};
use crate::models::{EngineKind, ModelStatus};
use crate::servers;
use crate::state::AppState;

pub use routes::router;

/// Hex SHA-256 of a gateway key. The agent computes the same on each request.
#[must_use]
pub fn hash_key(key: &str) -> String {
	use std::fmt::Write;
	Sha256::digest(key.as_bytes())
		.iter()
		.fold(String::with_capacity(64), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		})
}

/// `http://llm.podkit.internal:8088`, which every container on a server's `podkit` network reaches.
#[must_use]
pub fn gateway_url() -> String {
	format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}")
}

/// A model an application is linked to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Link {
	pub model_id: ModelId,
	pub name: String,
	pub slug: String,
	pub served_name: String,
	pub status: ModelStatus,
	pub is_default: bool,
}

const LINK_COLUMNS: &str = "am.model_id, m.name, m.slug, m.served_name, m.status, am.is_default";

pub async fn list(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<Vec<Link>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {LINK_COLUMNS} FROM application_models am JOIN models m ON m.id = am.model_id
		WHERE am.application_id = $1 ORDER BY am.model_id"
	)))
	.bind(app)
	.fetch_all(db)
	.await
}

/// Links `model` to `app`. The first link of an application becomes its
/// default; so does a link made with `make_default`. Linking again changes nothing
/// except that it may make the link the default.
pub async fn add(
	db: &PgPool,
	app: ApplicationId,
	model: ModelId,
	make_default: bool,
) -> Result<(), sqlx::Error> {
	let mut tx = db.begin().await?;
	// Serializes concurrent changes to one application's links.
	sqlx::query("SELECT id FROM applications WHERE id = $1 FOR UPDATE")
		.bind(app)
		.fetch_optional(&mut *tx)
		.await?;
	sqlx::query(
		"INSERT INTO application_models (application_id, model_id) VALUES ($1, $2)
		ON CONFLICT DO NOTHING",
	)
	.bind(app)
	.bind(model)
	.execute(&mut *tx)
	.await?;
	let has_default: bool = sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM application_models WHERE application_id = $1 AND is_default)",
	)
	.bind(app)
	.fetch_one(&mut *tx)
	.await?;
	if make_default || !has_default {
		set_default_in(&mut tx, app, model).await?;
	}
	tx.commit().await
}

async fn set_default_in(
	tx: &mut sqlx::PgConnection,
	app: ApplicationId,
	model: ModelId,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE application_models SET is_default = FALSE WHERE application_id = $1 AND is_default",
	)
	.bind(app)
	.execute(&mut *tx)
	.await?;
	sqlx::query(
		"UPDATE application_models SET is_default = TRUE WHERE application_id = $1 AND model_id = $2",
	)
	.bind(app)
	.bind(model)
	.execute(&mut *tx)
	.await?;
	Ok(())
}

/// Makes `model` the default. `false` when it is not linked.
pub async fn set_default(
	db: &PgPool,
	app: ApplicationId,
	model: ModelId,
) -> Result<bool, sqlx::Error> {
	let mut tx = db.begin().await?;
	let linked: bool = sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM application_models WHERE application_id = $1 AND model_id = $2)",
	)
	.bind(app)
	.bind(model)
	.fetch_one(&mut *tx)
	.await?;
	if linked {
		set_default_in(&mut tx, app, model).await?;
	}
	tx.commit().await?;
	Ok(linked)
}

/// Unlinks `model`. When it was the default, the oldest remaining link takes
/// over, and with none left the application's gateway key goes too. `None`
/// when it was not linked, else whether the application has no links now.
pub async fn remove(
	db: &PgPool,
	app: ApplicationId,
	model: ModelId,
) -> Result<Option<bool>, sqlx::Error> {
	let mut tx = db.begin().await?;
	sqlx::query("SELECT id FROM applications WHERE id = $1 FOR UPDATE")
		.bind(app)
		.fetch_optional(&mut *tx)
		.await?;
	let removed =
		sqlx::query("DELETE FROM application_models WHERE application_id = $1 AND model_id = $2")
			.bind(app)
			.bind(model)
			.execute(&mut *tx)
			.await?
			.rows_affected();
	if removed == 0 {
		return Ok(None);
	}
	sqlx::query(
		"UPDATE application_models SET is_default = TRUE
		WHERE application_id = $1 AND model_id = (
			SELECT model_id FROM application_models WHERE application_id = $1
			ORDER BY created_at, model_id LIMIT 1)
		AND NOT EXISTS (
			SELECT 1 FROM application_models WHERE application_id = $1 AND is_default)",
	)
	.bind(app)
	.execute(&mut *tx)
	.await?;
	let left: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM application_models WHERE application_id = $1")
			.bind(app)
			.fetch_one(&mut *tx)
			.await?;
	if left == 0 {
		sqlx::query("DELETE FROM gateway_keys WHERE application_id = $1")
			.bind(app)
			.execute(&mut *tx)
			.await?;
	}
	tx.commit().await?;
	Ok(Some(left == 0))
}

/// How many applications are linked to `model`.
pub async fn count_for_model(db: impl PgExecutor<'_>, model: ModelId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar("SELECT COUNT(*) FROM application_models WHERE model_id = $1")
		.bind(model)
		.fetch_one(db)
		.await
}

/// The application's gateway key, issued when it has none. Returns its hash.
pub async fn ensure_key(state: &AppState, app: ApplicationId) -> AppResult<String> {
	if let Some(hash) = sqlx::query_scalar::<_, String>(
		"SELECT key_hash FROM gateway_keys WHERE application_id = $1",
	)
	.bind(app)
	.fetch_optional(&state.db)
	.await?
	{
		return Ok(hash);
	}
	let key = format!("pk-{}", crypto::generate_token(32));
	let sealed = state
		.secrets
		.encrypt(&key)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt the gateway key: {e}")))?;
	// A concurrent link may have issued one meanwhile; whichever got in first stays.
	sqlx::query(
		"INSERT INTO gateway_keys (application_id, key_hash, key) VALUES ($1, $2, $3)
		ON CONFLICT DO NOTHING",
	)
	.bind(app)
	.bind(hash_key(&key))
	.bind(sealed)
	.execute(&state.db)
	.await?;
	Ok(
		sqlx::query_scalar("SELECT key_hash FROM gateway_keys WHERE application_id = $1")
			.bind(app)
			.fetch_one(&state.db)
			.await?,
	)
}

/// The variables a deploy of `app` gets: nothing when it has no links.
///
/// # Errors
/// The database or the stored key is unreadable.
pub async fn env(state: &AppState, app: ApplicationId) -> AppResult<Vec<(String, String)>> {
	let Some(sealed) =
		sqlx::query_scalar::<_, Vec<u8>>("SELECT key FROM gateway_keys WHERE application_id = $1")
			.bind(app)
			.fetch_optional(&state.db)
			.await?
	else {
		return Ok(Vec::new());
	};
	let Some(default) = list(&state.db, app)
		.await?
		.into_iter()
		.find(|l| l.is_default)
	else {
		return Ok(Vec::new());
	};
	let key = state.secrets.decrypt(&sealed).map_err(|e| {
		tracing::error!(application = app.0, "gateway key decrypt failed: {e}");
		ServerError::Internal
	})?;
	let base = gateway_url();
	Ok(vec![
		("OPENAI_BASE_URL".to_owned(), format!("{base}/v1")),
		("OPENAI_API_KEY".to_owned(), key.clone()),
		// The Anthropic SDKs add `/v1/messages` themselves.
		("ANTHROPIC_BASE_URL".to_owned(), base),
		("ANTHROPIC_API_KEY".to_owned(), key),
		("PODKIT_MODEL".to_owned(), default.served_name),
	])
}

#[derive(sqlx::FromRow)]
struct KeyedModel {
	key_hash: String,
	id: ModelId,
	served_name: String,
	slug: String,
	port: i32,
	engine: EngineKind,
}

/// The playground keys of the models on `server`, with the model each opens.
///
/// # Errors
/// The database fails.
async fn playground_rows(db: &PgPool, server: ServerId) -> Result<Vec<KeyedModel>, sqlx::Error> {
	sqlx::query_as(
		"SELECT k.key_hash, m.id, m.served_name, m.slug, m.port, m.engine
		FROM playground_keys k JOIN models m ON m.id = k.model_id
		WHERE m.server_id = $1
		ORDER BY k.key_hash",
	)
	.bind(server)
	.fetch_all(db)
	.await
}

/// What the agent of `server` should enforce: every key of an application on
/// the server, with the models of its links that run there, and the
/// playground keys of its models.
///
/// # Errors
/// The database fails.
pub async fn build_config(db: &PgPool, server: ServerId) -> Result<GatewayConfig, sqlx::Error> {
	let rows: Vec<KeyedModel> = sqlx::query_as(
		"SELECT k.key_hash, m.id, m.served_name, m.slug, m.port, m.engine
		FROM gateway_keys k
		JOIN applications a ON a.id = k.application_id
		JOIN application_models am ON am.application_id = a.id
		JOIN models m ON m.id = am.model_id
		WHERE a.server_id = $1 AND m.server_id = $1
		ORDER BY k.key_hash, m.id",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	let mut keys: BTreeMap<String, Vec<GatewayModel>> = BTreeMap::new();
	for row in rows {
		keys.entry(row.key_hash).or_default().push(GatewayModel {
			model_id: row.id.0,
			name: row.served_name,
			host: crate::models::alias(&row.slug),
			port: u16::try_from(row.port).unwrap_or(0),
			// Custom images are not known to take `stream_options`.
			stream_usage: row.engine != EngineKind::Custom,
		});
	}
	// Keys with no model on this server still have to be known, or they would be 401s.
	let empty: Vec<String> = sqlx::query_scalar(
		"SELECT k.key_hash FROM gateway_keys k JOIN applications a ON a.id = k.application_id
		WHERE a.server_id = $1",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	for hash in empty {
		keys.entry(hash).or_default();
	}
	let other_models =
		sqlx::query_scalar("SELECT DISTINCT served_name FROM models WHERE server_id = $1")
			.bind(server)
			.fetch_all(db)
			.await?;
	for row in playground_rows(db, server).await? {
		keys.entry(row.key_hash).or_default().push(GatewayModel {
			model_id: row.id.0,
			name: row.served_name,
			host: crate::models::alias(&row.slug),
			port: u16::try_from(row.port).unwrap_or(0),
			stream_usage: row.engine != EngineKind::Custom,
		});
	}
	// The gateway sits on the network of every app that uses a model and of
	// every model it serves, so `llm.podkit.internal` resolves for the apps
	// and the models answer the gateway.
	let environments: Vec<i64> = sqlx::query_scalar(
		"SELECT a.environment_id FROM gateway_keys k
			JOIN applications a ON a.id = k.application_id WHERE a.server_id = $1
		UNION
		SELECT m.environment_id FROM models m
			WHERE m.server_id = $1 AND (
				m.id IN (SELECT am.model_id FROM application_models am
					JOIN applications a ON a.id = am.application_id WHERE a.server_id = $1)
				OR m.id IN (SELECT model_id FROM playground_keys)
			)
		ORDER BY 1",
	)
	.bind(server)
	.fetch_all(db)
	.await?;
	Ok(GatewayConfig {
		keys: keys
			.into_iter()
			.map(|(key_hash, models)| GatewayKey { key_hash, models })
			.collect(),
		other_models,
		networks: environments
			.into_iter()
			.map(|e| crate::networks::environment_podman_name(crate::ids::EnvironmentId(e)))
			.collect(),
	})
}

/// Sends the server's routing table to its agent.
///
/// # Errors
/// The agent is unreachable or refuses the table.
pub async fn push(state: &AppState, client: &AgentClient, server: ServerId) -> AppResult<()> {
	let config = build_config(&state.db, server).await?;
	let _: serde_json::Value = client
		.json(Method::PUT, "/v1/gateway/config", Some(&config))
		.await?;
	Ok(())
}

/// Takes the counters the gateway of `server` collected and adds them to
/// `model_usage`.
///
/// # Errors
/// The agent is unreachable or the database fails. Counters taken from the
/// agent before a database failure are lost.
pub async fn collect_usage(state: &AppState, client: &AgentClient) -> AppResult<usize> {
	let rows: Vec<GatewayUsage> = client
		.json(Method::POST, "/v1/gateway/usage/drain", None::<&()>)
		.await?;
	for row in &rows {
		record_usage(&state.db, row).await?;
	}
	Ok(rows.len())
}

async fn record_usage(db: &PgPool, row: &GatewayUsage) -> Result<(), sqlx::Error> {
	let (Ok(requests), Ok(input), Ok(output)) = (
		i64::try_from(row.requests),
		i64::try_from(row.input_tokens),
		i64::try_from(row.output_tokens),
	) else {
		return Ok(());
	};
	sqlx::query(
		"INSERT INTO model_usage
			(key_hash, model_id, hour, application_id, team_id, requests, input_tokens, output_tokens)
		SELECT $1, $2, TIMESTAMPTZ 'epoch' + $3::bigint * INTERVAL '1 second', k.application_id,
			COALESCE(p.team_id, pp.team_id), $4, $5, $6
		FROM (SELECT 1) AS one
		LEFT JOIN gateway_keys k ON k.key_hash = $1
		LEFT JOIN applications a ON a.id = k.application_id
		LEFT JOIN projects p ON p.id = a.project_id
		LEFT JOIN playground_keys pk ON pk.key_hash = $1
		LEFT JOIN models pm ON pm.id = pk.model_id
		LEFT JOIN projects pp ON pp.id = pm.project_id
		ON CONFLICT (key_hash, model_id, hour) DO UPDATE SET
			requests = model_usage.requests + EXCLUDED.requests,
			input_tokens = model_usage.input_tokens + EXCLUDED.input_tokens,
			output_tokens = model_usage.output_tokens + EXCLUDED.output_tokens,
			application_id = COALESCE(model_usage.application_id, EXCLUDED.application_id),
			team_id = COALESCE(model_usage.team_id, EXCLUDED.team_id)",
	)
	.bind(&row.key_hash)
	.bind(row.model_id)
	.bind(row.hour)
	.bind(requests)
	.bind(input)
	.bind(output)
	.execute(db)
	.await?;
	Ok(())
}

/// Part of the periodic status sync: keeps the agent's table current and
/// brings the usage home. Servers without keys are left alone.
pub async fn sync(state: &AppState, server: ServerId, client: &AgentClient) {
	let keyed = sqlx::query_scalar::<_, bool>(
		"SELECT EXISTS (SELECT 1 FROM gateway_keys k JOIN applications a ON a.id = k.application_id
		WHERE a.server_id = $1)
		OR EXISTS (SELECT 1 FROM playground_keys k JOIN models m ON m.id = k.model_id
		WHERE m.server_id = $1)",
	)
	.bind(server)
	.fetch_one(&state.db)
	.await;
	match keyed {
		Ok(true) => {}
		Ok(false) => return,
		Err(e) => {
			tracing::warn!(%server, error = %e, "gateway sync could not read the keys");
			return;
		}
	}
	if let Err(e) = push(state, client, server).await {
		tracing::warn!(%server, error = %e, "gateway sync could not push the routing table");
	}
	if let Err(e) = collect_usage(state, client).await {
		tracing::warn!(%server, error = %e, "gateway sync could not collect usage");
	}
}

/// Pushes the table right after a change. A failure is logged and left to the
/// next sync, because the change itself is already stored.
pub async fn push_after_change(state: &AppState, server: ServerId) {
	let result = async {
		let server = servers::find(&state.db, server)
			.await?
			.ok_or(ServerError::ServerNotFound)?;
		let client = state.agents.client(state, &server).await?;
		push(state, &client, server.id).await
	}
	.await;
	if let Err(e) = result {
		tracing::warn!(%server, error = %e, "could not push the gateway routing table; the next sync will");
	}
}

/// An application a model is linked to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LinkedApp {
	pub application_id: ApplicationId,
	pub name: String,
	pub is_default: bool,
}

/// The applications linked to `model`, by name.
pub async fn apps_for_model(
	db: impl PgExecutor<'_>,
	model: ModelId,
) -> Result<Vec<LinkedApp>, sqlx::Error> {
	sqlx::query_as(
		"SELECT a.id AS application_id, a.name, am.is_default
		FROM application_models am JOIN applications a ON a.id = am.application_id
		WHERE am.model_id = $1 ORDER BY a.name, a.id",
	)
	.bind(model)
	.fetch_all(db)
	.await
}

/// Applications of `project` on `server` that `model` is not linked to yet.
pub async fn linkable_apps(
	db: impl PgExecutor<'_>,
	project: crate::ids::ProjectId,
	server: ServerId,
	model: ModelId,
) -> Result<Vec<(ApplicationId, String)>, sqlx::Error> {
	sqlx::query_as(
		"SELECT a.id, a.name FROM applications a
		WHERE a.project_id = $1 AND a.server_id = $2
			AND NOT EXISTS (SELECT 1 FROM application_models am
				WHERE am.application_id = a.id AND am.model_id = $3)
		ORDER BY a.name, a.id",
	)
	.bind(project)
	.bind(server)
	.bind(model)
	.fetch_all(db)
	.await
}

/// What one key used of a model.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct KeyUsage {
	/// The application the key belongs to, `Playground`, or `Removed key`.
	pub key_label: String,
	pub requests_24h: i64,
	pub input_24h: i64,
	pub output_24h: i64,
	pub requests_7d: i64,
	pub input_7d: i64,
	pub output_7d: i64,
}

/// Usage of `model` over the last 7 days, per key, busiest first. Counters
/// are kept per hour, so "24 h" means the last 24 hourly buckets.
pub async fn usage_for_model(
	db: impl PgExecutor<'_>,
	model: ModelId,
) -> Result<Vec<KeyUsage>, sqlx::Error> {
	sqlx::query_as(
		"SELECT COALESCE(a.name, CASE WHEN pk.model_id IS NULL THEN 'Removed key' ELSE 'Playground' END)
				AS key_label,
			COALESCE(SUM(mu.requests) FILTER (WHERE mu.hour >= NOW() - INTERVAL '24 hours'), 0)::BIGINT
				AS requests_24h,
			COALESCE(SUM(mu.input_tokens) FILTER (WHERE mu.hour >= NOW() - INTERVAL '24 hours'), 0)::BIGINT
				AS input_24h,
			COALESCE(SUM(mu.output_tokens) FILTER (WHERE mu.hour >= NOW() - INTERVAL '24 hours'), 0)::BIGINT
				AS output_24h,
			COALESCE(SUM(mu.requests), 0)::BIGINT AS requests_7d,
			COALESCE(SUM(mu.input_tokens), 0)::BIGINT AS input_7d,
			COALESCE(SUM(mu.output_tokens), 0)::BIGINT AS output_7d
		FROM model_usage mu
		LEFT JOIN applications a ON a.id = mu.application_id
		LEFT JOIN playground_keys pk ON pk.key_hash = mu.key_hash
		WHERE mu.model_id = $1 AND mu.hour >= NOW() - INTERVAL '7 days'
		GROUP BY mu.key_hash, a.name, pk.model_id
		ORDER BY requests_7d DESC, key_label",
	)
	.bind(model)
	.fetch_all(db)
	.await
}
