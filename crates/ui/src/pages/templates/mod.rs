//! The template gallery: browse the catalog of one-click services, read one, and deploy it
//! into a project. A template becomes an image application or a compose stack; this
//! page does not care which (`templates::service::deploy` decides) and sends the browser to
//! whatever was created.
//!
//! Two entrances share every handler: `/teams/{t}/templates` (the dialog asks which
//! project) and `/teams/{t}/projects/{p}/templates` (from the create-resource picker, the
//! project and environment are already chosen).

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maud::html;

use crate::components::{Size, Variant, button_class};
use crate::extract::UiUser;
use crate::{error, layout};

use podkit_core::ids::{ProjectId, TeamId};
use podkit_core::state::AppState;

mod deploy;
mod detail;
mod gallery;
#[cfg(test)]
mod tests;

/// `GET /market/{id}`: where the "Deploy to my podkit" button of the catalog's website
/// lands. It only finds the template's page here, in the viewer's first team; that page is
/// the confirmation, and deploying from it is a `POST` the viewer makes.
///
/// The address comes from another site, so nothing in it is trusted: the id is looked up in
/// this podkit's own catalog and the link is built from the catalog's copy of it.
async fn market(State(s): State<AppState>, user: UiUser, Path(id): Path<String>) -> Response {
	let teams = match podkit_core::teams::list_for_user(&s.db, user.id).await {
		Ok(teams) => teams,
		Err(e) => return error::bare(Some(&user), &e.into()),
	};
	let Some(team) = teams.first() else {
		return Redirect::to("/").into_response();
	};
	match podkit_core::templates::find(&id) {
		Some(l) => Redirect::to(&format!("/teams/{}/templates/{}", team.id, l.template.id))
			.into_response(),
		None => layout::bare_page(
			Some(&user.name),
			"Template not found",
			&html! {
				h1 { "This podkit does not have that template" }
				p class="ui-lede" {
					"Its catalog may be older than the website's. The administrator can refresh it from the gallery."
				}
				a class=(button_class(Variant::Default, Size::Md)) href={ "/teams/" (team.id) "/templates" } {
					"Open the gallery"
				}
			},
		)
		.into_response(),
	}
}

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/market/{template_id}", get(market))
		.route("/teams/{team_id}/templates", get(gallery::page))
		.route("/teams/{team_id}/templates/results", get(gallery::results))
		.route("/teams/{team_id}/templates/refresh", post(gallery::refresh))
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
