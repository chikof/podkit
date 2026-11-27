//! Presets in the web UI: the list, applying one, keeping one from a file, and saving a
//! project as one.
//!
//! A preset is a project somebody else described, so the apply page is a form of what only
//! this team can answer: which of its servers each object goes to, which key and registry,
//! and the secrets. Then the plan as a diff and one confirmed button, as for a project file.
//! Every action goes through [`podkit_core::presets::service`], which authorizes it as the
//! JSON API does.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde_json::{Value, json};

use super::iac::plan_view;
use crate::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, SELECT, Size, Tone, Variant, badge, button_class, card,
	card_content, card_section, dialog, dialog_open_button, icon,
};
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page, form_error};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::{AppResult, ServerError};
use podkit_core::ids::{PresetId, ProjectId, TeamId};
use podkit_core::presets::service::{self, Applied, Asked, NewPreset, Request, Target};
use podkit_core::presets::store::Origin;
use podkit_core::presets::{Bindings, Slot};
use podkit_core::state::AppState;

#[cfg(test)]
mod tests;

const APPLY_DIALOG: &str = "preset-apply";
/// The largest file taken from the form.
const MAX_FILE: usize = 1024 * 1024;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/presets", get(list))
		.route("/teams/{team_id}/presets/keep", post(keep))
		.route(
			"/teams/{team_id}/presets/save/{project_id}",
			get(save_page).post(save),
		)
		.route("/teams/{team_id}/presets/kept/{id}/remove", post(remove))
		.route("/teams/{team_id}/presets/{source}/{id}", get(apply_page))
		.route("/teams/{team_id}/presets/{source}/{id}/plan", post(plan))
		.route("/teams/{team_id}/presets/{source}/{id}/apply", post(apply))
}

fn base(team: TeamId) -> String {
	format!("/teams/{team}/presets")
}

/// A preset as a page shows it, wherever it came from.
struct Shown {
	name: String,
	description: String,
	file: String,
	/// Somebody handed the file in: nobody at podkit or in this team wrote it.
	unofficial: bool,
	/// For a secret to type in, what the catalog says it is for.
	secret_help: std::collections::BTreeMap<String, String>,
}

/// The preset `id` of `source`: `catalog` for the catalog's, `kept` for the team's own.
async fn shown(s: &AppState, ctx: &TeamCtx, source: &str, id: &str) -> AppResult<Shown> {
	match source {
		"catalog" => {
			let p = service::catalog_preset(id)?;
			Ok(Shown {
				name: p.name,
				description: p.description,
				file: p.file,
				unofficial: false,
				secret_help: p.secrets,
			})
		}
		"kept" => {
			let id = id
				.parse()
				.map(PresetId)
				.map_err(|_| ServerError::NotFound)?;
			let p = service::get(s, ctx.user.id, ctx.team.id, id).await?;
			Ok(Shown {
				name: p.name,
				description: p.description,
				file: p.file,
				unofficial: p.origin == Origin::Imported,
				secret_help: std::collections::BTreeMap::new(),
			})
		}
		_ => Err(ServerError::NotFound),
	}
}

fn crumbs<'a>(ctx: &TeamCtx, leaf: Option<&'a str>) -> Vec<Crumb<'a>> {
	match leaf {
		Some(leaf) => vec![("Presets", Some(base(ctx.team.id))), (leaf, None)],
		None => vec![("Presets", None)],
	}
}

// ----- the list

fn row(
	href: &str,
	name: &str,
	description: &str,
	tag: Option<Markup>,
	remove: Option<Markup>,
) -> Markup {
	html! {
		li class="flex items-center justify-between gap-4 py-3" {
			div class="min-w-0" {
				p class="font-medium" {
					(name)
					@if let Some(tag) = tag { " " (tag) }
				}
				@if !description.is_empty() {
					p class="text-sm text-muted-foreground" { (description) }
				}
			}
			div class="flex shrink-0 items-center gap-2" {
				@if let Some(remove) = remove { (remove) }
				a class=(button_class(Variant::Outline, Size::Sm)) href=(href) { "Apply\u{2026}" }
			}
		}
	}
}

