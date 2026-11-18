//! Team pages: creating a team. A user with no team lands here from `/`.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use crate::state::AppState;
use crate::teams::service;
use crate::ui::components::{BUTTON, INPUT, LABEL, card, card_content, card_header};
use crate::ui::extract::UiUser;
use crate::ui::layout::{self, form_error};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/new", get(new_page).post(create))
}

/// The create-team form, as a page body.
pub fn create_form(intro: &str) -> Markup {
	html! {
		div data-signals="{name: '', slug: '', _error: ''}" {
			(card(&html! {
				(card_header("Create a team", Some(intro)))
				(card_content(&html! {
					form class="flex flex-col gap-4" "data-on:submit"="evt.preventDefault(); @post('/teams/new')" {
						label class=(LABEL) {
							"Name"
							input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
						}
						label class=(LABEL) {
							"Slug"
							input class=(INPUT) type="text" required maxlength="60" pattern="[a-z0-9]([a-z0-9\\-]*[a-z0-9])?" placeholder="my-team" "data-bind:slug";
						}
						(form_error())
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create team" }
					}
				}))
			}))
		}
	}
}

async fn new_page(user: UiUser) -> Markup {
	layout::bare_page(
		Some(&user.name),
		"New team",
		&create_form("A team holds your projects, servers and members."),
	)
}

#[derive(Deserialize)]
struct CreateSignals {
	name: String,
	slug: String,
}

async fn create(
	State(s): State<AppState>,
	user: UiUser,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	match service::create(&s.db, user.id, &sig.name, &sig.slug, "").await {
		Ok(team) => sse::redirect(&format!("/teams/{}/projects", team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use crate::tests::{PgPool, db_test};
	use axum::http::StatusCode;
	use serde_json::json;

	use crate::tests::app;
	use crate::ui::testing::{assert_contains, get_html, post_signals, ui_session};

	#[db_test]
	async fn creating_a_team_redirects_to_its_projects(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;

		let (s, _, body) = get_html(&app, "/teams/new", Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Create a team");

		let (s, _, body) = post_signals(
			&app,
			"/teams/new",
			Some(&cookie),
			&json!({ "name": "Second", "slug": "second" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "window.location.assign('/teams/");
		assert_contains(&body, "/projects')");
	}

	#[db_test]
	async fn a_taken_or_invalid_slug_is_reported(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;

		let (_, _, body) = post_signals(
			&app,
			"/teams/new",
			Some(&cookie),
			&json!({ "name": "Again", "slug": "alice" }),
		)
		.await;
		assert_contains(&body, "A team with that slug already exists.");

		let (_, _, body) = post_signals(
			&app,
			"/teams/new",
			Some(&cookie),
			&json!({ "name": "Bad", "slug": "Not Valid" }),
		)
		.await;
		assert_contains(&body, "Slug must be");
	}
}

#[cfg(test)]
mod no_team_tests {
	use crate::tests::{PgPool, db_test};
	use axum::http::StatusCode;
	use serde_json::json;

	use crate::tests::{app, call};
	use crate::ui::testing::{assert_contains, get_html, ui_login};

	#[db_test]
	async fn a_user_with_no_team_is_offered_the_create_team_form(db: PgPool) {
		let app = app(db).await;
		let creds = json!({ "name": "dana", "email": "dana@example.com", "password": "correct horse battery" });
		let (s, _) = call(&app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
		assert!(s.is_success());
		let cookie = ui_login(&app, "dana").await;

		let (s, _, body) = get_html(&app, "/", Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Create a team");
		assert_contains(&body, "dana");
	}
}
