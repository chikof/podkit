//! Settings → Authentication: how people sign in to this team's podkit. The team policy "require
//! two-step verification for members" (with a grace period), the team's own identity providers,
//! and, for the instance administrator, the providers of the whole instance.
//!
//! A provider is `OpenID` Connect (Authentik, Keycloak, Authelia, Google, Microsoft, Dex ...) or
//! GitHub. The client secret is write-only: it is encrypted on its way in and the page never
//! shows it (an edit that leaves the field empty keeps it). Signing in through a provider may
//! create the account (just in time) only when the registration policy lets accounts be created;
//! it never links an existing account on the email alone.
//!
//! The handlers call the `sso` and `two_factor` services only; who may do what is decided there.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_cookies::Cookies;

use super::frame::{settings_body, settings_crumbs};
use crate::auth::origin::external_base;
use crate::auth::permission::{Action, Resource};
use crate::error::{AppResult, ServerError};
use crate::ids::{AuthProviderId, TeamId};
use crate::sso::providers::{self, Input, Kind, Provider, Scope};
use crate::sso::signin::redirect_uri;
use crate::state::AppState;
use crate::two_factor::policy;
use crate::ui::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, SELECT, Size, TEXTAREA, Tone, Variant, badge, button_class,
	card, card_content, card_footer, card_heading, copy_field, empty_state, icon, save_bar,
	save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::pages::account_security::when;
use crate::ui::sse::{self, js_string};
use crate::ui::{error, shell};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/settings/authentication", get(page))
		.route(
			"/teams/{team_id}/settings/authentication/policy",
			post(save_policy),
		)
		.route(
			"/teams/{team_id}/settings/authentication/{scope}/providers",
			post(create),
		)
		.route(
			"/teams/{team_id}/settings/authentication/{scope}/providers/{provider_id}",
			get(edit_page).post(update),
		)
		.route(
			"/teams/{team_id}/settings/authentication/{scope}/providers/{provider_id}/delete",
			post(delete),
		)
}

fn base(team: TeamId) -> String {
	format!("/teams/{team}/settings/authentication")
}

/// The path segment of a scope, and back. Anything else is a page that does not exist.
fn scope_of(ctx: &TeamCtx, segment: &str) -> Result<Scope, ServerError> {
	match segment {
		"team" => Ok(Scope::Team(ctx.team.id)),
		"instance" => Ok(Scope::Instance),
		_ => Err(ServerError::NotFound),
	}
}

fn segment(scope: Scope) -> &'static str {
	match scope {
		Scope::Team(_) => "team",
		Scope::Instance => "instance",
	}
}

// --- the page ------------------------------------------------------------------------

fn policy_card(
	ctx: &TeamCtx,
	policy: &policy::Policy,
	without: &[String],
	can_edit: bool,
) -> Markup {
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/policy", base(ctx.team.id)))
	);
	card(&html! {
		(card_heading("Two-step verification", Some("Ask every member of this team for a second step when they sign in.")))
		@if can_edit {
			form "data-on:submit"=(save) {
				(card_content(&html! {
					div class="ui-check-row" {
						input id="p-require" type="checkbox" switch role="switch" class="ui-switch" "data-bind:require" aria-describedby="p-require-help";
						div {
							label for="p-require" class="ui-check-title" { "Require two-step verification for members" }
							small id="p-require-help" { "A member needs an authenticator app or a passkey. Until they have one they can reach only their own Account pages." }
						}
					}
					div class="ui-field-group" {
						label class=(LABEL) for="p-grace" { "Grace period (days)" }
						input id="p-grace" class={ (INPUT) " max-w-32" } type="number" min="0" max=(policy::MAX_GRACE_DAYS) inputmode="numeric" required "data-bind:grace" aria-describedby="p-grace-help";
						p id="p-grace-help" class="ui-hint" { "How long members have to set it up after you switch it on. 0 shuts them out at once." }
					}
					(status_line(policy, without))
				}))
				(save_bar(&["require", "grace"]))
			}
		} @else {
			(card_content(&html! {
				p class="text-sm" { @if policy.required { "Required for members of this team." } @else { "Not required." } }
				(status_line(policy, without))
				p class="mt-2 text-sm text-muted-foreground" { "Only people who can update the team may change it." }
			}))
		}
	})
}