const REMOVE_DIALOG: &str = "preset-remove";

/// The button of one kept preset: it says which one, and asks.
fn remove_button(id: PresetId, name: &str) -> Markup {
	let ask = format!(
		"$_remove_id = {}; $_remove_name = {}; document.getElementById({}).showModal()",
		js_string(&id.to_string()),
		js_string(name),
		js_string(REMOVE_DIALOG)
	);
	html! {
		button type="button" class=(button_class(Variant::Ghost, Size::Sm)) aria-haspopup="dialog"
			aria-label={ "Remove " (name) } "data-on:click"=(ask) { "Remove" }
	}
}

fn remove_dialog(team: TeamId) -> Markup {
	let post = format!(
		"@post({} + $_remove_id + '/remove')",
		js_string(&format!("{}/kept/", base(team)))
	);
	dialog(
		REMOVE_DIALOG,
		"Remove this preset?",
		Some("Projects that were made from it stay as they are."),
		&html! { p class="text-sm font-medium" "data-text"="$_remove_name" {} },
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(post)
				"data-indicator:_removing" "data-attr:disabled"="$_removing" { "Remove" }
		},
	)
}

fn keep_signals() -> String {
	json!({ "pk_name": "", "pk_file": [], "_error": "", "_busy": false }).to_string()
}

