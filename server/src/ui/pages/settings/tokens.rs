//! Settings → API tokens: make a token, see yours, revoke one.
//!
//! A token acts as the person who made it, narrowed to the scopes and projects picked here
//! (see `auth::tokens`). Any member may make one for themselves. The secret comes back once,
//! in the response to "Create token", and is never stored or sent again: the list shows only
//! the public prefix.
//!
//! The scope and project checkboxes bind one boolean signal each (`s_<scope>`, `p<id>`), so
//! the form is a plain set of native checkboxes grouped in `fieldset`s.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::frame::{settings_body, settings_crumbs};
use crate::auth::permission::{Action, Resource};
use crate::auth::tokens::service::{self, CreateInput};
use crate::auth::tokens::{ApiToken, Scope, expiry};
use crate::error::ServerError;
use crate::ids::{ApiTokenId, ProjectId, TeamId};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, SELECT, Size, Tone, Variant, badge, button_class, card,
	card_content, card_footer, card_heading, copy_field, dialog, empty_state, icon,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::{self, js_string};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/settings/api-tokens",
			get(page).post(create),
		)
		.route(
			"/teams/{team_id}/settings/api-tokens/{token_id}/revoke",
			post(revoke),
		)
}

fn base(team: TeamId) -> String {
	format!("/teams/{team}/settings/api-tokens")
}

/// What a scope lets the token do, in the words the form shows.
fn blurb(scope: Scope) -> &'static str {
	match scope {
		Scope::Read => "Read anything you can read. Secrets stay hidden.",
		Scope::ReadSensitive => "Also reveal secrets, such as database passwords.",
		Scope::Write => "Create and update.",
		Scope::Deploy => "Deploy, restart and stop.",
		Scope::Delete => "Delete.",
		Scope::Root => "Everything above, and managing API tokens.",
	}
}

/// `read:sensitive` as a signal name: `s_read_sensitive`.
fn scope_signal(scope: Scope) -> String {
	format!("s_{}", scope.as_str().replace(':', "_"))
}

/// `2026-11-17 20:03 UTC`: no locale, no JS, same everywhere.
fn when(t: OffsetDateTime) -> String {
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

fn expires_cell(token: &ApiToken) -> Markup {
	html! {
		@match token.expires_at {
			None => { "Never" }
			Some(at) if at <= OffsetDateTime::now_utc() => {
				(badge(Tone::Warning, "Expired"))
			}
			Some(at) => { (when(at)) }
		}
	}
}

/// The tokens card: head with the count, then the rows. Patched whole after a create or a
/// revoke, so the count follows.
fn token_list(tokens: &[ApiToken], project_names: &BTreeMap<i64, String>) -> Markup {
	html! {
		div id="token-list" {
			(card(&html! {
				div class="ui-card-h" {
					div {
						h2 class="ui-card-title" { "API tokens" span class="ui-count" { (tokens.len()) } }
						p class="ui-card-sub" {
							"Yours, for scripts and CI. Send one as "
							code class="ui-mono" { "Authorization: Bearer pk_…" }
							". See docs/api.md."
						}
					}
				}
				(token_rows(tokens, project_names))
			}))
		}
	}
}

fn token_rows(tokens: &[ApiToken], project_names: &BTreeMap<i64, String>) -> Markup {
	html! {
		div {
			@if tokens.is_empty() {
				div class="ui-card-b" {
					(empty_state(
						"terminal",
						"No API tokens yet.",
						Some("Make one below to call the API from a script or a CI job."),
						None,
					))
				}
			} @else {
				table class="ui-table" {
					caption class="ui-visually-hidden" { "Your API tokens" }
					thead {
						tr {
							th scope="col" { "Name" }
							th scope="col" { "Scopes" }
							th scope="col" { "Projects" }
							th scope="col" { "Expires" }
							th scope="col" { "Last used" }
							th scope="col" { span class="ui-visually-hidden" { "Actions" } }
						}
					}
					tbody {
						@for t in tokens {
							@let open = format!(
								"$_rev_id = {}; $_rev_name = {}; $_rev_error = ''; el.commandForElement || document.getElementById('revoke-token-dialog').showModal()",
								js_string(&t.id.0.to_string()),
								js_string(&t.name)
							);
							tr {
								th scope="row" {
									div class="ui-who" {
										div {
											b { (t.name) }
											small class="ui-mono" { (crate::auth::tokens::PREFIX) (t.prefix) "_…" }
										}
									}
								}
								td data-label="Scopes" {
									div class="ui-token-scopes" {
										@for scope in &t.scopes {
											span class="ui-tag ui-tag-neutral" { (scope.as_str()) }
										}
									}
								}
								td data-label="Projects" {
									@if t.project_ids.is_empty() {
										"All projects"
									} @else {
										@for (i, p) in t.project_ids.iter().enumerate() {
											@if i > 0 { ", " }
											@if let Some(name) = project_names.get(&p.0) { (name) } @else { "Project " (p.0) }
										}
									}
								}
								td data-label="Expires" { (expires_cell(t)) }
								td data-label="Last used" {
									@if let Some(at) = t.last_used_at { (when(at)) } @else { "Never" }
								}
								td class="ui-table-act" {
									button type="button" class=(button_class(Variant::Ghost, Size::Icon))
										aria-label={ "Revoke " (t.name) } command="show-modal" commandfor="revoke-token-dialog"
										aria-haspopup="dialog" "data-on:click"=(open) {
										(icon("trash-2"))
									}
								}
							}
						}
					}
				}
			}
		}
	}
}

fn revoke_dialog(team: TeamId) -> Markup {
	let action = format!(
		"@post({} + $_rev_id + '/revoke')",
		js_string(&format!("{}/", base(team)))
	);
	dialog(
		"revoke-token-dialog",
		"Revoke this token?",
		Some("Anything that uses it stops working at once. This cannot be undone."),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Revoke " strong class="break-all" "data-text"="$_rev_name" {} "?" }
				p class="text-sm text-destructive" role="alert" "data-show"="$_rev_error" "data-text"="$_rev_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(action)
				"data-indicator:_revoking" "data-attr:disabled"="$_revoking" { "Revoke token" }
		},
	)
}

