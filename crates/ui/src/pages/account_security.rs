//! Account → Security: the signed-in person's own sign-in settings. An authenticator app, recovery
//! codes, passkeys, the sessions that are open, and the identity providers linked to the
//! account. It sits inside a team's frame (`/teams/{team}/account/security`) because the shell
//! needs one, but nothing on it belongs to the team, and a team that requires two-step
//! verification sends its members here until they have set it up.
//!
//! Changes that would weaken the account (turning a factor off, new recovery codes, removing a
//! passkey) ask for the password again when the account has one. Recovery codes are shown
//! once, in the response that makes them, and never again.
//!
//! The handlers call `two_factor`, `sso` and `auth` services only.

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower_cookies::Cookies;

use crate::assets::passkey_script;
use crate::components::{
	BUTTON, BannerKind, INPUT, LABEL, Size, Tone, Variant, badge, banner, button_class, card,
	card_content, card_heading, dialog, empty_state, icon,
};
use crate::extract::{TeamCtx, UiUser};
use crate::layout::{app_page, form_error};
use crate::sse::{self, js_string};
use crate::{error, shell};
use podkit_core::auth::origin::external_base;
use podkit_core::auth::{self, SessionToken};
use podkit_core::error::{AppResult, ServerError};
use podkit_core::ids::{IdentityId, PasskeyId};
use podkit_core::sso::signin;
use podkit_core::state::AppState;
use podkit_core::two_factor::policy::{self, Standing};
use podkit_core::two_factor::service::{self as tf, Enrollment, Status};
use podkit_core::two_factor::{passkeys, totp};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/account/security", get(page))
		.route(
			"/teams/{team_id}/account/security/totp/start",
			post(totp_start),
		)
		.route(
			"/teams/{team_id}/account/security/totp/confirm",
			post(totp_confirm),
		)
		.route(
			"/teams/{team_id}/account/security/totp/cancel",
			post(totp_cancel),
		)
		.route(
			"/teams/{team_id}/account/security/totp/disable",
			post(totp_disable),
		)
		.route(
			"/teams/{team_id}/account/security/recovery",
			post(recovery_new),
		)
		.route(
			"/teams/{team_id}/account/security/passkeys/{passkey_id}/remove",
			post(passkey_remove),
		)
		.route(
			"/teams/{team_id}/account/security/sessions/{session_id}/revoke",
			post(session_revoke),
		)
		.route(
			"/teams/{team_id}/account/security/sessions/revoke-others",
			post(sessions_revoke_others),
		)
		.route(
			"/teams/{team_id}/account/security/identities/{identity_id}/unlink",
			post(identity_unlink),
		)
		.route("/account/passkeys/start", post(passkey_start))
		.route("/account/passkeys/finish", post(passkey_finish))
}

fn base(ctx: &TeamCtx) -> String {
	format!("/teams/{}/account/security", ctx.team.id)
}

/// `2026-11-17 20:03 UTC`: no locale, no script, the same everywhere.
pub(crate) fn when(t: OffsetDateTime) -> String {
	let t = t.to_offset(time::UtcOffset::UTC);
	format!(
		"{}-{:02}-{:02} {:02}:{:02} UTC",
		t.date().year(),
		t.month() as u8,
		t.day(),
		t.hour(),
		t.minute(),
	)
}

/// What is left of a user-agent string a person can recognise: it is cut, and maud escapes it.
fn browser(user_agent: Option<&str>) -> String {
	let ua = user_agent.unwrap_or("Unknown browser");
	let known = [
		("Firefox/", "Firefox"),
		("Edg/", "Edge"),
		("Chrome/", "Chrome"),
		("Safari/", "Safari"),
	];
	let name = known.iter().find(|(m, _)| ua.contains(m)).map(|(_, n)| *n);
	let os = ["Windows", "Android", "iPhone", "iPad", "Mac OS X", "Linux"]
		.iter()
		.find(|o| ua.contains(**o))
		.copied();
	match (name, os) {
		(Some(n), Some(o)) => format!("{n} on {o}"),
		(Some(n), None) => n.to_owned(),
		_ => ua.chars().take(48).collect(),
	}
}