fn status_line(policy: &policy::Policy, without: &[String]) -> Markup {
	html! {
		@if policy.required {
			p class="text-sm text-muted-foreground" {
				@if without.is_empty() {
					"Every member has a second step."
				} @else {
					(without.len()) " of the members have none yet: " (without.join(", ")) "."
				}
				@if let Some(deadline) = policy.deadline {
					" Their time runs out " (when(deadline)) "."
				}
			}
		}
	}
}

fn provider_list(
	ctx: &TeamCtx,
	scope: Scope,
	list: &[Provider],
	title: &str,
	text: &str,
) -> Markup {
	let base = base(ctx.team.id);
	card(&html! {
		(card_heading(title, Some(text)))
		@if list.is_empty() {
			div class="ui-card-b" { (empty_state("key-round", "No providers yet.", Some("Add one below."), None)) }
		} @else {
			ul class="ui-sec-list" aria-label=(title) {
				@for p in list {
					li class="ui-sec-item" {
						div class="ui-sec-main" {
							b { (p.name) }
							small class="ui-hint" { (p.kind.label()) @if let Some(issuer) = &p.issuer { " · " (issuer) } }
						}
						div class="ui-actions" {
							@if !p.enabled { (badge(Tone::Warning, "Off")) }
							a class=(button_class(Variant::Outline, Size::Md)) href={ (base) "/" (segment(scope)) "/providers/" (p.id) } { "Edit" }
						}
					}
				}
			}
		}
	})
}

/// The fields of the form, bound to signals `p_*`. `team` is the scope for the group mapping.
fn fields(scope: Scope, editing: bool) -> Markup {
	html! {
		div class="ui-pair" {
			label class=(LABEL) {
				"Kind"
				select class=(SELECT) "data-bind:p_kind" disabled[editing] {
					button { selectedcontent {} }
					option value="oidc" { "OpenID Connect" }
					option value="github" { "GitHub" }
				}
			}
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="Authentik" "data-bind:p_name";
			}
		}
		div class="ui-pair" {
			label class=(LABEL) {
				"Slug"
				input class=(INPUT) type="text" required maxlength="40" pattern="[a-z0-9\\-]+" autocomplete="off" placeholder="authentik" "data-bind:p_slug";
			}
			label class=(LABEL) {
				"Issuer URL (OpenID Connect) or GitHub Enterprise address"
				input class=(INPUT) type="url" autocomplete="off" placeholder="https://auth.example.com/application/o/podkit/" "data-bind:p_issuer";
			}
		}
		div class="ui-pair" {
			label class=(LABEL) {
				"Client ID"
				input class=(INPUT) type="text" required autocomplete="off" "data-bind:p_client_id";
			}
			label class=(LABEL) {
				"Client secret"
				input class=(INPUT) type="password" autocomplete="new-password" placeholder=(if editing { "Unchanged" } else { "" }) "data-bind:p_secret";
				small class="ui-hint" { "Write-only: podkit stores it encrypted and never shows it again." }
			}
		}
		label class=(LABEL) {
			"Scopes"
			input class=(INPUT) type="text" autocomplete="off" placeholder="openid email profile" "data-bind:p_scopes";
		}
		div class="ui-check" {
			input id={ "p-jit-" (segment(scope)) } type="checkbox" class=(CHECKBOX) "data-bind:p_jit";
			label for={ "p-jit-" (segment(scope)) } class="cursor-pointer" { "Create an account on first sign-in, when the instance's registration policy allows new accounts" }
		}
		div class="ui-check" {
			input id={ "p-unverified-" (segment(scope)) } type="checkbox" class=(CHECKBOX) "data-bind:p_unverified";
			label for={ "p-unverified-" (segment(scope)) } class="cursor-pointer" { "Trust the provider's email even when it does not say it was verified (Microsoft)" }
		}
		@if matches!(scope, Scope::Team(_)) {
			div class="ui-pair" {
				label class=(LABEL) {
					"Groups claim"
					input class=(INPUT) type="text" autocomplete="off" placeholder="groups" "data-bind:p_claim";
				}
				label class=(LABEL) {
					"Group to role (one per line: group = Role)"
					textarea class=(TEXTAREA) rows="3" placeholder="ops = Owner" "data-bind:p_groups" {}
				}
			}
		}
		div class="ui-check" {
			input id={ "p-enabled-" (segment(scope)) } type="checkbox" class=(CHECKBOX) "data-bind:p_enabled";
			label for={ "p-enabled-" (segment(scope)) } class="cursor-pointer" { "Show it on the sign-in page" }
		}
	}
}