fn scopes_fieldset() -> Markup {
	html! {
		fieldset class="ui-token-fieldset" {
			legend { "Scopes" }
			@for scope in Scope::ALL {
				@let signal = scope_signal(scope);
				@let id = format!("tok-{signal}");
				div class="ui-token-scope-row" {
					input id=(id) type="checkbox" class=(CHECKBOX) "data-bind"=(signal) checked[scope == Scope::Read]
						aria-describedby={ (id) "-hint" };
					label for=(id) class="ui-token-scope" {
						span class="ui-check-title ui-mono" { (scope.as_str()) }
						small id={ (id) "-hint" } { (blurb(scope)) }
					}
				}
			}
		}
	}
}

fn projects_fieldset(list: &[projects::Project]) -> Markup {
	html! {
		fieldset class="ui-token-fieldset" {
			legend { "Projects" }
			p class="ui-hint" {
				@if list.is_empty() {
					"The token can reach every project of this team."
				} @else {
					"Leave all unchecked for every project of this team, or pick the ones the token may reach. Other projects answer as if they did not exist."
				}
			}
			@for p in list {
				@let id = format!("tok-p{}", p.id.0);
				div class="ui-check" {
					input id=(id) type="checkbox" class=(CHECKBOX) "data-bind"=(format!("p{}", p.id.0));
					label for=(id) class="cursor-pointer" { (p.name) }
				}
			}
		}
	}
}

async fn load(
	s: &AppState,
	ctx: &TeamCtx,
) -> Result<(Vec<ApiToken>, Vec<projects::Project>), ServerError> {
	let tokens = service::list(&s.db, ctx.user.id, ctx.team.id).await?;
	let projects = if ctx.perms.can(Action::Read, Resource::Project) {
		projects::service::list(&s.db, ctx.user.id, ctx.team.id)
			.await?
			.into_iter()
			.map(|p| p.project)
			.collect()
	} else {
		Vec::new()
	};
	Ok((tokens, projects))
}

fn names(projects: &[projects::Project]) -> BTreeMap<i64, String> {
	projects.iter().map(|p| (p.id.0, p.name.clone())).collect()
}

/// The starting value of every signal on the page.
fn signals(projects: &[projects::Project]) -> Value {
	let mut s = json!({
		"name": "", "expires": "90",
		"_rev_id": "", "_rev_name": "", "_rev_error": "", "_error": "",
	});
	for scope in Scope::ALL {
		s[scope_signal(scope)] = Value::Bool(scope == Scope::Read);
	}
	for p in projects {
		s[format!("p{}", p.id.0)] = Value::Bool(false);
	}
	s
}

