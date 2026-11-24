//! Audit events about a person rather than a team (sign-ins, second factors): the log is
//! kept per team, so the event goes to every team the person belongs to. A person in no
//! team leaves none; they have no audit log to read it in.

use sqlx::PgPool;

use crate::audit::{self, Diff, Target};
use crate::ids::UserId;

/// Records `action` in the audit log of every team `user` is in, as the caller of the
/// current request.
pub async fn record_everywhere(
	db: &PgPool,
	user: UserId,
	action: &str,
	target: Target,
	diff: Diff,
) {
	let teams = match crate::teams::list_for_user(db, user).await {
		Ok(teams) => teams,
		Err(e) => {
			tracing::error!(action, "cannot list teams for an audit event: {e}");
			return;
		}
	};
	for team in teams {
		audit::log(db, user, team.id, action, target.clone(), diff.clone()).await;
	}
}