fn signals_for(p: Option<&Provider>, groups: &str) -> Value {
	match p {
		Some(p) => json!({
			"p_kind": p.kind.as_str(), "p_name": p.name, "p_slug": p.slug,
			"p_issuer": p.issuer.clone().unwrap_or_default(), "p_client_id": p.client_id,
			"p_secret": "", "p_scopes": p.scopes, "p_jit": p.jit, "p_unverified": p.allow_unverified_email,
			"p_claim": p.group_claim.clone().unwrap_or_default(), "p_groups": groups,
			"p_enabled": p.enabled, "_error": "",
		}),
		None => json!({
			"p_kind": "oidc", "p_name": "", "p_slug": "", "p_issuer": "", "p_client_id": "",
			"p_secret": "", "p_scopes": "openid email profile", "p_jit": true, "p_unverified": false,
			"p_claim": "", "p_groups": "", "p_enabled": true, "_error": "",
		}),
	}
}

fn add_form(ctx: &TeamCtx, scope: Scope, heading: &str) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!(
			"{}/{}/providers",
			base(ctx.team.id),
			segment(scope)
		))
	);
	html! {
		form "data-on:submit"=(post) data-signals=(signals_for(None, "").to_string()) {
			(card(&html! {
				(card_heading(heading, Some("Register podkit with the provider first; the redirect URL to give it is on the provider's page once you save.")))
				(card_content(&html! { (fields(scope, false)) (form_error()) }))
				(card_footer(&html! {
					button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("plus")) span { "Add provider" } }
				}))
			}))
		}
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> axum::response::Result<Markup, Response> {
	let result: AppResult<Markup> = async {
		let can_edit = ctx.perms.can(Action::Update, Resource::Team);
		let pol = policy::get(&s.db, ctx.user.id, ctx.team.id).await?;
		let without = if ctx.perms.can(Action::Read, Resource::Membership) && pol.required {
			policy::members_without(&s.db, ctx.user.id, ctx.team.id).await?
		} else {
			Vec::new()
		};
		let team_scope = Scope::Team(ctx.team.id);
		let team_providers = if can_edit { providers::list(&s.db, ctx.user.id, team_scope).await? } else { Vec::new() };
		let admin = providers::is_instance_admin(&s.db, ctx.user.id).await?;
		let instance_providers = if admin { providers::list(&s.db, ctx.user.id, Scope::Instance).await? } else { Vec::new() };

		let signals = json!({ "require": pol.required, "grace": pol.grace_days.to_string(), "_error": "" });
		let content = html! {
			div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
				(policy_card(&ctx, &pol, &without, can_edit))
				@if can_edit {
					(provider_list(&ctx, team_scope, &team_providers, "Team providers", "Members of this team can sign in with these. People who sign in through one join the team."))
					(add_form(&ctx, team_scope, "Add a team provider"))
				}
				@if admin {
					(provider_list(&ctx, Scope::Instance, &instance_providers, "Instance providers", "Offered on the sign-in page for everyone on this podkit. Only the administrator, the oldest account, manages them."))
					(add_form(&ctx, Scope::Instance, "Add an instance provider"))
				}
			}
		};
		Ok(app_page(&ctx, "Authentication", &settings_crumbs(&ctx, "Authentication"), &settings_body(&ctx, &content)))
	}
	.await;
	result.map_err(|e| error::page(&ctx, &e))
}

async fn edit_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	Path((_, segment_name, id)): Path<(TeamId, String, AuthProviderId)>,
) -> axum::response::Result<Markup, Response> {
	let result: AppResult<Markup> = async {
		let scope = scope_of(&ctx, &segment_name)?;
		let p = providers::find(&s.db, ctx.user.id, scope, id).await?;
		let groups = match scope {
			Scope::Team(team) => providers::group_roles_text(&s.db, team, &p.group_roles).await?,
			Scope::Instance => String::new(),
		};
		let b = base(ctx.team.id);
		let save = format!("evt.preventDefault(); @post({})", js_string(&format!("{b}/{segment_name}/providers/{id}")));
		let delete = format!("@post({})", js_string(&format!("{b}/{segment_name}/providers/{id}/delete")));
		let redirect = redirect_uri(&s, &headers, p.id);
		let _ = external_base(&s.config, &headers);
		let content = html! {
			div class="flex flex-col gap-6" data-signals=(signals_for(Some(&p), &groups).to_string()) {
				(card(&html! {
					(card_heading("Redirect URL", Some("Give this to the provider as the redirect (callback) URL of the application you made for podkit.")))
					(card_content(&copy_field("sso-redirect", "Redirect URL", &redirect)))
				}))
				form "data-on:submit"=(save) {
					(card(&html! {
						(card_heading(&p.name, Some("Leave the client secret empty to keep the one podkit has.")))
						(card_content(&html! { (fields(scope, true)) }))
					}))
					(save_bar(&["p_name", "p_slug", "p_issuer", "p_client_id", "p_secret", "p_scopes", "p_jit", "p_unverified", "p_claim", "p_groups", "p_enabled"]))
				}
				(card(&html! {
					(card_heading("Remove this provider", Some("People who signed in through it can still sign in with their password. Accounts it created have none, so they would be locked out.")))
					(card_footer(&html! {
						button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(delete) { (icon("trash-2")) span { "Remove provider" } }
					}))
				}))
			}
		};
		Ok(app_page(&ctx, "Authentication", &[("Settings", Some(format!("/teams/{}/settings", ctx.team.id))), ("Authentication", Some(b)), (&p.name, None)], &settings_body(&ctx, &content)))
	}
	.await;
	result.map_err(|e| error::page(&ctx, &e))
}