fn create_card(team: TeamId, list: &[projects::Project]) -> Markup {
	let post = format!("evt.preventDefault(); @post({})", js_string(&base(team)));
	card(&html! {
		form id="create-token" "data-on:submit"=(post) {
			(card_heading("Create a token", Some("It acts as you, limited to what you tick. The secret is shown once, right after.")))
			(card_content(&html! {
				div class="ui-pair" {
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="CI deploy" "data-bind:name";
					}
					label class=(LABEL) {
						"Expires"
						select class=(SELECT) "data-bind:expires" {
							button { selectedcontent {} }
							option value="30" { "In 30 days" }
							option value="90" selected { "In 90 days" }
							option value="365" { "In a year" }
							option value="" { "Never" }
						}
					}
				}
				(scopes_fieldset())
				(projects_fieldset(list))
				(form_error())
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
					(icon("plus"))
					span { "Create token" }
				}
			}))
		}
	})
}

/// The one-time reveal, patched over the empty `#token-created` after a create.
fn created_panel(name: &str, secret: &str) -> Markup {
	html! {
		div id="token-created" role="status" {
			(card(&html! {
				(card_heading("Copy your new token", Some("It is shown only this once. podkit keeps a hash, not the token, so a lost one cannot be recovered: revoke it and make another.")))
				(card_content(&html! {
					p class="text-sm" { "Token " strong { (name) } }
					(copy_field("token-secret", "API token", secret))
				}))
			}))
		}
	}
}

