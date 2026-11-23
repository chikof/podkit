//! Keeps the agents' log settings in step with the team's and watches forwarding.
//!
//! [`tick`] runs on the metrics sampler's turn for every active server: it hands the agent the
//! team's retention and forwarding when they differ from what it was last given (so a changed
//! setting, a new server and a restarted control plane all end up the same), and tells the team
//! once when the agent starts dropping forwarded lines. Nothing here runs a loop of its own.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock, PoisonError};

use proto::{LogConfig, LogExport};

use super::{ExportKind, service};
use crate::agents::AgentClient;
use crate::ids::{ServerId, TeamId};
use crate::notifications::{self, Event, EventKind};
use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;

/// What the control plane remembers about each agent between ticks.
#[derive(Default)]
struct Memory {
	/// A hash of the last settings the agent accepted.
	given: HashMap<ServerId, u64>,
	/// The agent's count of dropped lines at the last tick.
	dropped: HashMap<ServerId, u64>,
}

fn memory() -> std::sync::MutexGuard<'static, Memory> {
	static MEMORY: OnceLock<Mutex<Memory>> = OnceLock::new();
	MEMORY
		.get_or_init(Mutex::default)
		.lock()
		.unwrap_or_else(PoisonError::into_inner)
}

/// What the agent of `server` is told: the team's settings with the team and server names as
/// the labels every forwarded line carries.
async fn config_for(s: &AppState, server: &Server) -> Option<LogConfig> {
	let team = server.team_id;
	let settings = service::stored(&s.db, team)
		.await
		.map_err(|e| tracing::warn!(%team, error = %e, "log settings could not be read"))
		.ok()?;
	let export = match settings.export {
		None => None,
		Some(export) => {
			let authorization = match service::sealed_authorization(&s.db, team).await {
				Ok(Some(sealed)) => match s.secrets.decrypt(&sealed) {
					Ok(value) => Some(value),
					Err(e) => {
						tracing::error!(%team, "log export header decrypt failed: {e}");
						return None;
					}
				},
				Ok(None) => None,
				Err(e) => {
					tracing::warn!(%team, error = %e, "log export header could not be read");
					return None;
				}
			};
			let slug = sqlx::query_scalar::<_, String>("SELECT slug FROM teams WHERE id = $1")
				.bind(team)
				.fetch_optional(&s.db)
				.await
				.ok()
				.flatten()
				.unwrap_or_default();
			Some(LogExport {
				kind: match export.kind {
					ExportKind::Loki => proto::LogExportKind::Loki,
					ExportKind::Otlp => proto::LogExportKind::Otlp,
				},
				url: export.url,
				authorization,
				tenant: export.tenant,
				labels: vec![
					("team".to_owned(), slug),
					("server".to_owned(), server.name.clone()),
				],
			})
		}
	};
	Some(LogConfig {
		retention_days: u32::try_from(settings.retention_days)
			.unwrap_or(super::DEFAULT_RETENTION_DAYS as u32),
		max_bytes: u64::try_from(settings.max_mb).unwrap_or(0) << 20,
		export,
	})
}

fn fingerprint(config: &LogConfig) -> u64 {
	let mut hasher = DefaultHasher::new();
	serde_json::to_string(config)
		.unwrap_or_default()
		.hash(&mut hasher);
	hasher.finish()
}

/// Gives the agent its settings if it has not got these yet. Returns whether it has them now.
async fn give(s: &AppState, server: &Server, agent: &AgentClient) -> bool {
	let Some(config) = config_for(s, server).await else {
		return false;
	};
	let print = fingerprint(&config);
	if memory().given.get(&server.id) == Some(&print) {
		return true;
	}
	match agent.log_config(&config).await {
		Ok(()) => {
			memory().given.insert(server.id, print);
			true
		}
		Err(e) => {
			// An agent from before the log store has no such route: nothing to be done but wait
			// for the upgrade.
			tracing::debug!(server = %server.id, error = %e, "the agent did not take the log settings");
			false
		}
	}
}

/// Hands every server of the team its settings now, instead of at the next tick. Never fails
/// the caller: a server that does not answer gets them on a later tick.
pub async fn push_team(s: &AppState, team: TeamId) {
	let servers = match servers::list_by_team(&s.db, team).await {
		Ok(servers) => servers,
		Err(e) => {
			tracing::warn!(%team, error = %e, "log settings could not be pushed");
			return;
		}
	};
	let pushes = servers
		.into_iter()
		.filter(|server| server.status == ServerStatus::Active || server.is_local)
		.map(|server| async move {
			if let Ok(agent) = s.agents.client(s, &server).await {
				give(s, &server, &agent).await;
			}
		});
	futures_util::future::join_all(pushes).await;
}

/// The sampler's turn for one server: settings, then the forwarding check.
pub async fn tick(s: &AppState, server: &Server, agent: &AgentClient) {
	if !give(s, server, agent).await {
		return;
	}
	let Ok(status) = agent.log_status().await else {
		return;
	};
	let before = memory()
		.dropped
		.insert(server.id, status.export_dropped)
		.unwrap_or(status.export_dropped);
	if status.export_dropped > before {
		let lost = status.export_dropped - before;
		let title = format!("Log forwarding on {} is dropping lines", server.name);
		let event = Event::new(
			server.team_id,
			EventKind::LogExportFailing,
			format!("server:{}", server.id),
			title,
		)
		.body(format!(
			"{lost} line{} could not be sent to the log backend since the last check. They are still stored and searchable in podkit.",
			if lost == 1 { "" } else { "s" }
		))
		.link(format!("/teams/{}/logs", server.team_id));
		notifications::emit(s, event).await;
	}
}