// --- pieces of the page ----------------------------------------------------------

/// The authenticator app card; patched whole after a step.
fn totp_card(ctx: &TeamCtx, status: &Status, pending: Option<&Enrollment>) -> Markup {
	let base = base(ctx);
	let content = if let Some(setup) = pending {
		let qr = totp::qr(&setup.uri);
		html! {
			(card_heading("Authenticator app", Some("Scan the code with an app such as Aegis, 2FAS, Google Authenticator or your password manager.")))
			(card_content(&html! {
				div class="ui-totp-setup" {
					@if let Some(qr) = &qr {
						svg class="ui-qr" viewBox={ "0 0 " (qr.size) " " (qr.size) } role="img" aria-label="QR code for your authenticator app" shape-rendering="crispEdges" {
							path d=(qr.path) {}
						}
					}
					div class="ui-totp-key" {
						p class="ui-hint" { "Can't scan it? Type this key into the app:" }
						code class="ui-mono ui-totp-secret" { (setup.secret) }
					}
				}
				form class="ui-totp-confirm" "data-on:submit"={ "evt.preventDefault(); @post(" (js_string(&format!("{base}/totp/confirm"))) ")" } {
					div class="ui-field-group" {
						label class=(LABEL) for="totp-code" { "Code from the app" }
						p id="totp-code-hint" class="ui-hint" { "Six digits. Enter it to finish." }
						input id="totp-code" class=(INPUT) type="text" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]{6,7}" maxlength="7" required aria-describedby="totp-code-hint" "data-bind:code";
					}
					(form_error())
					div class="ui-actions" {
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("check")) span { "Turn on" } }
						button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"={ "@post(" (js_string(&format!("{base}/totp/cancel"))) ")" } { "Cancel" }
					}
				}
			}))
		}
	} else if status.totp {
		html! {
			(card_heading("Authenticator app", Some("A code from your app is asked for after your password.")))
			(card_content(&html! {
				div class="ui-actions" {
					(badge(Tone::Success, "On"))
					(confirm_button(
						"Turn off",
						&format!("{base}/totp/disable"),
						"Turn off the authenticator app?",
						"Signing in will need only your password again, unless you have a passkey.",
						Variant::Outline,
					))
				}
			}))
		}
	} else {
		html! {
			(card_heading("Authenticator app", Some("Ask for a 6-digit code from an app on your phone after the password.")))
			(card_content(&html! {
				div class="ui-actions" {
					(badge(Tone::Neutral, "Off"))
					button type="button" class=(BUTTON) "data-on:click"={ "@post(" (js_string(&format!("{base}/totp/start"))) ")" } { (icon("plus")) span { "Set up" } }
				}
			}))
		}
	};
	html! { div id="totp-card" { (card(&content)) } }
}

/// A button that opens the shared "Confirm with your password" dialog for `action`.
fn confirm_button(label: &str, action: &str, title: &str, text: &str, variant: Variant) -> Markup {
	let open = format!(
		"$_act = {}; $_title = {}; $_text = {}; $pw = ''; $_confirm_error = ''; el.commandForElement || document.getElementById('confirm-dialog').showModal()",
		js_string(action),
		js_string(title),
		js_string(text),
	);
	html! {
		button type="button" class=(button_class(variant, Size::Md)) command="show-modal" commandfor="confirm-dialog"
			aria-haspopup="dialog" "data-on:click"=(open) { (label) }
	}
}