#[allow(
	clippy::too_many_lines,
	reason = "one markup block: the page is read top to bottom like the page it draws"
)]
async fn list(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let kept = service::list(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let catalog = service::catalog();
	let projects = podkit_core::projects::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or_default();
	let can_keep = ctx.perms.can(Action::Create, Resource::Project);
	let can_remove = ctx.perms.can(Action::Delete, Resource::Project);
	let here = base(ctx.team.id);
	let keep_post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{here}/keep"))
	);
	let body = html! {
		div class="ui-page-body" data-signals=(json!({ "_remove_id": "", "_remove_name": "", "_removing": false }).to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Whole projects" }
					h1 { "Presets" }
					p class="ui-lede" {
						"A preset is a project somebody described: its environments, databases and applications. Apply one and choose which of your servers each part runs on."
					}
				}
			}
			(card(&html! {
				(card_section("From the catalog", Some("Presets that come with the template catalog."), None))
				(card_content(&html! {
					@if catalog.is_empty() {
						p class="text-sm text-muted-foreground" {
							"None yet. They arrive when the administrator downloads or refreshes the template catalog."
						}
					} @else {
						ul class="divide-y" {
							@for p in &catalog {
								(row(&format!("{here}/catalog/{}", p.id), &p.name, &p.description, None, None))
							}
						}
					}
				}))
			}))
			(card(&html! {
				(card_section("This team's", Some("Projects saved as presets here, and preset files handed in."), None))
				(card_content(&html! {
					@if kept.is_empty() {
						p class="text-sm text-muted-foreground" { "None yet." }
					} @else {
						ul class="divide-y" {
							@for p in &kept {
								(row(
									&format!("{here}/kept/{}", p.id),
									&p.name,
									&p.description,
									(p.origin == Origin::Imported).then(|| badge(Tone::Warning, "Unofficial")),
									can_remove.then(|| remove_button(p.id, &p.name)),
								))
							}
						}
					}
				}))
			}))
			@if can_remove && !kept.is_empty() { (remove_dialog(ctx.team.id)) }
			@if can_keep {
				(card(&html! {
					(card_section("Save a project as a preset", Some("You read what will be kept before it is saved."), None))
					(card_content(&html! {
						@if projects.is_empty() {
							p class="text-sm text-muted-foreground" { "This team has no project yet." }
						} @else {
							ul class="flex flex-wrap gap-2" {
								@for p in &projects {
									li {
										a class=(button_class(Variant::Outline, Size::Sm)) href={ (here) "/save/" (p.project.id) } { (p.project.name) }
									}
								}
							}
						}
					}))
				}))
				(card(&html! {
					(card_section("Keep a preset from a file", Some("A podkit.toml with one project. It is marked unofficial: nobody here wrote it."), None))
					(card_content(&html! {
						form class="flex flex-col gap-4" data-signals=(keep_signals()) "data-on:submit"=(keep_post) "data-indicator:_busy" {
							label class=(LABEL) for="pk-name" {
								"Name"
								input id="pk-name" class=(INPUT) type="text" required maxlength="120" "data-bind:pk_name";
							}
							label class=(LABEL) for="pk-file" {
								"File"
								input id="pk-file" class=(INPUT) type="file" required accept=".toml,application/toml,text/plain" "data-bind:pk_file";
							}
							(form_error())
							div class="flex justify-end" {
								button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" { (icon("upload")) "Keep" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(app_page(&ctx, "Presets", &crumbs(&ctx, None), &body))
}

fn uploaded(signals: &HashMap<String, Value>, key: &str) -> AppResult<String> {
	let contents = signals
		.get(key)
		.and_then(|files| files.get(0))
		.and_then(|file| file.get("contents"))
		.and_then(Value::as_str)
		.ok_or_else(|| ServerError::Validation("Choose a file first.".to_owned()))?;
	let bytes = base64::engine::general_purpose::STANDARD
		.decode(contents.trim())
		.map_err(|_| ServerError::Validation("The file could not be read.".to_owned()))?;
	if bytes.len() > MAX_FILE {
		return Err(ServerError::Validation("The file is too large.".to_owned()));
	}
	String::from_utf8(bytes)
		.map_err(|_| ServerError::Validation("The file is not UTF-8 text.".to_owned()))
}

fn text(signals: &HashMap<String, Value>, key: &str) -> String {
	signals
		.get(key)
		.and_then(Value::as_str)
		.unwrap_or_default()
		.trim()
		.to_owned()
}

async fn keep(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(signals): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let kept = async {
		let file = uploaded(&signals, "pk_file")?;
		let new = NewPreset {
			name: text(&signals, "pk_name"),
			slug: None,
			description: String::new(),
		};
		service::import(&s, ctx.user.id, ctx.team.id, &new, file).await
	};
	match kept.await {
		Ok(p) => sse::redirect(&format!("{}/kept/{}", base(ctx.team.id), p.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, PresetId)>,
) -> Response {
	match service::remove(&s, ctx.user.id, ctx.team.id, id).await {
		// The list is the page the button is on: it is loaded again, without the preset.
		Ok(()) => sse::reload().into_response(),
		Err(e) => error::action(&e),
	}
}

// ----- saving a project

fn save_signals(name: &str) -> String {
	json!({ "pv_name": name, "pv_description": "", "_error": "", "_busy": false }).to_string()
}

async fn save_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let file = service::from_project(&s, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let name = podkit_core::iac::doc::parse(&file)
		.ok()
		.and_then(|d| d.projects.into_values().next())
		.map(|p| p.name)
		.unwrap_or_default();
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/save/{project}", base(ctx.team.id)))
	);
	let body = html! {
		div class="ui-page-body" data-signals=(save_signals(&name)) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Presets" }
					h1 { "Save " (name) " as a preset" }
					p class="ui-lede" {
						"This is what will be kept. Read it before you save: a preset is meant to be handed to others."
					}
				}
			}
			(card(&html! {
				(card_section("What is kept", None, None))
				(card_content(&html! {
					ul class="text-sm list-disc pl-5" {
						li { "No secret: each one is a placeholder that whoever applies the preset fills in." }
						li { "No server, SSH key or registry of this team: each reads " code { "${choose}" } "." }
						li { "No hostname, and no application slug of its own: both are unique on an install. A slug is made from the project the preset is applied to." }
					}
					pre class="ui-iac-pre mt-4" tabindex="0" aria-label="The preset file" { (file) }
				}))
			}))
			(card(&html! {
				(card_section("Name it", None, None))
				(card_content(&html! {
					form class="flex flex-col gap-4" "data-on:submit"=(post) "data-indicator:_busy" {
						label class=(LABEL) for="pv-name" {
							"Name"
							input id="pv-name" class=(INPUT) type="text" required maxlength="120" "data-bind:pv_name";
						}
						label class=(LABEL) for="pv-description" {
							"Description (optional)"
							input id="pv-description" class=(INPUT) type="text" maxlength="500" "data-bind:pv_description";
						}
						(form_error())
						div class="flex justify-end" {
							button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" { (icon("check")) "Save as a preset" }
						}
					}
				}))
			}))
		}
	};
	Ok(app_page(
		&ctx,
		"Save as a preset",
		&crumbs(&ctx, Some("Save a project")),
		&body,
	))
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(signals): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let new = NewPreset {
		name: text(&signals, "pv_name"),
		slug: None,
		description: text(&signals, "pv_description"),
	};
	match service::save_project(&s, ctx.user.id, ctx.team.id, project, &new).await {
		Ok(_) => sse::redirect(&base(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

// ----- applying one

/// The signals of the apply form. A slot's name has slashes in it, so the fields are numbered
/// in the order [`service::asked`] gives them, which is the same on every request.
fn apply_signals(asked: &Asked) -> String {
	let mut signals = serde_json::Map::new();
	let only = |have: &[String]| (have.len() == 1).then(|| have[0].clone());
	let pick = |slot: &Slot, have: &[String]| {
		slot.suggested
			.clone()
			.filter(|s| have.contains(s))
			.or_else(|| only(have))
			.unwrap_or_default()
	};
	for (i, slot) in asked.needs.servers.iter().enumerate() {
		signals.insert(
			format!("ps_server_{i}"),
			json!(pick(slot, &asked.available.servers)),
		);
	}
	for (i, slot) in asked.needs.deploy_keys.iter().enumerate() {
		signals.insert(
			format!("ps_key_{i}"),
			json!(pick(slot, &asked.available.ssh_keys)),
		);
	}
	for (i, slot) in asked.needs.registries.iter().enumerate() {
		signals.insert(
			format!("ps_registry_{i}"),
			json!(pick(slot, &asked.available.registries)),
		);
	}
	for i in 0..asked.needs.secrets.len() {
		signals.insert(format!("ps_secret_{i}"), json!(""));
	}
	for (key, value) in [
		("ps_target", json!("new")),
		("ps_slug", json!(asked.slug)),
		("ps_name", json!(asked.name)),
		("ps_deploy", json!(false)),
		("_error", json!("")),
		("_busy", json!(false)),
		("_applying", json!(false)),
	] {
		signals.insert(key.to_owned(), value);
	}
	Value::Object(signals).to_string()
}

/// One picker per place: the object it is for, and the team's own to choose from.
fn pickers(legend: &str, prefix: &str, slots: &[Slot], have: &[String], none: &str) -> Markup {
	html! {
		@if !slots.is_empty() {
			fieldset class="flex flex-col gap-3" {
				legend class="font-medium" { (legend) }
				@if have.is_empty() {
					p class="text-sm text-destructive" role="alert" { (none) }
				}
				@for (i, slot) in slots.iter().enumerate() {
					label class=(LABEL) for={ (prefix) "-" (i) } {
						(slot.at)
						select id={ (prefix) "-" (i) } class=(SELECT) required
							"data-bind"=(format!("ps_{prefix}_{i}")) {
							option value="" { "Choose\u{2026}" }
							@for name in have { option value=(name) { (name) } }
						}
					}
				}
			}
		}
	}
}

fn apply_dialog(post_to: &str, name: &str) -> Markup {
	let post = format!("@post({})", js_string(post_to));
	dialog(
		APPLY_DIALOG,
		&format!("Apply {name}?"),
		Some(
			"It is planned again against what exists now, then applied. A change that fails stops the rest, and what was made before it stays.",
		),
		&html! {
			p class="text-sm" "data-show"="$ps_deploy" style="display: none" {
				"Deploy is on: what the preset makes is deployed."
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Not yet" }
			}
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post)
				"data-indicator:_applying" "data-attr:disabled"="$_applying" { "Apply" }
		},
	)
}

#[allow(
	clippy::too_many_lines,
	reason = "one markup block: the page is read top to bottom like the page it draws"
)]
async fn apply_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, source, id)): Path<(TeamId, String, String)>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let preset = shown(&s, &ctx, &source, &id).await.map_err(fail)?;
	let asked = service::asked(&s, ctx.user.id, ctx.team.id, &preset.file)
		.await
		.map_err(fail)?;
	let projects = podkit_core::projects::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;
	let here = format!("{}/{source}/{id}", base(ctx.team.id));
	let plan_post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{here}/plan"))
	);
	let body = html! {
		div class="ui-page-body" data-signals=(apply_signals(&asked)) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Presets" }
					h1 {
						(preset.name)
						@if preset.unofficial { " " (badge(Tone::Warning, "Unofficial")) }
					}
					@if !preset.description.is_empty() { p class="ui-lede" { (preset.description) } }
				}
			}
			@if preset.unofficial {
				(card(&html! {
					(card_section(
						"This preset came from a file",
						Some("Nobody at podkit or in this team wrote it. Read all of it before you apply it: it says what will run on your servers."),
						None,
					))
					(card_content(&html! {
						pre class="ui-iac-pre" tabindex="0" aria-label="The preset file" { (preset.file) }
					}))
				}))
			}
			(card(&html! {
				(card_section("Where it goes, and what only you can answer", Some("Nothing is made until you apply."), None))
				(card_content(&html! {
					form class="flex flex-col gap-6" "data-on:submit"=(plan_post) "data-indicator:_busy" {
						fieldset class="flex flex-col gap-3" {
							legend class="font-medium" { "Project" }
							label class=(LABEL) for="ps-target" {
								"Apply it to"
								select id="ps-target" class=(SELECT) "data-bind:ps_target" {
									option value="new" { "A new project" }
									@for p in &projects {
										option value=(p.project.id) { "The project " (p.project.name) " (its parts are added, nothing is removed)" }
									}
								}
							}
							div class="flex flex-col gap-3" "data-show"="$ps_target == 'new'" {
								label class=(LABEL) for="ps-name" {
									"Name"
									input id="ps-name" class=(INPUT) type="text" maxlength="120" "data-bind:ps_name";
								}
								label class=(LABEL) for="ps-slug" {
									"Slug"
									input id="ps-slug" class=(INPUT) type="text" maxlength="60" aria-describedby="ps-slug-help" "data-bind:ps_slug";
									span id="ps-slug-help" class="ui-hint" { "Lowercase letters, digits and hyphens. Applications are named after it." }
								}
							}
						}
						(pickers("A server for each part", "server", &asked.needs.servers, &asked.available.servers,
							"This team has no server yet: add one under Servers first."))
						(pickers("An SSH key to clone with", "key", &asked.needs.deploy_keys, &asked.available.ssh_keys,
							"This team has no SSH key yet: add one under Keys first."))
						(pickers("A registry to pull from", "registry", &asked.needs.registries, &asked.available.registries,
							"This team has no saved registry yet: add one under Registries first."))
						@if !asked.needs.secrets.is_empty() {
							fieldset class="flex flex-col gap-3" {
								legend class="font-medium" { "Secrets to fill in" }
								@for (i, name) in asked.needs.secrets.iter().enumerate() {
									label class=(LABEL) for={ "ps-secret-" (i) } {
										(name)
										input id={ "ps-secret-" (i) } class=(INPUT) type="password" required autocomplete="off"
											"data-bind"=(format!("ps_secret_{i}"));
										@if let Some(help) = preset.secret_help.get(name) {
											span class="ui-hint" { (help) }
										}
									}
								}
							}
						}
						@if !asked.needs.generated.is_empty() {
							p class="text-sm text-muted-foreground" {
								"podkit makes up " (asked.needs.generated.len()) " secret(s) for this preset: "
								(asked.needs.generated.join(", ")) "."
							}
						}
						div class="ui-check" {
							input id="ps-deploy" type="checkbox" class=(CHECKBOX) "data-bind:ps_deploy";
							label for="ps-deploy" class="cursor-pointer" { "Deploy after applying" }
						}
						(form_error())
						div class="flex justify-end" {
							button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" { "Plan" }
						}
					}
				}))
			}))
			(plan_view::empty())
			(apply_dialog(&format!("{here}/apply"), &preset.name))
		}
	};
	Ok(app_page(
		&ctx,
		&preset.name,
		&crumbs(&ctx, Some(&preset.name)),
		&body,
	))
}

/// The form as a request: the numbered fields back under the names of their places.
fn request(asked: &Asked, file: String, signals: &HashMap<String, Value>) -> AppResult<Request> {
	let named = |prefix: &str, slots: &[Slot]| {
		slots
			.iter()
			.enumerate()
			.map(|(i, slot)| (slot.at.clone(), text(signals, &format!("ps_{prefix}_{i}"))))
			.collect()
	};
	let target = match text(signals, "ps_target").as_str() {
		"new" => Target::New {
			slug: Some(text(signals, "ps_slug")).filter(|s| !s.is_empty()),
			name: Some(text(signals, "ps_name")).filter(|s| !s.is_empty()),
		},
		id => Target::Project(ProjectId(id.parse().map_err(|_| {
			ServerError::Validation("Choose where the preset goes.".to_owned())
		})?)),
	};
	Ok(Request {
		preset: file,
		answers: Bindings {
			servers: named("server", &asked.needs.servers),
			deploy_keys: named("key", &asked.needs.deploy_keys),
			registries: named("registry", &asked.needs.registries),
			secrets: asked
				.needs
				.secrets
				.iter()
				.enumerate()
				.map(|(i, name)| {
					// A secret keeps its spaces: it is not trimmed like a name.
					let value = signals
						.get(&format!("ps_secret_{i}"))
						.and_then(Value::as_str)
						.unwrap_or_default();
					(name.clone(), value.to_owned())
				})
				.collect(),
		},
		target,
		deploy: signals.get("ps_deploy").and_then(Value::as_bool) == Some(true),
	})
}

async fn asked_request(
	s: &AppState,
	ctx: &TeamCtx,
	source: &str,
	id: &str,
	signals: &HashMap<String, Value>,
) -> AppResult<Request> {
	let preset = shown(s, ctx, source, id).await?;
	let asked = service::asked(s, ctx.user.id, ctx.team.id, &preset.file).await?;
	request(&asked, preset.file, signals)
}

async fn plan(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, source, id)): Path<(TeamId, String, String)>,
	ReadSignals(signals): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let planned = async {
		let request = asked_request(&s, &ctx, &source, &id, &signals).await?;
		service::plan(&s, ctx.user.id, ctx.team.id, &request).await
	};
	match planned.await {
		Ok(plan) => {
			let apply = dialog_open_button(
				APPLY_DIALOG,
				BUTTON,
				&html! { (icon("check")) "Apply\u{2026}" },
			);
			sse::patch(&plan_view::planned(&plan, Some(&apply)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

/// The confirm dialog closes whatever happened; the result is under the form.
fn closing(events: sse::Events) -> Response {
	events
		.and(sse::script(&format!(
			"document.getElementById({})?.close()",
			js_string(APPLY_DIALOG)
		)))
		.into_response()
}

async fn apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, source, id)): Path<(TeamId, String, String)>,
	ReadSignals(signals): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let applied = async {
		let request = asked_request(&s, &ctx, &source, &id, &signals).await?;
		service::apply(&s, ctx.user.id, ctx.team.id, &request).await
	};
	match applied.await {
		Ok(Applied::Refused(plan)) => closing(sse::patch(&plan_view::planned(&plan, None))),
		// The report says which change failed; what ran before it is in the project.
		Ok(Applied::Failed { applied, .. }) => closing(sse::patch(&plan_view::applied(&applied))),
		Ok(Applied::Done { project_id, .. }) => closing(sse::redirect(&format!(
			"/teams/{}/projects/{project_id}",
			ctx.team.id
		))),
		Err(e) => closing(sse::error(&error::message(&e))),
	}
}
