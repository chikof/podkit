//! Rules of the catalog slice: which server's hardware a request may use.

use proto::Hardware;

use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::{members, scope};

/// The hardware snapshot of `server`, or `None` when no server was asked for.
///
/// The catalog is not scoped to a team, so the server's own team decides:
/// a server of a team the user is not in is as good as missing.
///
/// # Errors
/// `ServerNotFound` for an unknown server or one of another team, `Forbidden`
/// without `read server`, `Conflict` before the server has reported its hardware.
pub async fn hardware(
	s: &AppState,
	user: UserId,
	server: Option<ServerId>,
) -> AppResult<Option<Hardware>> {
	let Some(id) = server else {
		return Ok(None);
	};
	let team: Option<TeamId> = sqlx::query_scalar("SELECT team_id FROM servers WHERE id = $1")
		.bind(id)
		.fetch_optional(&s.db)
		.await?;
	let team = team.ok_or(ServerError::ServerNotFound)?;
	if members::find(&s.db, team, user).await?.is_none() {
		return Err(ServerError::ServerNotFound);
	}
	authorize(&s.db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(&s.db, team, id).await?;
	match server.hardware {
		Some(hardware) => Ok(Some(hardware.0)),
		None => Err(ServerError::Conflict(format!(
			"{} has not reported its hardware yet; try again shortly",
			server.name
		))),
	}
}

/// A server of the team, with what it last reported.
pub struct TeamServer {
	pub id: ServerId,
	pub name: String,
	pub hardware: Option<Hardware>,
}

/// The team's servers with their hardware, for rating recipes against each.
///
/// # Errors
/// `Forbidden` without `read server`.
pub async fn team_servers(s: &AppState, user: UserId, team: TeamId) -> AppResult<Vec<TeamServer>> {
	authorize(&s.db, user, team, Action::Read, Resource::Server).await?;
	let mut servers: Vec<_> = crate::servers::list_by_team(&s.db, team)
		.await?
		.into_iter()
		.map(|server| TeamServer {
			id: server.id,
			name: server.name,
			hardware: server.hardware.map(|h| h.0),
		})
		.collect();
	servers.sort_by_key(|server| server.id.0);
	Ok(servers)
}

/// Searches Hugging Face for GGUF repos, sized against `hardware`.
///
/// # Errors
/// `Forbidden` without `read model_file`; `Validation` for a query that is
/// empty, too long or has control characters; `HuggingFaceUnavailable`.
pub async fn hf_search(
	s: &AppState,
	user: UserId,
	team: TeamId,
	query: &str,
	hardware: Option<&Hardware>,
) -> AppResult<Vec<super::search::Hit>> {
	authorize(&s.db, user, team, Action::Read, Resource::ModelFile).await?;
	let query = query.trim();
	if query.is_empty() || query.chars().count() > 100 || query.chars().any(char::is_control) {
		return Err(ServerError::Validation(
			"Search for 1 to 100 characters.".to_owned(),
		));
	}
	super::search::search(s, query, super::search::DEFAULT_LIMIT, hardware).await
}
