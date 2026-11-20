//! The signed-in person's own page: their name, their sign-in email and their picture.
//! It sits inside a team's frame (`/teams/{team}/account`) because the shell needs one, but
//! nothing on it belongs to the team. Only the owner can change their picture.

use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maud::html;
use tower_cookies::Cookies;

use crate::auth::service as auth_service;
use crate::avatars::{self, MAX_UPLOAD};
use crate::error::AppResult;
use crate::state::AppState;
use crate::ui::components::{
	AvatarField, Tone, avatar_field, card, card_content, card_heading, user_avatar_lg,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::{error, shell};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/account", get(page))
		.route(
			"/teams/{team_id}/account/avatar",
			post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD + 64 * 1024)),
		)
		.route("/teams/{team_id}/account/avatar/remove", post(remove))
}

fn url(ctx: &TeamCtx) -> String {
	format!("/teams/{}/account", ctx.team.id)
}

/// Turns the outcome of a picture change into a toast on the page the person came from.
/// A plain form post, so the answer is a redirect, not a stream.
pub fn answer(cookies: &Cookies, back: &str, result: AppResult<()>, done: &str) -> Response {
	match result {
		Ok(()) => shell::flash(cookies, Tone::Success, done),
		Err(e) => shell::flash(cookies, Tone::Danger, &error::message(&e)),
	}
	Redirect::to(back).into_response()
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let email = match auth_service::email_of(&s.db, ctx.user.id).await {
		Ok(email) => email,
		Err(e) => return error::bare(Some(&ctx.user), &e),
	};
	let current = user_avatar_lg(&ctx.user.name, ctx.user.id, ctx.user.avatar.as_deref());
	let base = url(&ctx);
	let remove = format!("{base}/avatar/remove");
	let body = html! {
		header class="ui-page-head" {
			div class="ui-page-title" {
				p class="ui-eyebrow" { "You" }
				h1 { "Account" }
				p class="ui-lede" { "How you appear to the people you share a team with." }
			}
		}
		div class="ui-account" {
			(card(&html! {
				(card_heading("Profile", Some("The name and the email you sign in with.")))
				(card_content(&html! {
					dl class="ui-facts-col" {
						div { dt class="ui-hint" { "Name" } dd class="text-sm" { (ctx.user.name) } }
						div { dt class="ui-hint" { "Email" } dd class="text-sm" { (email.unwrap_or_default()) } }
					}
				}))
			}))
			(card(&html! {
				(card_heading("Picture", Some("Teammates see it beside your name in the sidebar and the members list.")))
				(card_content(&avatar_field(&AvatarField {
					id: "account-picture",
					subject: "account",
					current: &current,
					action: &format!("{base}/avatar"),
					remove_action: ctx.user.avatar.is_some().then_some(remove.as_str()),
				})))
			}))
		}
	};
	app_page(&ctx, "Account", &[("Account", None)], &body).into_response()
}

async fn upload(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	form: Multipart,
) -> Response {
	let result = match avatars::read_upload(form).await {
		Ok((declared, bytes)) => {
			avatars::service::set_user(&s.db, ctx.user.id, ctx.user.id, declared.as_deref(), &bytes)
				.await
				.map(|_| ())
		}
		Err(e) => Err(e),
	};
	answer(&cookies, &url(&ctx), result, "Your picture was updated.")
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx, cookies: Cookies) -> Response {
	let result = avatars::service::clear_user(&s.db, ctx.user.id, ctx.user.id).await;
	answer(&cookies, &url(&ctx), result, "Your picture was removed.")
}
