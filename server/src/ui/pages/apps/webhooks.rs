//! The Webhooks tab: the URL a git provider (or, for an image application, a registry or CI
//! job) calls to deploy, and rotating its
//! secret.
//!
//! The secret is shown once, in the response to the request that created or rotated it, and
//! never again: this page holds the URL and the instructions, not the secret. Rotating
//! replaces it at once, so whatever still sends the old one is refused until it is updated.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};

use super::create::copy_row;
use super::{Tab, app_url, close_dialog, closer, forbid_unless_allowed, load, origin, page};
use crate::applications::SourceKind;
use crate::applications::service;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	Dismiss, Size, Variant, button_class, card, card_content, card_section, dialog, dialog_cancel,
	dialog_open_button, icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/webhooks",
			get(webhooks_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/webhooks/rotate",
			post(rotate),
		)
}

/// Where the new secret lands after a rotation. Empty until then.
fn secret_slot(secret: Option<&str>) -> Markup {
	html! {
		div id="webhook-secret" aria-live="polite" {
			@if let Some(secret) = secret {
				div class="flex flex-col gap-3" {
					(copy_row("New webhook secret", secret))
					p class="text-sm text-destructive" { "Copy the secret now. It is shown only this once and can't be recovered." }
				}
			}
		}
	}
}

async fn webhooks_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Webhooks)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let url = format!("{}/webhooks/deploy/{}", origin(&headers), app.id);
	let rotate = format!("@post({})", js_string(&format!("{base}/webhooks/rotate")));
	// A git application is deployed by a push; an image application by a new image, pushed
	// from a registry or a CI job. Neither is told about the other's world.
	let git = app.source_kind == SourceKind::Git;
	let (title, lead, caller) = if git {
		(
			"Deploy on push",
			"Call this URL from your git provider and podkit deploys the application on every push.",
			"Your git provider",
		)
	} else {
		(
			"Deploy on a new image",
			"Call this URL after you push a new image, from your registry or a CI job, and podkit redeploys the application.",
			"Whatever calls this URL",
		)
	};
	let rotate_lead = format!(
		"The current secret stops working at once. {caller} is refused until you give it the new one."
	);
	let content = html! {
		div class="flex flex-col gap-6" data-signals="{_rotating: false, _error: ''}" {
			(card(&html! {
				(card_section(title, Some(lead), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						(copy_row("Webhook URL", &url))
						ul class="flex list-disc flex-col gap-1 pl-5 text-sm text-muted-foreground" {
							@if git {
								li { "GitHub: add a webhook with content type " code class="font-mono" { "application/json" } " and the secret below." }
								li { "GitLab: add a webhook and put the secret in " code class="font-mono" { "Secret token" } " (sent as " code class="font-mono" { "X-Gitlab-Token" } ")." }
								li { "Anything else: send the secret in an " code class="font-mono" { "X-Webhook-Secret" } " header." }
							} @else {
								li { "Send the secret in an " code class="font-mono" { "X-Webhook-Secret" } " header." }
							}
						}
					}
				}))
			}))
			(card(&html! {
				(card_section(
					"Secret",
					Some("The secret was shown once, when the application was created. It can't be shown again, but you can replace it."),
					Some(&dialog_open_button(
						"webhook-rotate",
						&button_class(Variant::Outline, Size::Sm),
						&html! { (icon("refresh-cw")) "Rotate secret" },
					)),
				))
				(card_content(&secret_slot(None)))
			}))
			(dialog(
				"webhook-rotate",
				"Rotate the webhook secret?",
				Some(rotate_lead.as_str()),
				Dismiss::Esc,
				&html! {
					(closer("webhook-rotate"))
					p class="text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
				},
				&html! {
					(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Keep the current secret" }))
					button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(rotate)
						"data-indicator:_rotating" "data-attr:disabled"="$_rotating" { "Rotate secret" }
				},
			))
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Webhooks, &content).await)
}

async fn rotate(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match service::rotate_webhook_secret(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(secret) => sse::patch(&secret_slot(Some(&secret)))
			.and(close_dialog("webhook-rotate"))
			.and(sse::signals(&serde_json::json!({ "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}
