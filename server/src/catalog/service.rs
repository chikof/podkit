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
