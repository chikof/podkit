//! What a pull request's preview may read of the application's shared variables. The
//! application's own variables are decided in [`super::plan`] when the preview is made;
//! shared ones are inherited at every deploy, so they are filtered where they are resolved.

use sqlx::PgPool;

use crate::ids::EnvironmentId;
use crate::shared_env::SharedVar;

/// `rows` as a preview may see them: every variable that is not a secret, and a secret only
/// if it is marked for previews and the pull request is the team's own, or a fork's whose
/// variables the application allowed.
#[must_use]
pub fn restrict(rows: Vec<SharedVar>, from_fork: bool, fork_variables: bool) -> Vec<SharedVar> {
	let may = !from_fork || fork_variables;
	rows.into_iter()
		.filter(|r| !r.is_secret || (may && r.preview))
		.collect()
}

/// `rows` for a resource of `environment`: unchanged unless the environment is a preview's.
///
/// # Errors
/// The database fails.
pub async fn restrict_shared(
	db: &PgPool,
	environment: EnvironmentId,
	rows: Vec<SharedVar>,
) -> Result<Vec<SharedVar>, sqlx::Error> {
	let preview: Option<(bool, bool)> = sqlx::query_as(
		"SELECT p.from_fork, COALESCE(s.fork_variables, FALSE)
		FROM previews p
		LEFT JOIN preview_settings s ON s.application_id = p.application_id
		WHERE p.environment_id = $1",
	)
	.bind(environment)
	.fetch_optional(db)
	.await?;
	Ok(match preview {
		None => rows,
		Some((from_fork, fork_variables)) => restrict(rows, from_fork, fork_variables),
	})
}
