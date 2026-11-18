//! The template gallery: browse the catalog of one-click services, read one, and deploy it
//! into a project. A template becomes an image application or a compose stack; this
//! page does not care which (`templates::service::deploy` decides) and sends the browser to
//! whatever was created.
//!
//! Two entrances share every handler: `/teams/{t}/templates` (the dialog asks which
//! project) and `/teams/{t}/projects/{p}/templates` (from the create-resource picker, the
//! project and environment are already chosen).

use axum::Router;
use axum::routing::{get, post};

use crate::ids::{ProjectId, TeamId};
use crate::state::AppState;

mod deploy;
mod detail;
mod gallery;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/templates", get(gallery::page))
		.route("/teams/{team_id}/templates/results", get(gallery::results))
		.route(
			"/teams/{team_id}/templates/{template_id}",
			get(detail::page),
		)
		.route(
			"/teams/{team_id}/templates/{template_id}/environments",
			get(deploy::environments),
		)
		.route(
			"/teams/{team_id}/templates/{template_id}/deploy",
			post(deploy::deploy),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/templates",
			get(gallery::project_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/templates/results",
			get(gallery::project_results),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/templates/{template_id}",
			get(detail::project_page),
		)
}

/// Where a gallery page is: the team's, or one project's (with the environment the picker
/// passed, if any).
#[derive(Debug, Clone, Copy)]
pub(super) struct Place {
	pub team: TeamId,
	pub project: Option<ProjectId>,
	pub environment: Option<i64>,
}

impl Place {
	/// `/teams/{t}/templates` or `/teams/{t}/projects/{p}/templates`.
	pub fn base(self) -> String {
		match self.project {
			Some(p) => format!("/teams/{}/projects/{p}/templates", self.team),
			None => format!("/teams/{}/templates", self.team),
		}
	}

	/// `?environment=<id>` or nothing; append more with [`Place::query`].
	pub fn env_param(self) -> Option<String> {
		self.environment.map(|e| format!("environment={e}"))
	}
}