fn created_slot() -> Markup {
	html! { div id="token-created" {} }
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let (tokens, list) = load(&s, &ctx).await.map_err(|e| error::page(&ctx, &e))?;
	let body = html! {
		div class="ui-settings-tokens" "data-signals"=(signals(&list).to_string()) {
			(created_slot())
			(token_list(&tokens, &names(&list)))
			(create_card(ctx.team.id, &list))
			(revoke_dialog(ctx.team.id))
		}
	};
	Ok(app_page(
		&ctx,
		"API tokens",
		&settings_crumbs(&ctx, "API tokens"),
		&settings_body(&ctx, &body),
	))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateSignals {
	name: String,
	expires: String,
	/// `s_<scope>` and `p<project id>` booleans.
	#[serde(flatten)]
	rest: BTreeMap<String, Value>,
}

impl CreateSignals {
	fn ticked(&self, key: &str) -> bool {
		self.rest.get(key) == Some(&Value::Bool(true))
	}

	fn input(&self) -> Result<CreateInput, ServerError> {
		let days = match self.expires.trim() {
			"" => None,
			raw => Some(
				raw.parse::<u32>()
					.map_err(|_| ServerError::Validation("Pick an expiry.".to_owned()))?,
			),
		};
		let project_ids = self
			.rest
			.keys()
			.filter(|k| self.ticked(k))
			.filter_map(|k| k.strip_prefix('p')?.parse::<i64>().ok())
			.map(ProjectId)
			.collect();
		Ok(CreateInput {
			name: self.name.trim().to_owned(),
			scopes: Scope::ALL
				.into_iter()
				.filter(|s| self.ticked(&scope_signal(*s)))
				.collect(),
			project_ids,
			expires_at: expiry(days)?,
		})
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let result = async {
		let made = service::create(&s.db, ctx.user.id, ctx.team.id, sig.input()?).await?;
		let (tokens, list) = load(&s, &ctx).await?;
		Ok::<_, ServerError>((made, tokens, list))
	}
	.await;
	match result {
		Ok((made, tokens, list)) => {
			let mut reset = signals(&list);
			reset["name"] = Value::String(String::new());
			sse::patch(&created_panel(&made.token.name, &made.secret))
				.and(sse::patch(&token_list(&tokens, &names(&list))))
				// Back to the form as rendered (and out of the "touched" state that paints an
				// empty name red), then the signals to match.
				.and(sse::script(
					"document.getElementById('create-token').reset()",
				))
				.and(sse::signals(&reset))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn revoke(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, token)): Path<(TeamId, ApiTokenId)>,
) -> Response {
	let result = async {
		service::revoke(&s.db, ctx.user.id, ctx.team.id, token).await?;
		load(&s, &ctx).await
	}
	.await;
	match result {
		Ok((tokens, list)) => sse::patch(&token_list(&tokens, &names(&list)))
			.and(sse::script(
				"document.getElementById('revoke-token-dialog')?.close()",
			))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_rev_error": error::message(&e) })).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::{Value, json};

	use crate::tests::{PgPool, app, call, count, db_test, registries_ui};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
		ui_session,
	};

	fn page(team: i64) -> String {
		format!("/teams/{team}/settings/api-tokens")
	}

	/// The `pk_…` string in a create response.
	fn secret_in(body: &str) -> String {
		let start = body.find("pk_").expect("a token in the response");
		body[start..]
			.chars()
			.take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
			.collect()
	}

	fn form(name: &str) -> Value {
		json!({
			"name": name, "expires": "30", "s_read": true, "s_write": false, "s_root": false,
		})
	}

	#[db_test]
	async fn creating_shows_the_secret_once_and_stores_only_a_hash(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;

		let (s, _, body) = get_html(&app, &page(team), Some(&alice)).await;
		assert_eq!(s, StatusCode::OK);
		let main = page_main(&body);
		assert_contains(main, "No API tokens yet.");
		assert_contains(main, "read:sensitive");
		assert_eq!(main.matches("<h1").count(), 1);

		let (s, _, body) = post_signals(&app, &page(team), Some(&alice), &form("CI")).await;
		assert_eq!(s, StatusCode::OK);
		let secret = secret_in(&body);
		assert!(secret.len() > 60, "{secret}");
		assert_contains(&body, "token-created");

		// Never again: not on the page, not in the database.
		let (_, _, body) = get_html(&app, &page(team), Some(&alice)).await;
		assert_not_contains(&body, &secret);
		assert_contains(page_main(&body), "CI");
		let hash = registries_ui::token_hash(&db).await;
		assert!(!String::from_utf8_lossy(&hash).contains(&secret));
		assert_ne!(hash, secret.as_bytes());

		// And it works as a bearer token on the API.
		let (s, _) = call(&app, "GET", "/api/v1/teams", Some(&secret), None).await;
		assert_eq!(s, StatusCode::OK);
		// It is not a login for the web UI.
		let (s, _, _) = crate::ui::testing::send(
			&app,
			"GET",
			&page(team),
			Some(&format!("session={secret}")),
			None,
			None,
		)
		.await;
		assert_ne!(s, StatusCode::OK);
	}

	#[db_test]
	async fn names_are_escaped_and_bad_forms_are_refused(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;

		let (s, _, body) =
			post_signals(&app, &page(team), Some(&alice), &form("<script>x</script>")).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&body, "<script>x");
		let (_, _, body) = get_html(&app, &page(team), Some(&alice)).await;
		assert_not_contains(page_main(&body), "<script>x");
		assert_contains(page_main(&body), "&lt;script&gt;");

		// No scope ticked, or an unknown project: nothing is made.
		let none = json!({ "name": "empty", "expires": "30" });
		let (_, _, body) = post_signals(&app, &page(team), Some(&alice), &none).await;
		assert_contains(&body, "_error");
		let foreign = json!({ "name": "x", "expires": "30", "s_read": true, "p999999": true });
		let (_, _, body) = post_signals(&app, &page(team), Some(&alice), &foreign).await;
		assert_contains(&body, "_error");
		assert_eq!(count(&db, "api_tokens").await, 1);
	}

	#[db_test]
	async fn revoking_takes_effect_at_once_and_only_for_the_owner(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;
		let (_, _, body) = post_signals(&app, &page(team), Some(&alice), &form("CI")).await;
		let secret = secret_in(&body);
		let id = registries_ui::token_id(&db).await;

		// Another account, even a member of nothing, cannot revoke it.
		let creds = json!({ "name": "mallory", "email": "mallory@example.com", "password": "correct horse battery" });
		call(&app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
		let mallory = ui_login(&app, "mallory").await;
		let url = format!("{}/{id}/revoke", page(team));
		let (_, _, _) = post_signals(&app, &url, Some(&mallory), &json!({})).await;
		assert_eq!(count(&db, "api_tokens").await, 1);
		let (s, _) = call(&app, "GET", "/api/v1/teams", Some(&secret), None).await;
		assert_eq!(s, StatusCode::OK);

		let (s, _, _) = post_signals(&app, &url, Some(&alice), &json!({})).await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(count(&db, "api_tokens").await, 0);
		let (s, _) = call(&app, "GET", "/api/v1/teams", Some(&secret), None).await;
		assert_eq!(s, StatusCode::UNAUTHORIZED);
	}
}
