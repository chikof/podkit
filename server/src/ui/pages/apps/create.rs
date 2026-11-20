//! Creating an application: one page with sections, for either source, a Git repository or a
//! Docker image. A Configure surface: low decoration, validation next to the field
//! (`:user-invalid`) and the server's reasons in one line above the buttons.
//!
//! The webhook secret exists in plaintext only in the response to the create request; the
//! card that shows it is the only place it ever appears.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::form::{
	AppSignals, blank_signals, build_fields, clear_credentials, git_source_fields, identity_fields,
	image_source_fields, load_keys, runtime_fields,
};
use super::{app_url, crumbs, origin, project_url};
use crate::applications::SourceKind;
use crate::applications::service::{self, Created};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::git_providers;
use crate::ids::{EnvironmentId, ProjectId, ServerId, TeamId};
use crate::projects;
use crate::ssh_keys::service::{self as key_service, KeyView};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_OUTLINE, INPUT, Size, TabItem, Variant, button_class, card, card_content, card_footer,
	card_section, copy_field, icon, link_button, tabs,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::pages::git_providers::picker::provider_source_fields;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/projects/{project_id}/apps", post(create))
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/new",
			get(new_page),
		)
}

#[derive(Deserialize, Default)]
struct NewQuery {
	environment: Option<i64>,
	#[serde(default)]
	source: String,
}

fn field_group(title: &str, description: Option<&str>, fields: &Markup) -> Markup {
	card(&html! {
		(card_section(title, description, None))
		(card_content(&html! { div class="flex flex-col gap-4" { (fields) } }))
	})
}

#[allow(
	clippy::too_many_lines,
	reason = "one page: the tabs, the fields and the form"
)]
async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers = service::servers_for_create(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;
	let image = q.source == "image";
	// "Git provider" is offered to someone who may read providers; the URL source stays.
	let provider =
		!image && q.source == "provider" && ctx.perms.can(Action::Read, Resource::GitProvider);
	let source = if image {
		"image"
	} else if provider {
		"provider"
	} else {
		"git"
	};
	let environment = q.environment.map(|e| e.to_string()).unwrap_or_default();
	let back = if environment.is_empty() {
		project_url(ctx.team.id, project.id)
	} else {
		format!(
			"{}?environment={environment}",
			project_url(ctx.team.id, project.id)
		)
	};

	let switch = |kind: &str| {
		let mut href = format!(
			"{}/apps/new?source={kind}",
			project_url(ctx.team.id, project.id)
		);
		if !environment.is_empty() {
			href.push_str("&environment=");
			href.push_str(&environment);
		}
		href
	};
	let (git_href, provider_href, image_href) =
		(switch("git"), switch("provider"), switch("image"));
	let mut items = vec![TabItem {
		label: "Git repository",
		href: &git_href,
		current: !image && !provider,
	}];
	if ctx.perms.can(Action::Read, Resource::GitProvider) {
		items.push(TabItem {
			label: "Git provider",
			href: &provider_href,
			current: provider,
		});
	}
	items.push(TabItem {
		label: "Docker image",
		href: &image_href,
		current: image,
	});
	let source_tabs = tabs("Source type", &items);

	let post = format!(
		"evt.preventDefault(); @post({})",
		crate::ui::sse::js_string(&format!("{}/apps", project_url(ctx.team.id, project.id)))
	);
	let (keys, can_create_key) = load_keys(&s, &ctx).await;
	let providers = if provider {
		git_providers::service::list(&s, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or_default()
	} else {
		Vec::new()
	};
	let source_fields = if image {
		html! { (image_source_fields(None)) }
	} else if provider {
		html! { (provider_source_fields(ctx.team.id, &providers)) }
	} else {
		html! { (git_source_fields(None, &keys, can_create_key)) }
	};
	let body = html! {
		div class="flex flex-col gap-6" data-signals=(blank_signals(&servers, source, &environment)) {
			div class="flex flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "New application" }
				p class="text-sm text-muted-foreground" {
					@if image { "An image from a registry, pulled and run as a container on one of your servers." }
					@else if provider { "A repository from a connected git provider, built and run as a container on one of your servers." }
					@else { "A git repository, built and run as a container on one of your servers." }
				}
			}
			(source_tabs)
			div id="app-form" {
				form class="flex flex-col gap-6" "data-on:submit"=(post) {
					(field_group("Application", None, &identity_fields(Some(&servers), true)))
					@if image {
						(field_group("Image", Some("What to run. podkit pulls it when you deploy."), &source_fields))
					} @else if provider {
						(field_group("Repository", Some("It is cloned with the provider's access, so no token or deploy key is needed."), &source_fields))
						(field_group("Build", None, &build_fields()))
					} @else {
						(field_group("Repository", Some("Where the code lives. A private repository needs a credential for its URL."), &source_fields))
						(field_group("Build", None, &build_fields()))
					}
					(field_group("Runtime", Some("Changes apply on the next deploy."), &runtime_fields()))
					(form_error())
					div class="flex flex-wrap items-center justify-end gap-2" {
						(link_button(Variant::Outline, Size::Md, &back, &html! { "Cancel" }))
						button type="submit" class=(button_class(Variant::Default, Size::Md))
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create application" }
					}
				}
			}
		}
	};
	Ok(app_page(
		&ctx,
		"New application",
		&crumbs(&ctx, &project, "New application"),
		&body,
	))
}