fn confirm_dialog(has_password: bool) -> Markup {
	dialog(
		"confirm-dialog",
		"Are you sure?",
		None,
		&html! {
			div class="flex flex-col gap-3" {
				h3 class="ui-dialog-sub" "data-text"="$_title" {}
				p class="text-sm" "data-text"="$_text" {}
				@if has_password {
					div class="ui-field-group" {
						label class=(LABEL) for="confirm-pw" { "Your password" }
						input id="confirm-pw" class=(INPUT) type="password" autocomplete="current-password" "data-bind:pw";
					}
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_confirm_error" "data-text"="$_confirm_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" } }
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"="@post($_act)" "data-indicator:_confirming" "data-attr:disabled"="$_confirming" { "Confirm" }
		},
	)
}

/// The codes, once. The slot exists on every page (hidden) so the passkey script can fill it.
fn codes_block(codes: &[String], back: &str) -> Markup {
	html! {
		div id="recovery-codes" data-passkey-codes hidden[codes.is_empty()] role="status" {
			(card(&html! {
				(card_heading("Save your recovery codes", Some("Each works once if you lose your phone or passkey. They are shown now and never again. Keep them somewhere safe, like your password manager.")))
				(card_content(&html! {
					ol class="ui-codes" {
						@for code in codes { li { code class="ui-mono" { (code) } } }
					}
					a class=(BUTTON) href=(back) { (icon("check")) span { "I saved them" } }
				}))
			}))
		}
	}
}

fn recovery_card(ctx: &TeamCtx, status: &Status) -> Markup {
	let base = base(ctx);
	html! { div id="recovery-card" { (card(&html! {
		(card_heading("Recovery codes", Some("One-time codes for when your phone or passkey is not at hand.")))
		(card_content(&html! {
			@if status.enrolled() {
				div class="ui-actions" {
					(badge(if status.recovery_left > 2 { Tone::Success } else { Tone::Warning }, &format!("{} left", status.recovery_left)))
					(confirm_button(
						"Make new codes",
						&format!("{base}/recovery"),
						"Make new recovery codes?",
						"The old codes stop working at once.",
						Variant::Outline,
					))
				}
			} @else {
				p class="text-sm text-muted-foreground" { "Turn on an authenticator app or add a passkey first: the codes go with them." }
			}
		}))
	})) } }
}

fn passkeys_card(ctx: &TeamCtx, status: &Status, rp_sync: &Markup) -> Markup {
	let base = base(ctx);
	card(&html! {
		(card_heading("Passkeys", Some("Sign in with your fingerprint, face or a security key. A passkey also counts as two-step verification.")))
		(card_content(&html! {
			@if status.passkeys.is_empty() {
				(empty_state("key-round", "No passkeys yet.", Some("Add one to this device or a security key."), None))
			} @else {
				ul class="ui-sec-list" aria-label="Your passkeys" {
					@for p in &status.passkeys {
						li class="ui-sec-item" {
							div class="ui-sec-main" {
								b { (p.name) }
								small class="ui-hint" { "Added " (when(p.created_at)) " · " @if let Some(at) = p.last_used_at { "last used " (when(at)) } @else { "never used" } }
							}
							(confirm_button(
								"Remove",
								&format!("{base}/passkeys/{}/remove", p.id),
								&format!("Remove the passkey {}?", p.name),
								"You will not be able to sign in with it any more.",
								Variant::Ghost,
							))
						}
					}
				}
			}
			div data-passkey-scope {
				div data-passkey-register-form {
					div class="ui-field-group" {
						label class=(LABEL) for="passkey-name" { "Name" }
						input id="passkey-name" class=(INPUT) type="text" maxlength="60" autocomplete="off" placeholder="Laptop, phone, YubiKey";
					}
					div class="ui-actions" {
						button type="button" class=(BUTTON) data-passkey="register" hidden { (icon("plus")) span { "Add a passkey" } }
					}
				}
				p class="ui-hint" data-passkey-status role="status" {}
				p class="ui-hint" data-passkey-note hidden { "Passkeys need a secure connection (HTTPS, or localhost) and a browser that supports them." }
			}
			(rp_sync)
		}))
	})
}

fn sessions_card(ctx: &TeamCtx, sessions: &[auth::session::Entry]) -> Markup {
	let base = base(ctx);
	card(&html! {
		(card_heading("Sessions", Some("Where you are signed in. Sign out of any you do not recognise.")))
		div {
			table class="ui-table" {
				caption class="ui-visually-hidden" { "Your open sessions" }
				thead { tr {
					th scope="col" { "Browser" }
					th scope="col" { "Signed in with" }
					th scope="col" { "Started" }
					th scope="col" { span class="ui-visually-hidden" { "Actions" } }
				} }
				tbody {
					@for s in sessions {
						tr {
							th scope="row" {
								(browser(s.user_agent.as_deref()))
								@if let Some(ip) = &s.ip { small class="ui-mono" { " " (ip) } }
							}
							td data-label="Signed in with" { (method_label(&s.method)) }
							td data-label="Started" { (when(s.created_at)) }
							td class="ui-table-act" {
								@if s.current {
									(badge(Tone::Success, "This device"))
								} @else {
									button type="button" class=(button_class(Variant::Ghost, Size::Md))
										"data-on:click"={ "@post(" (js_string(&format!("{base}/sessions/{}/revoke", s.id))) ")" } { "Sign out" }
								}
							}
						}
					}
				}
			}
			@if sessions.iter().any(|s| !s.current) {
				div class="ui-card-b" {
					button type="button" class=(button_class(Variant::Outline, Size::Md))
						"data-on:click"={ "@post(" (js_string(&format!("{base}/sessions/revoke-others"))) ")" } { "Sign out all other sessions" }
				}
			}
		}
	})
}

fn method_label(method: &str) -> &'static str {
	match method {
		"sso" => "Identity provider",
		"passkey" => "Passkey",
		_ => "Password",
	}
}

fn linked_card(ctx: &TeamCtx, linkages: &[signin::Linkage]) -> Markup {
	let base = base(ctx);
	card(&html! {
		(card_heading("Linked sign-ins", Some("Identity providers that sign in to this account.")))
		(card_content(&html! {
			ul class="ui-sec-list" aria-label="Linked sign-ins" {
				@for l in linkages {
					li class="ui-sec-item" {
						div class="ui-sec-main" {
							b { (l.provider) }
							@if let Some(email) = &l.email { small class="ui-hint" { (email) } }
						}
						button type="button" class=(button_class(Variant::Ghost, Size::Md))
							"data-on:click"={ "@post(" (js_string(&format!("{base}/identities/{}/unlink", l.id.0))) ")" } { "Unlink" }
					}
				}
			}
		}))
	})
}

/// What the teams the person is in ask of them.
async fn policy_banners(s: &AppState, ctx: &TeamCtx) -> AppResult<Markup> {
	let mut out = Vec::new();
	for team in &ctx.teams {
		match policy::standing(&s.db, ctx.user.id, team.id).await? {
			Standing::Fine => {}
			Standing::Grace(deadline) => out.push(banner(
				BannerKind::Warning,
				Some(&format!("{} requires two-step verification.", team.name)),
				&format!(
					"Set it up before {}, or you lose access to the team.",
					when(deadline)
				),
				None,
			)),
			Standing::Blocked => out.push(banner(
				BannerKind::Error,
				Some(&format!("{} requires two-step verification.", team.name)),
				"You cannot open that team until you turn on an authenticator app or add a passkey.",
				None,
			)),
		}
	}
	Ok(html! { @for b in &out { (b) } })
}

async fn render(
	s: &AppState,
	ctx: &TeamCtx,
	token: &str,
	origin: &str,
	revealed: &[String],
) -> AppResult<Markup> {
	let user = ctx.user.id;
	let status = tf::status(&s.db, user).await?;
	let has_password = tf::has_password(&s.db, user).await?;
	let pending = if status.totp {
		None
	} else {
		tf::pending_totp(s, user).await?
	};
	let sessions = auth::service::sessions(&s.db, user, token).await?;
	let linkages = signin::linkages(&s.db, user).await?;
	let banners = policy_banners(s, ctx).await?;
	let sync = passkeys::sync_info(&s.db, origin, user).await?;
	let rp_sync = html! {
		div data-passkey-sync data-rp-id=(sync.rp_id) data-user-id=(sync.user_id) data-credential-ids=(sync.credential_ids) hidden {}
	};
	let base = base(ctx);
	let body = html! {
		header class="ui-page-head" {
			div class="ui-page-title" {
				p class="ui-eyebrow" { "You" }
				h1 { "Security" }
				p class="ui-lede" { "How you sign in to podkit, and where you are signed in." }
			}
		}
		(banners)
		div class="ui-account" "data-signals"=(json!({ "code": "", "_error": "", "pw": "", "_act": "", "_title": "", "_text": "", "_confirm_error": "" }).to_string()) {
			(codes_block(revealed, &base))
			(totp_card(ctx, &status, pending.as_ref()))
			(recovery_card(ctx, &status))
			(passkeys_card(ctx, &status, &rp_sync))
			(sessions_card(ctx, &sessions))
			@if !linkages.is_empty() { (linked_card(ctx, &linkages)) }
			(confirm_dialog(has_password))
		}
		(passkey_script())
	};
	let account = format!("/teams/{}/account", ctx.team.id);
	Ok(app_page(
		ctx,
		"Security",
		&[("Account", Some(account)), ("Security", None)],
		&body,
	))
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	SessionToken(token): SessionToken,
) -> Response {
	let origin = external_base(&s.config, &headers);
	match render(&s, &ctx, &token, &origin, &[]).await {
		Ok(page) => page.into_response(),
		Err(e) => error::page(&ctx, &e),
	}
}

// --- actions -------------------------------------------------------------------------

/// Back to the page after a change, with a message for the toast.
fn done(cookies: &Cookies, ctx: &TeamCtx, message: &str) -> Response {
	shell::flash(cookies, Tone::Success, message);
	sse::redirect(&base(ctx)).into_response()
}

async fn card_now(s: &AppState, ctx: &TeamCtx, pending: Option<&Enrollment>) -> AppResult<Markup> {
	let status = tf::status(&s.db, ctx.user.id).await?;
	Ok(totp_card(ctx, &status, pending))
}

async fn totp_start(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let result = async {
		let setup = tf::begin_totp(&s, ctx.user.id).await?;
		card_now(&s, &ctx, Some(&setup)).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn totp_cancel(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let result = async {
		tf::cancel_totp(&s.db, ctx.user.id).await?;
		card_now(&s, &ctx, None).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card).into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodeSignals {
	code: String,
}

async fn totp_confirm(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<CodeSignals>,
) -> Response {
	match tf::confirm_totp(&s, ctx.user.id, &sig.code).await {
		Ok(codes) if codes.is_empty() => done(&cookies, &ctx, "Authenticator app turned on."),
		Ok(codes) => reveal(&s, &ctx, &codes).await,
		Err(e) => {
			sse::signals(&json!({ "_error": error::message(&e), "code": "" })).into_response()
		}
	}
}

/// Patches in the new codes and the refreshed cards; the page is not reloaded, so the codes
/// are on screen until the person leaves.
async fn reveal(s: &AppState, ctx: &TeamCtx, codes: &[String]) -> Response {
	let result = async {
		let status = tf::status(&s.db, ctx.user.id).await?;
		Ok::<_, ServerError>((totp_card(ctx, &status, None), recovery_card(ctx, &status)))
	}
	.await;
	match result {
		Ok((totp, recovery)) => sse::patch(&codes_block(codes, &base(ctx)))
			.and(sse::patch(&totp))
			.and(sse::patch(&recovery))
			.and(sse::script(
				"document.getElementById('confirm-dialog')?.close(); document.getElementById('recovery-codes')?.scrollIntoView({block: 'start'})",
			))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PasswordSignals {
	#[serde(rename = "pw")]
	password: String,
}

/// A refused confirmation says so inside the dialog.
fn confirm_failed(e: &ServerError) -> Response {
	sse::signals(&json!({ "_confirm_error": error::message(e), "pw": "" })).into_response()
}

async fn totp_disable(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<PasswordSignals>,
) -> Response {
	match tf::disable_totp(&s.db, ctx.user.id, &sig.password).await {
		Ok(()) => done(&cookies, &ctx, "Authenticator app turned off."),
		Err(e) => confirm_failed(&e),
	}
}

async fn recovery_new(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<PasswordSignals>,
) -> Response {
	match tf::regenerate_recovery_codes(&s.db, ctx.user.id, &sig.password).await {
		Ok(codes) => reveal(&s, &ctx, &codes).await,
		Err(e) => confirm_failed(&e),
	}
}

async fn passkey_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(i64, PasskeyId)>,
	ReadSignals(sig): ReadSignals<PasswordSignals>,
) -> Response {
	match passkeys::remove(&s.db, ctx.user.id, id, &sig.password).await {
		Ok(()) => done(&cookies, &ctx, "Passkey removed."),
		Err(e) => confirm_failed(&e),
	}
}

async fn session_revoke(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(i64, i64)>,
) -> Response {
	match auth::service::revoke_session(&s.db, ctx.user.id, id).await {
		Ok(()) => done(&cookies, &ctx, "Signed out of that session."),
		Err(e) => error::action(&e),
	}
}

async fn sessions_revoke_others(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	SessionToken(token): SessionToken,
) -> Response {
	match auth::service::revoke_other_sessions(&s.db, ctx.user.id, &token).await {
		Ok(_) => done(&cookies, &ctx, "Signed out of every other session."),
		Err(e) => error::action(&e),
	}
}

async fn identity_unlink(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(i64, i64)>,
) -> Response {
	match signin::unlink(&s.db, ctx.user.id, IdentityId(id)).await {
		Ok(()) => done(&cookies, &ctx, "Provider sign-in removed."),
		Err(e) => error::action(&e),
	}
}

// --- the passkey ceremony (JSON, for passkey.js) ---------------------------------------

fn json_error(e: &ServerError) -> Response {
	(e.status(), Json(json!({ "error": error::message(e) }))).into_response()
}

async fn passkey_start(State(s): State<AppState>, headers: HeaderMap, user: UiUser) -> Response {
	let origin = external_base(&s.config, &headers);
	let result = async {
		let email = auth::service::email_of(&s.db, user.id)
			.await?
			.ok_or(ServerError::NotFound)?;
		passkeys::start_enroll(&s.db, &origin, user.id, &email, &user.name).await
	}
	.await;
	match result {
		Ok(c) => Json(json!({ "flow": c.flow, "options": c.options })).into_response(),
		Err(e) => json_error(&e),
	}
}

#[derive(Deserialize)]
struct EnrollFinish {
	flow: String,
	#[serde(default)]
	name: String,
	credential: Value,
}

async fn passkey_finish(
	State(s): State<AppState>,
	headers: HeaderMap,
	user: UiUser,
	Json(body): Json<EnrollFinish>,
) -> Response {
	let origin = external_base(&s.config, &headers);
	let result = async {
		let credential = serde_json::from_value(body.credential).map_err(|_| {
			ServerError::Validation("The passkey answer could not be read.".to_owned())
		})?;
		passkeys::finish_enroll(&s.db, &origin, user.id, &body.flow, &body.name, &credential).await
	}
	.await;
	match result {
		Ok(codes) => Json(json!({ "codes": codes })).into_response(),
		Err(e) => json_error(&e),
	}
}
