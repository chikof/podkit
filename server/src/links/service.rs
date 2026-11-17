//! What the link routes do, free of HTTP: authorize, scope to the team,
//! validate, then store and tell the agent.

use sqlx::PgPool;

use super::Link;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ModelId, ProjectId, TeamId, UserId};
use crate::state::AppState;
use crate::{applications, links, models, scope};

fn not_linked() -> ServerError {
	ServerError::ModelNotFound
}

/// # Errors
/// `Forbidden` without `read application`, `NotFound` for another team's application.
pub async fn list(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<Link>> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	Ok(links::list(&s.db, app.id).await?)
}

/// Links a model to an application. The model must be of the same team and
/// run on the application's server, and its served name must not clash with
/// another linked model's.
///
/// # Errors
/// `Forbidden` without `update application` and `read model`; `NotFound` for
/// another team's application or model; `Validation` for a model on another
/// server; `Conflict` for a name already taken by another link.
pub async fn link(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	model: ModelId,
	make_default: bool,
) -> AppResult<Link> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	authorize(&s.db, user, team, Action::Read, Resource::Model).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let model = models::find_in_team(&s.db, team, model)
		.await?
		.ok_or(ServerError::ModelNotFound)?;
	if model.server_id != app.server_id {
		return Err(ServerError::Validation(
			"the model runs on a different server than the application; the gateway only \
			reaches models on the application's own server"
				.to_owned(),
		));
	}
	if matches!(model.status, models::ModelStatus::Removing) {
		return Err(ServerError::Conflict(
			"the model is being removed".to_owned(),
		));
	}
	let existing = links::list(&s.db, app.id).await?;
	if let Some(other) = existing
		.iter()
		.find(|l| l.model_id != model.id && l.served_name == model.served_name)
	{
		return Err(ServerError::Conflict(format!(
			"the application is already linked to {:?}, which is also served as {:?}",
			other.name, model.served_name
		)));
	}

	links::ensure_key(s, app.id).await?;
	links::add(&s.db, app.id, model.id, make_default).await?;
	links::push_after_change(s, app.server_id).await;
	one(&s.db, app.id, model.id).await
}

/// Makes a linked model the one `PODKIT_MODEL` names on the next deploy.
///
/// # Errors
/// As [`link`]; `NotFound` when the model is not linked.
pub async fn set_default(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	model: ModelId,
) -> AppResult<Link> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	if !links::set_default(&s.db, app.id, model).await? {
		return Err(not_linked());
	}
	one(&s.db, app.id, model).await
}

/// Removes a link. The variables go from the application's next deploy on;
/// a running container keeps what it was started with.
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound` when the application
/// or the link does not exist.
pub async fn unlink(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	model: ModelId,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	// Bring home what the gateway counted while the key still names the application.
	collect_best_effort(s, &app).await;
	if links::remove(&s.db, app.id, model).await?.is_none() {
		return Err(not_linked());
	}
	links::push_after_change(s, app.server_id).await;
	Ok(())
}

async fn collect_best_effort(s: &AppState, app: &applications::Application) {
	let result = async {
		let server = crate::servers::find(&s.db, app.server_id)
			.await?
			.ok_or(ServerError::ServerNotFound)?;
		let client = s.agents.client(s, &server).await?;
		links::collect_usage(s, &client).await
	}
	.await;
	if let Err(e) = result {
		tracing::warn!(application = app.id.0, error = %e, "could not collect gateway usage before unlinking");
	}
}

async fn one(db: &PgPool, app: ApplicationId, model: ModelId) -> AppResult<Link> {
	links::list(db, app)
		.await?
		.into_iter()
		.find(|l| l.model_id == model)
		.ok_or_else(not_linked)
}
