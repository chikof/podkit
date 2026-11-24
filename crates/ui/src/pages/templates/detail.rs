//! One template: what it is, what it runs, what it needs, what it asks, and the Deploy
//! button that opens the deploy dialog (`deploy.rs`).
//!
//! The variables list shows the questions the user answers and names the generated ones
//! ("generated"): their values do not exist before a deploy and are never shown on a page.

use axum::extract::{Path, Query, State};
use axum::response::Response;
use maud::{Markup, html};
use serde::Deserialize;

use super::{Place, deploy, gallery};
use crate::components::{
	LogoSize, Priority, Size, Tone, Variant, badge, button_class, card, card_content, card_heading,
	icon, image_logo, table,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::pages::apps::chrome;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{ProjectId, TeamId};
use podkit_core::projects::{self, Project};
use podkit_core::state::AppState;
use podkit_core::templates::{self, Loaded, VariableInfo};

#[derive(Debug, Default, Deserialize)]
pub(super) struct DetailQuery {
	#[serde(default)]
	environment: Option<i64>,
	/// Open the deploy dialog on load.
	#[serde(default)]
	deploy: Option<String>,
}

/// A link a template names, only when it is a web address: the text comes from a file we
/// ship, but a link is only ever `http(s)`.
fn web_link(url: Option<&str>) -> Option<&str> {
	url.filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

fn link_row(label: &str, url: &str) -> Markup {
	html! {
		a class=(button_class(Variant::Outline, Size::Sm)) href=(url) target="_blank" rel="noopener noreferrer" {
			(label) (icon("external-link"))
		}
	}
}

fn services_card(l: &Loaded) -> Markup {
	let rows: Vec<Vec<Markup>> = l
		.services
		.iter()
		.map(|svc| {
			vec![
				html! { span class="font-mono" { (svc.name) } },
				html! { span class="block max-w-64 truncate font-mono text-xs" title=(svc.image) { (svc.image) } },
				html! {
					@if let Some(port) = svc.domain_port { "Public, port " (port) } @else { "Private" }
				},
				html! { @if svc.volumes { "Yes" } @else { "No" } },
			]
		})
		.collect();
	card(&html! {
		(card_heading("Services", Some("What runs when you deploy it. Images come from public registries.")))
		(card_content(&table(
			"tpl-services",
			"Services of this template",
			&["Service", "Image", "Reachable", "Keeps data"],
			&rows,
		)))
	})
}

fn requirements_card(l: &Loaded) -> Markup {
	let t = &l.template;
	let domains = l
		.services
		.iter()
		.filter(|s| s.domain_port.is_some())
		.count();
	let volumes = l.services.iter().any(|s| s.volumes);
	card(&html! {
		(card_heading("Requirements", None))
		(card_content(&html! {
			ul class="flex flex-col gap-2 text-sm" {
				li class="flex items-start gap-2" {
					span class="mt-0.5 text-muted-foreground" { (icon("server")) }
					span {
						@if let Some(ram) = t.requires.ram_mb {
							"A server with at least " strong { (ram) " MB" } " of free memory."
						} @else {
							"Runs on any server; no special memory needs."
						}
					}
				}
				li class="flex items-start gap-2" {
					span class="mt-0.5 text-muted-foreground" { (icon("layers")) }
					span {
						@if l.plain_app {
							"Deployed as one " strong { "application" } "."
						} @else {
							"Deployed as a " strong { "stack" } " of " (l.services.len())
							@if l.services.len() == 1 { " service" } @else { " services" }
							"."
						}
					}
				}
				@if domains > 0 {
					li class="flex items-start gap-2" {
						span class="mt-0.5 text-muted-foreground" { (icon("globe")) }
						span { "A hostname per public service. A free one is made unless you give your own." }
					}
				}
				@if volumes {
					li class="flex items-start gap-2" {
						span class="mt-0.5 text-muted-foreground" { (icon("hard-drive")) }
						span { "Disk for its data volumes. Removing it keeps them unless you ask to delete them." }
					}
				}
				@if t.gateway {
					li class="flex items-start gap-2" {
						span class="mt-0.5 text-muted-foreground" { (icon("brain")) }
						span { "Works with an OpenAI-compatible endpoint, such as one of your models." }
					}
				}
			}
		}))
	})
}

fn variable_row(v: &VariableInfo) -> Markup {
	let title = v.label.as_deref().unwrap_or(&v.name);
	html! {
		li class="flex flex-col gap-1 py-3" data-variable=(v.name) data-kind=(v.kind) {
			div class="flex flex-wrap items-center gap-2" {
				span class="text-sm font-medium" { (title) }
				span class="font-mono text-xs text-muted-foreground" { (v.name) }
				@match v.kind {
					"generated" => {
						(badge(Tone::Neutral, "generated"))
						@if v.revealed { (badge(Tone::Warning, "shown once after deploy")) }
					}
					"domain" => { (badge(Tone::Neutral, "optional")) }
					_ => {
						@if v.required { (badge(Tone::Warning, "required")) } @else { (badge(Tone::Neutral, "optional")) }
						@if v.secret { (badge(Tone::Neutral, "secret")) }
					}
				}
			}
			@if let Some(d) = &v.description {
				p class="text-sm text-muted-foreground" { (d) }
			}
			@if v.kind == "generated" {
				p class="text-sm text-muted-foreground" {
					@if v.revealed { "Made when you deploy. podkit shows it to you once, right after." }
					@else if v.secret { "Made when you deploy and stored encrypted. It is never shown." }
					@else { "Made when you deploy." }
				}
			}
			@if v.kind == "input" && !v.secret {
				@if let Some(default) = &v.default {
					p class="text-sm text-muted-foreground" { "Default " code class="font-mono" { (default) } }
				}
			}
		}
	}
}

fn variables_card(vars: &[VariableInfo]) -> Markup {
	let asked: Vec<&VariableInfo> = vars.iter().filter(|v| v.kind != "generated").collect();
	let generated: Vec<&VariableInfo> = vars.iter().filter(|v| v.kind == "generated").collect();
	card(&html! {
		(card_heading("Variables", Some("What you can set when you deploy. The rest podkit makes for you.")))
		(card_content(&html! {
			@if vars.is_empty() {
				p class="text-sm text-muted-foreground" { "Nothing to fill in. It deploys as it is." }
			}
			@if !asked.is_empty() {
				h3 class="text-xs font-medium text-muted-foreground" { "You choose" }
				ul class="mb-2 flex flex-col" {
					@for v in &asked { (variable_row(v)) }
				}
			}
			@if !generated.is_empty() {
				h3 class="text-xs font-medium text-muted-foreground" { "Generated" }
				ul class="flex flex-col" {
					@for v in &generated { (variable_row(v)) }
				}
			}
		}))
	})
}

fn can_deploy(ctx: &TeamCtx, l: &Loaded) -> bool {
	if l.plain_app {
		ctx.perms.can(Action::Create, Resource::Application)
	} else {
		ctx.perms.can(Action::Create, Resource::Stack)
			&& ctx.perms.can(Action::Update, Resource::Stack)
	}
}

fn crumbs<'a>(
	ctx: &TeamCtx,
	project: Option<&'a Project>,
	name: &'a str,
	place: Place,
) -> Vec<Crumb<'a>> {
	let gallery = Some(place.base());
	match project {
		Some(p) => vec![
			("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
			(
				p.name.as_str(),
				Some(format!("/teams/{}/projects/{}", ctx.team.id, p.id)),
			),
			("Templates", gallery),
			(name, None),
		],
		None => vec![("Templates", gallery), (name, None)],
	}
}

async fn render(
	s: &AppState,
	ctx: &TeamCtx,
	place: Place,
	project: Option<&Project>,
	id: &str,
	query: &DetailQuery,
) -> Result<Markup, Response> {
	let l = templates::find(id).ok_or_else(|| error::page(ctx, &ServerError::NotFound))?;
	let t = &l.template;
	let vars = templates::variables(t);
	let deployable = can_deploy(ctx, l);
	let dialog = if deployable {
		let targets = deploy::load_targets(s, ctx, place)
			.await
			.map_err(|e| error::page(ctx, &e))?;
		Some(deploy::dialog(place, l, &vars, &targets))
	} else {
		None
	};
	let open = deployable && query.deploy.is_some();
	let body = html! {
		div class="flex flex-col gap-6" {
			(chrome::head(
				&image_logo(&podkit_core::logos::template_url(l), LogoSize::Lg, Priority::Normal),
				&html! {
					(icon("layers"))
					"Template \u{b7} " (gallery::category_label(&t.category)) " \u{b7} " (if l.plain_app { "Application" } else { "Stack" })
				},
				&t.name,
				&html! {
					span class="ui-head-desc" { (t.description) }
					@if !t.version.is_empty() { span class="ui-head-ref" { (t.version) } }
				},
				&html! {
					@if deployable {
						div class="ui-app-actions" {
							button type="button" class=(button_class(Variant::Default, Size::Md))
								command="show-modal" commandfor=(deploy::DIALOG_ID) aria-haspopup="dialog"
								"data-on:click"="el.commandForElement || document.getElementById('tpl-deploy').showModal()" {
								(icon("rocket")) "Deploy"
							}
						}
					}
				},
			))
			div class="flex flex-wrap items-center gap-2" {
				@if let Some(url) = web_link(t.website.as_deref()) { (link_row("Website", url)) }
				@if let Some(url) = web_link(t.docs.as_deref()) { (link_row("Documentation", url)) }
				@if let Some(url) = web_link(t.source.as_deref()) { (link_row("Source", url)) }
			}
			@if !deployable {
				p class="text-sm text-muted-foreground" { "Your role in this team can look at templates but not deploy them." }
			}
			(variables_card(&vars))
			(services_card(l))
			(requirements_card(l))
			@if !l.warnings.is_empty() {
				(card(&html! {
					(card_heading("Notes on this template", Some("podkit adjusts or ignores these compose settings when it deploys it.")))
					(card_content(&html! {
						ul class="flex list-disc flex-col gap-1 pl-5 text-sm text-muted-foreground" {
							@for w in &l.warnings { li { (w) } }
						}
					}))
				}))
			}
			@if let Some(dialog) = &dialog { (dialog) }
			@if open {
				div hidden "data-init"="document.getElementById('tpl-deploy')?.showModal()" {}
			}
		}
	};
	Ok(app_page(
		ctx,
		&t.name,
		&crumbs(ctx, project, &t.name, place),
		&body,
	))
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, String)>,
	Query(q): Query<DetailQuery>,
) -> Result<Markup, Response> {
	let place = Place {
		team: ctx.team.id,
		project: None,
		environment: q.environment,
	};
	render(&s, &ctx, place, None, &id, &q).await
}

pub(super) async fn project_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, String)>,
	Query(q): Query<DetailQuery>,
) -> Result<Markup, Response> {
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let place = Place {
		team: ctx.team.id,
		project: Some(project.id),
		environment: q.environment,
	};
	render(&s, &ctx, place, Some(&project), &id, &q).await
}