/// A read-only field with a button that copies its value. `execCommand` rather than the
/// Clipboard API, which browsers withhold over plain HTTP.
pub(super) fn copy_row(label: &str, value: &str) -> Markup {
	html! {
		div class="flex flex-col gap-2 text-sm font-medium" {
			span { (label) }
			div class="flex gap-2" {
				input class={ (INPUT) " font-mono" } type="text" readonly value=(value) autocomplete="off" spellcheck="false" aria-label=(label) "data-on:focus"="el.select()";
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"="el.previousElementSibling.select(); document.execCommand('copy')" { (icon("copy")) "Copy" }
			}
		}
	}
}

/// Shown once, in the response to the create request. Nothing here can be fetched again: the
/// secret exists in plaintext only in this stream.
fn created_card(ctx: &TeamCtx, origin: &str, created: &Created, key: Option<&KeyView>) -> Markup {
	let app = &created.app;
	let webhook = format!("{origin}/webhooks/deploy/{}", app.id);
	// Through a git provider podkit makes the webhook itself; the secret is only needed to
	// make one by hand, when that did not work.
	let by_provider = app.git_provider_id.is_some();
	let by_hand = !by_provider || created.notice.is_some();
	html! {
		div id="app-form" {
			(card(&html! {
				(card_section("Application created", Some(if by_provider && !by_hand {
					"podkit made the push webhook at your git provider: a push to the branch deploys it."
				} else if app.source_kind == SourceKind::Git {
					"Add the webhook to your git provider to deploy on every push."
				} else {
					"Call the webhook from your registry or CI to redeploy when the image changes."
				}), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						@if let Some(notice) = &created.notice {
							div class="ui-banner ui-banner-warn" role="status" { (icon("triangle-alert")) div class="ui-banner-body" { (notice) } }
						}
						@if by_hand {
						(copy_row("Webhook URL", &webhook))
						(copy_row("Webhook secret", &created.webhook_secret))
						@if let Some(key) = key {
							(copy_field("deploy-key-public", &format!("Deploy key: public key of {}", key.name), &key.public_key))
							p class="text-sm text-muted-foreground" { "Add it as a deploy key at your git provider before the first deploy. Read access is enough." }
						}
						div class="ui-banner ui-banner-warn" role="status" { (icon("triangle-alert")) div class="ui-banner-body" { b { "Copy the secret now." } " It is shown only this once and can't be recovered." } }
						p class="text-sm text-muted-foreground" {
							"Send it as the GitHub signature secret, or in an "
							code class="font-mono" { "X-Gitlab-Token" } " or " code class="font-mono" { "X-Webhook-Secret" } " header."
						}
						}
					}
				}))
				(card_footer(&html! {
					a class=(button_class(Variant::Default, Size::Md)) href=(app_url(ctx.team.id, app.project_id, app.id)) { "Go to " (app.name) }
				}))
			}))
		}
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<AppSignals>,
) -> Response {
	let result = async {
		let server = sig
			.server_id
			.parse::<i64>()
			.map(ServerId)
			.map_err(|_| ServerError::Validation("Pick a server.".to_owned()))?;
		let mut input = sig.input(None)?;
		input.environment_id = sig
			.environment
			.trim()
			.parse::<i64>()
			.ok()
			.map(EnvironmentId);
		service::create(&s, ctx.user.id, ctx.team.id, project, server, input).await
	}
	.await;
	// A key made or picked for this application: its public half goes on the created card.
	let key = match &result {
		Ok(created) => match created.app.deploy_key_id {
			Some(id) => key_service::get(&s.db, ctx.user.id, ctx.team.id, id)
				.await
				.ok(),
			None => None,
		},
		Err(_) => None,
	};
	match result {
		Ok(created) => sse::patch(&created_card(
			&ctx,
			&origin(&headers),
			&created,
			key.as_ref(),
		))
		.and(sse::signals(&clear_credentials()))
		.and(sse::signals(&json!({ "_error": "" })))
		.into_response(),
		Err(e) => error::action(&e),
	}
}