// --- actions -------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct PolicySignals {
	require: bool,
	grace: String,
}

async fn save_policy(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<PolicySignals>,
) -> Response {
	let result = async {
		let grace = sig.grace.trim().parse::<i32>().map_err(|_| {
			ServerError::Validation("The grace period is a number of days.".to_owned())
		})?;
		policy::set(&s.db, ctx.user.id, ctx.team.id, sig.require, grace).await
	}
	.await;
	match result {
		Ok(_) => {
			shell::flash(
				&cookies,
				Tone::Success,
				"Two-step verification policy saved.",
			);
			sse::signals(&json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&base(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names)]
struct ProviderSignals {
	p_kind: String,
	p_name: String,
	p_slug: String,
	p_issuer: String,
	p_client_id: String,
	p_secret: String,
	p_scopes: String,
	p_jit: bool,
	p_unverified: bool,
	p_claim: String,
	p_groups: String,
	p_enabled: bool,
}

impl ProviderSignals {
	fn input(self, kind: Option<Kind>) -> Result<Input, ServerError> {
		let kind = kind
			.or_else(|| Kind::parse(&self.p_kind))
			.ok_or_else(|| ServerError::Validation("Pick a kind of provider.".to_owned()))?;
		Ok(Input {
			kind,
			name: self.p_name,
			slug: self.p_slug,
			issuer: self.p_issuer,
			client_id: self.p_client_id,
			client_secret: self.p_secret,
			scopes: self.p_scopes,
			jit: self.p_jit,
			allow_unverified_email: self.p_unverified,
			group_claim: self.p_claim,
			group_roles: self.p_groups,
			enabled: self.p_enabled,
		})
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, segment_name)): Path<(TeamId, String)>,
	ReadSignals(sig): ReadSignals<ProviderSignals>,
) -> Response {
	let result = async {
		let scope = scope_of(&ctx, &segment_name)?;
		let input = sig.input(None)?;
		providers::create(&s, ctx.user.id, scope, &input).await
	}
	.await;
	match result {
		Ok(p) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("{} added. Register the redirect URL below with it.", p.name),
			);
			sse::redirect(&format!(
				"{}/{segment_name}/providers/{}",
				base(ctx.team.id),
				p.id
			))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn update(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, segment_name, id)): Path<(TeamId, String, AuthProviderId)>,
	ReadSignals(sig): ReadSignals<ProviderSignals>,
) -> Response {
	let result = async {
		let scope = scope_of(&ctx, &segment_name)?;
		let current = providers::find(&s.db, ctx.user.id, scope, id).await?;
		let input = sig.input(Some(current.kind))?;
		providers::update(&s, ctx.user.id, scope, id, &input).await
	}
	.await;
	match result {
		Ok(p) => {
			shell::flash(&cookies, Tone::Success, &format!("{} saved.", p.name));
			sse::signals(&json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&format!(
					"{}/{segment_name}/providers/{}",
					base(ctx.team.id),
					p.id
				)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn delete(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, segment_name, id)): Path<(TeamId, String, AuthProviderId)>,
) -> Response {
	let result = async {
		let scope = scope_of(&ctx, &segment_name)?;
		providers::delete(&s, ctx.user.id, scope, id).await
	}
	.await;
	match result {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Provider removed.");
			sse::redirect(&base(ctx.team.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}
