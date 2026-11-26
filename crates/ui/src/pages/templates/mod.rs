//! The template gallery: browse the catalog of one-click services, read one, and deploy it
//! into a project. A template becomes an image application or a compose stack; this
//! page does not care which (`templates::service::deploy` decides) and sends the browser to
//! whatever was created.
//!
//! Two entrances share every handler: `/teams/{t}/templates` (the dialog asks which
//! project) and `/teams/{t}/projects/{p}/templates` (from the create-resource picker, the
//! project and environment are already chosen).

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maud::html;

use crate::components::{BannerKind, Size, Variant, banner, button_class};
use crate::extract::UiUser;
use crate::{error, layout};

use podkit_core::ids::{ProjectId, TeamId};
use podkit_core::state::AppState;

mod deploy;
mod detail;
mod gallery;
#[cfg(test)]
mod tests;

#[derive(Default, serde::Deserialize)]
struct MarketQuery {
	/// Set on the second visit, the one made from this origin.
	#[serde(default, deserialize_with = "one")]
	here: bool,
}

fn one<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
	Ok(<String as serde::Deserialize>::deserialize(d)? == "1")
}

/// `GET /market/{id}`: where the "Deploy to my podkit" button of the catalog's website
/// lands. It only finds the template's page here, in the viewer's first team; that page is
/// the confirmation, and deploying from it is a `POST` the viewer makes.
///
/// The address comes from another site, so nothing in it is trusted: the id is looked up in
/// this podkit's own catalog and the link is built from the catalog's copy of it.
///
/// The session cookie is `SameSite=Strict`, so a browser does not send it on the visit that
/// starts at the website. A visit with no session is therefore answered once with a page
/// that asks for the same address again, from here: that second visit carries the cookie.
async fn market(
	State(s): State<AppState>,
	user: Result<UiUser, Response>,
	Path(id): Path<String>,
	Query(q): Query<MarketQuery>,
) -> Response {
	let user = match user {
		Ok(user) => user,
		// Only an address made of an id is written into the page.
		Err(_) if !q.here && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => {
			return html! {
				(maud::DOCTYPE)
				meta http-equiv="refresh" content={ "0;url=/market/" (id) "?here=1" };
				title { "podkit" }
				a href={ "/market/" (id) "?here=1" } { "Continue to podkit" }
			}
			.into_response();
		}
		Err(to_login) => return to_login,
	};
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

/// What an application or a stack says when the catalog's template has changed since it
/// was deployed from it. It is the owner's copy: nothing changes by itself.
pub fn newer_banner(team: TeamId, t: &podkit_core::templates::Loaded) -> maud::Markup {
	let link = html! {
		a href={ "/teams/" (team) "/templates/" (t.template.id) } { "See the template" }
	};
	banner(
		BannerKind::Info,
		Some("The catalog's template has changed since this was deployed."),
		&format!(
			"{} in the catalog is at {}. This is your own copy: nothing changes unless you deploy the template again.",
			t.template.name, t.template.version
		),
		Some(&link),
	)
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
