//! The General tab: the application's source, build, port and limits. Someone who may update
//! it gets the form; anyone else gets the same facts as text. Changes apply on the next
//! deploy, and credentials are write-only: the hints say whether one is stored.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde_json::json;

use super::form::{
	AppSignals, build_fields, clear_credentials, deploy_key_field, git_source_fields,
	git_token_hint, identity_fields, image_source_fields, load_keys, prefilled_signals,
	registry_hint, runtime_fields,
};
use super::header::header;
use super::{AppPath, Tab, app_url, load, page, toast_ok};
use crate::applications::service::{self, ApplicationView};
use crate::applications::{BuildStrategy, SourceKind};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::registries::Registry;
use crate::ssh_keys::service::KeyView;
use crate::state::AppState;
use crate::ui::components::{
	Size, Variant, button_class, card, card_content, card_footer, card_section,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	// The application's own page is its deployments; its configuration is this tab.
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/settings",
		get(general_page).post(save),
	)
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

/// The source, build and limits as text, for someone who may not change them.
fn summary(app: &ApplicationView) -> Markup {
	html! {
		(card(&html! {
			(card_section("Source", None, None))
			(card_content(&html! {
				dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
					(fact("Server", &html! { (app.server_name) }))
					@match app.source_kind {
						SourceKind::Image => {
							(fact("Image", &html! { code class="font-mono" { (app.image.as_deref().unwrap_or_default()) } }))
							(fact("Registry login", &html! { @if app.has_registry_auth { "Stored" } @else { "None" } }))
						}
						SourceKind::Git => {
							(fact("Repository", &html! { code class="font-mono break-all" { (app.repo_url.as_deref().unwrap_or_default()) } }))
							(fact("Ref", &html! { code class="font-mono" { (app.git_ref) } }))
							(fact("Source access", &html! {
								@if app.has_deploy_key { "Deploy key stored" }
								@else if app.has_git_token { "Access token stored" }
								@else { "Public repository" }
							}))
						}
					}
				}
			}))
		}))
		@if app.source_kind == SourceKind::Git {
			(card(&html! {
				(card_section("Build", None, None))
				(card_content(&html! {
					dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
						(fact("Strategy", &html! {
							@match app.build_strategy {
								BuildStrategy::Dockerfile => { "Dockerfile (" (app.dockerfile_path) ")" }
								BuildStrategy::Nixpacks => { "Nixpacks" }
							}
						}))
					}
				}))
			}))
		}
		(card(&html! {
			(card_section("Runtime", None, None))
			(card_content(&html! {
				dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
					(fact("Container port", &html! { (app.container_port) }))
					(fact("Memory limit", &html! {
						@if let Some(mb) = app.memory_limit_mb { (mb) " MB" } @else { "No limit" }
					}))
					(fact("CPU limit", &html! {
						@if let Some(cpu) = app.cpu_limit { (cpu) " cores" } @else { "No limit" }
					}))
				}
			}))
		}))
	}
}

fn section(title: &str, description: Option<&str>, fields: &Markup) -> Markup {
	card(&html! {
		(card_section(title, description, None))
		(card_content(&html! { div class="flex flex-col gap-4" { (fields) } }))
	})
}

fn edit_form(
	base: &str,
	app: &ApplicationView,
	keys: &[KeyView],
	can_create_key: bool,
	registries: &[Registry],
	team: TeamId,
) -> Markup {
	let save = format!(
		"evt.preventDefault(); @post({})",
		crate::ui::sse::js_string(&format!("{base}/settings"))
	);
	html! {
		div class="flex flex-col gap-6" data-signals=(prefilled_signals(app)) {
			form class="flex flex-col gap-6" "data-on:submit"=(save) {
				(section(
					"Application",
					Some(&format!("It runs on {}. The server can't be changed.", app.server_name)),
					&identity_fields(None, false),
				))
				@match app.source_kind {
					SourceKind::Image => {
						(section("Image", Some("Changes apply on the next deploy."), &image_source_fields(Some(app), registries, team)))
					}
					SourceKind::Git => {
						(section("Repository", Some("Changes apply on the next deploy."), &git_source_fields(Some(app), keys, can_create_key)))
						(section("Build", None, &build_fields()))
					}
				}
				(section("Runtime", None, &runtime_fields()))
				(card(&html! {
					(card_content(&html! { (form_error()) }))
					(card_footer(&html! {
						button type="submit" class=(button_class(Variant::Default, Size::Md))
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save changes" }
					}))
				}))
			}
		}
	}
}

async fn general_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let content = if ctx.perms.can(Action::Update, Resource::Application) {
		let (keys, can_create_key) = load_keys(&s, &ctx).await;

		let registries = crate::registries::service::list(&s.db, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or_default();
		edit_form(&base, &app, &keys, can_create_key, &registries, ctx.team.id)
	} else {
		html! { div class="flex flex-col gap-6" { (summary(&app)) } }
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Settings, &content).await)
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<AppSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		// The stored kind decides the fields; a client cannot change it.
		let current = service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		let input = sig.input(Some(current.source_kind))?;
		service::update(&s, ctx.user.id, ctx.team.id, project, app, input).await?;
		service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(updated) => {
			let rows = super::recent_deployments(&s, &ctx, &updated).await;
			// Re-read the vault so a key made by this save is in the list, then point the
			// picker at the key the application now uses.
			let (keys, can_create_key) = load_keys(&s, &ctx).await;
			let picked = updated
				.deploy_key_id
				.map(|k| k.0.to_string())
				.unwrap_or_default();
			sse::patch(&header(&s, &ctx, &updated, &rows).await)
				.and(sse::patch(&deploy_key_field(
					Some(&updated),
					&keys,
					can_create_key,
				)))
				.and(sse::patch(&git_token_hint(Some(&updated))))
				.and(sse::patch(&registry_hint(Some(&updated))))
				.and(sse::signals(&clear_credentials()))
				.and(sse::signals(
					&json!({ "_error": "", "deploy_key_id": picked }),
				))
				.and(toast_ok("Settings saved"))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}
