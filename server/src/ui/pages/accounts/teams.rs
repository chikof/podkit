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
use crate::ui::components::{BUTTON, INPUT, LABEL, card, card_content};
use crate::ui::extract::UiUser;
use crate::ui::layout::{self, form_error};
use crate::ui::{error, shell, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/new", get(new_page).post(create))
}

/// The slug a team name suggests: lower-case letters and digits, one dash between words.
/// Runs in the browser while the slug has not been typed by hand (`_slugEdited`).
const SLUG_FROM_NAME: &str = "if (!$_slugEdited) $slug = $name.toLowerCase().normalize('NFKD').replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60).replace(/-+$/g, '')";

/// The create-team form, as a page body.
pub fn create_form(intro: &str) -> Markup {
	html! {
		div class="ui-create-team" data-signals="{name: '', slug: '', _slugEdited: false, _error: ''}" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Create a team" }
					p class="ui-lede" { (intro) }
				}
			}
			(card(&html! {
				(card_content(&html! {
					form class="ui-auth-form" "data-on:submit"="evt.preventDefault(); @post('/teams/new')" {
						div class="ui-field-group" {
							label class=(LABEL) for="team-name" { "Name" }
							input id="team-name" class=(INPUT) type="text" required maxlength="60" autofocus autocomplete="off" "data-bind:name" "data-effect"=(SLUG_FROM_NAME);
						}
						div class="ui-field-group" {
							label class=(LABEL) for="team-slug" { "Slug" }
							input id="team-slug" class={ (INPUT) " font-mono" } type="text" required maxlength="60" pattern="[a-z0-9]([a-z0-9\\-]*[a-z0-9])?" placeholder="my-team" autocomplete="off" "data-bind:slug" "data-on:input"="$_slugEdited = true";
							p class="ui-hint" { "Lower-case letters, digits and dashes. It is filled in from the name until you change it." }
						}
						(form_error())
						div class="ui-form-actions" {
							button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create team" }
						}
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
	cookies: tower_cookies::Cookies,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	match service::create(&s.db, user.id, &sig.name, &sig.slug, "").await {
		Ok(team) => {
			shell::flash(
				&cookies,
				crate::ui::components::Tone::Success,
				&format!("Team {} created.", team.name),
			);
			sse::redirect(&format!("/teams/{}/projects", team.id)).into_response()
		}
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
	async fn the_create_form_fills_the_slug_from_the_name_until_it_is_typed(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;
		let (_, _, body) = get_html(&app, "/teams/new", Some(&cookie)).await;
		assert_contains(&body, "_slugEdited: false");
		assert_contains(&body, "data-effect=\"if (!$_slugEdited) $slug = $name");
		assert_contains(&body, "data-on:input=\"$_slugEdited = true\"");
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
