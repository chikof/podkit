//! The set-up of one catalog model: model, server, recipe with its fit, overrides. The
//! same content is a stepped modal dialog on the catalog (`?dialog=1`, answered as a
//! Datastar patch that also opens it) and a plain page at the card's link, so it works
//! without script and a set-up can be linked to.
//!
//! The last step posts to `downloads/recipe` (see `flow`), which starts the download and
//! moves on to its progress page and then the start form.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use proto::Hardware;

use super::catalog::{CatalogQuery, View, tier_label, with_query};
use super::{
	catalog_url, crumbs, family_logo, fit_badge, flow_steps, human_bytes, models_url, params_text,
};
use crate::auth::permission::{Action, Resource};
use crate::catalog::service::{self as catalog_service, TeamServer};
use crate::catalog::{self, Fit, Model, Recipe, suggest};
use crate::error::ServerError;
use crate::ids::TeamId;
use crate::models::engines;
use crate::projects;
use crate::projects::service::ProjectSummary;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_OUTLINE, Current, INPUT, LABEL, LogoSize, SELECT, Size, Variant, button_class, icon,
	stepper,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/models/catalog/{catalog_id}", get(page))
}

/// Everything one render needs.
struct Setup<'a> {
	ctx: &'a TeamCtx,
	model: &'a Model,
	servers: &'a [TeamServer],
	view: &'a View,
	projects: &'a [ProjectSummary],
	rated: Vec<(Recipe, Option<Fit>)>,
	can_download: bool,
	/// Dialog: one step shows at a time. Page: all of them, in order.
	stepped: bool,
	/// The 1-based step the dialog opens on.
	start: u8,
}

fn hardware_line(hw: Option<&Hardware>) -> String {
	let Some(hw) = hw else {
		return "This server hasn't reported its hardware yet, so fit can't be rated.".to_owned();
	};
	let gpu = hw.gpus.first().map_or_else(
		|| "no GPU".to_owned(),
		|g| format!("{} with {}", g.name, human_bytes(g.vram_mb * 1_000_000)),
	);
	format!(
		"{gpu} · {} RAM · {} CPU threads",
		human_bytes(hw.memory.total_mb * 1_000_000),
		hw.cpu.threads
	)
}

fn recipe_row(recipe: &Recipe, fit: Option<&Fit>, first: bool, stepped: bool) -> Markup {
	let engine = fit.and_then(|f| f.engine).unwrap_or(recipe.engine);
	html! {
		li {
			label class="ui-option text-sm" {
				div class="flex flex-wrap items-center justify-between gap-2" {
					span class="flex items-center gap-2 font-medium" {
						input type="radio" name="recipe" value=(recipe.id) "data-bind:recipe" autofocus[first && stepped];
						(recipe.quant) " · " (engines::spec(engine).label)
					}
					@if let Some(fit) = fit { (fit_badge(fit)) }
				}
				p class="text-muted-foreground" {
					"Download " (human_bytes(recipe.total_bytes))
					" · needs about " (human_bytes(recipe.est_ram_mb.max(recipe.est_vram_mb) * (1 << 20)))
					@if recipe.verified { " · verified on podkit" }
				}
				@if let Some(fit) = fit { p class="text-muted-foreground" { (fit.explanation) } }
				@if let Some(note) = &recipe.note { p class="text-muted-foreground" { (note) } }
			}
		}
	}
}

impl Setup<'_> {
	fn base(&self) -> String {
		format!("{}/{}", catalog_url(self.ctx.team.id), self.model.id)
	}

	/// The query that makes the server pickers and links remember project and environment.
	fn target_pairs(&self) -> Vec<String> {
		self.view.target.pairs()
	}

	fn panel(&self, n: u8, title: &str, body: &Markup, nav: &Markup) -> Markup {
		let id = format!("setup-step-{n}");
		let heading = format!("{id}-title");
		html! {
			section id=(id) class="flex flex-col gap-4 outline-hidden" tabindex="-1" aria-labelledby=(heading)
				"data-show"=[self.stepped.then(|| format!("$step == {n}"))]
				style=[(self.stepped && n != self.start).then_some("display: none")] {
				h3 id=(heading) class="text-base font-semibold" {
					@if self.stepped { span class="text-muted-foreground" { (n) " of 4 · " } }
					(title)
				}
				(body)
				@if self.stepped { div class="flex flex-wrap justify-between gap-2" { (nav) } }
			}
		}
	}

	fn go(to: u8) -> String {
		format!(
			"$step = {to}; setTimeout(() => document.getElementById('setup-step-{to}')?.focus(), 0)"
		)
	}

	fn next(&self, from: u8) -> Markup {
		html! {
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(Self::go(from + 1))
				autofocus[from == self.start] { "Next" (icon("arrow-right")) }
		}
	}

	fn back(from: u8) -> Markup {
		html! {
			button type="button" class=(button_class(Variant::Ghost, Size::Md)) "data-on:click"=(Self::go(from - 1)) {
				(icon("arrow-left")) "Back"
			}
		}
	}

	fn step_model(&self) -> Markup {
		let m = self.model;
		self.panel(
			1,
			"Model",
			&html! {
				div class="flex items-center gap-4" {
					(family_logo(&m.family, LogoSize::Lg))
					div class="flex min-w-0 flex-col gap-1" {
						p class="text-base font-semibold" { (m.name) }
						p class="text-sm text-muted-foreground" {
							(m.family) " · " (params_text(m.params_b, m.active_params_b)) " · " (m.license) " · " (tier_label(m.tier))
						}
					}
				}
				p class="mt-3 text-sm text-muted-foreground" {
					"The next steps pick the server it runs on and how it is quantized, then download the weights and start it."
				}
			},
			&html! { span {} (self.next(1)) },
		)
	}

	fn server_picker(&self) -> Markup {
		let hw = self.view.hardware(self.servers);
		let prefix = with_query(&self.base(), &{
			let mut parts = vec!["dialog=1".to_owned(), "step=3".to_owned()];
			parts.extend(self.target_pairs());
			parts
		});
		let joiner = if prefix.contains('?') {
			"&server="
		} else {
			"?server="
		};
		html! {
			@if self.servers.is_empty() {
				p class="text-sm text-muted-foreground" { "This team has no server yet." }
			} @else if self.stepped {
				div class=(LABEL) {
					label for="setup-server" { "Server" }
					select id="setup-server" class=(SELECT) "data-bind:server"
						"data-on:change"={ "@get(" (js_string(&format!("{prefix}{joiner}"))) " + el.value)" } {
						@for srv in self.servers {
							option value=(srv.id.0) selected[self.view.server == Some(srv.id)] { (srv.name) }
						}
					}
				}
				p class="text-sm text-muted-foreground" { (hardware_line(hw)) }
			} @else {
				form method="get" action=(self.base()) class="flex flex-col gap-3" {
					@for pair in self.target_pairs() {
						@if let Some((k, v)) = pair.split_once('=') { input type="hidden" name=(k) value=(v); }
					}
					label class=(LABEL) {
						"Server"
						select class=(SELECT) name="server" "data-on:change"="el.form.requestSubmit()" {
							@for srv in self.servers {
								option value=(srv.id.0) selected[self.view.server == Some(srv.id)] { (srv.name) }
							}
						}
					}
					noscript { button type="submit" class=(BUTTON_OUTLINE) { "Rate against this server" } }
				}
				p class="text-sm text-muted-foreground" { (hardware_line(hw)) }
			}
		}
	}

	fn step_server(&self) -> Markup {
		self.panel(
			2,
			"Server",
			&self.server_picker(),
			&html! { (Self::back(2)) (self.next(2)) },
		)
	}

	fn step_recipe(&self) -> Markup {
		self.panel(
			3,
			"Recipe",
			&html! {
				p class="text-sm text-muted-foreground" { "What to download and how to run it, best fit first." }
				ul class="flex flex-col gap-3" {
					@for (i, (recipe, fit)) in self.rated.iter().enumerate() {
						(recipe_row(recipe, fit.as_ref(), i == 0, self.stepped))
					}
				}
			},
			&html! { (Self::back(3)) (self.next(3)) },
		)
	}

	fn step_overrides(&self) -> Markup {
		self.panel(
			4,
			"Overrides",
			&html! {
				p class="text-sm text-muted-foreground" { "Optional. The defaults are fine for most models." }
				div class="flex flex-col gap-4" {
					label class=(LABEL) {
						"Project"
						select class=(SELECT) required "data-bind:project" {
							@for p in self.projects { option value=(p.project.id.0) { (p.project.name) } }
						}
					}
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
					}
					label class=(LABEL) {
						"Name clients send as `model` (optional)"
						input class=(INPUT) type="text" maxlength="100" placeholder="defaults to the slug" "data-bind:served";
					}
					label class=(LABEL) {
						"Context size (tokens)"
						input class=(INPUT) type="number" min="256" max="1048576" "data-bind:ctx";
					}
				}
				(form_error())
			},
			&html! {
				(Self::back(4))
				@if self.can_download {
					(Self::download_button())
				}
			},
		)
	}

	fn download_button() -> Markup {
		html! {
			button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy"
				"data-attr:disabled"="$_busy || !$server" { (icon("download")) "Download" }
		}
	}

	fn signals(&self) -> String {
		let first = self
			.rated
			.first()
			.map(|(r, _)| r.id.clone())
			.unwrap_or_default();
		let project = self.view.target.project_id().map(|p| p.to_string());
		let project = project
			.filter(|p| {
				self.projects
					.iter()
					.any(|s| s.project.id.0.to_string() == *p)
			})
			.or_else(|| self.projects.first().map(|p| p.project.id.to_string()))
			.unwrap_or_default();
		format!(
			"{{step: {}, catalog: {}, recipe: {}, server: {}, project: {}, environment: {}, name: {}, served: '', ctx: '8192', _error: ''}}",
			self.start,
			js_string(&self.model.id),
			js_string(&first),
			js_string(&self.view.server.map_or(String::new(), |s| s.to_string())),
			js_string(&project),
			js_string(
				&self
					.view
					.target
					.environment_id()
					.map_or(String::new(), |e| e.to_string())
			),
			js_string(&self.model.name),
		)
	}

	fn download_post(&self) -> String {
		format!(
			"evt.preventDefault(); @post({})",
			js_string(&format!(
				"{}/downloads/recipe",
				models_url(self.ctx.team.id)
			))
		)
	}

	/// The page's download form: recipe and overrides, then the buttons. (The server step
	/// is a GET form of its own above it, so the two never nest.)
	fn form(&self) -> Markup {
		html! {
			form class="flex flex-col gap-8" "data-on:submit"=(self.download_post()) {
				(self.step_recipe())
				(self.step_overrides())
				div class="flex flex-wrap gap-3" {
					@if self.can_download { (Self::download_button()) }
					a class=(button_class(Variant::Outline, Size::Md)) href=(with_query(&catalog_url(self.ctx.team.id), &self.view.carry())) { "Back to the catalog" }
				}
			}
		}
	}

	/// The dialog's content: heading, stepper, and the four steps in one form.
	fn dialog_content(&self) -> Markup {
		html! {
			div id="setup-body" class="flex flex-col gap-5 p-6" "data-signals"=(self.signals()) {
				div class="flex flex-col gap-1.5 pr-8" {
					h2 id="setup-title" class="text-lg leading-none font-semibold" { "Set up " (self.model.name) }
					p class="text-sm text-muted-foreground" { "Download the weights to a server, then start the engine." }
				}
				(stepper("Set-up steps", &flow_steps(), &Current::Signal("step")))
				div class="ui-setup-panels" {
					form class="flex flex-col gap-6" "data-on:submit"=(self.download_post()) {
						(self.step_model())
						(self.step_server())
						(self.step_recipe())
						(self.step_overrides())
					}
				}
			}
		}
	}

	fn page_content(&self) -> Markup {
		let m = self.model;
		html! {
			div class="ui-page-body" "data-signals"=(self.signals()) {
				header class="ui-page-head" {
					div class="ui-head-id" {
						(family_logo(&m.family, LogoSize::Lg))
						div class="ui-page-title" {
							p class="ui-eyebrow" { "Catalog" }
							h1 { (m.name) }
							p class="ui-lede" {
								(m.family) " · " (params_text(m.params_b, m.active_params_b)) " · " (m.license) " · " (tier_label(m.tier))
							}
						}
					}
				}
				(self.step_server())
				(self.form())
			}
		}
	}
}

/// Loads what both renders need and builds the `Setup`.
struct Loaded {
	servers: Vec<TeamServer>,
	projects: Vec<ProjectSummary>,
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, catalog_id)): Path<(TeamId, String)>,
	Query(q): Query<CatalogQuery>,
) -> Response {
	let dialog = q.dialog.as_deref() == Some("1");
	let fail = |e: &ServerError| {
		if dialog {
			error::action(e)
		} else {
			error::page(&ctx, e)
		}
	};
	let Some(model) = catalog::model(&catalog_id) else {
		return fail(&ServerError::ModelNotFound);
	};
	let loaded = async {
		Ok::<_, ServerError>(Loaded {
			servers: catalog_service::team_servers(&s, ctx.user.id, ctx.team.id).await?,
			projects: projects::service::list(&s.db, ctx.user.id, ctx.team.id).await?,
		})
	}
	.await;
	let Loaded { servers, projects } = match loaded {
		Ok(l) => l,
		Err(e) => return fail(&e),
	};
	let view = View::from_query(&q, &servers);
	let rated: Vec<(Recipe, Option<Fit>)> = match view.hardware(&servers) {
		Some(hw) => suggest(hw, model)
			.into_iter()
			.map(|(r, f)| (r, Some(f)))
			.collect(),
		None => model.recipes.iter().cloned().map(|r| (r, None)).collect(),
	};
	let setup = Setup {
		ctx: &ctx,
		model,
		servers: &servers,
		view: &view,
		projects: &projects,
		rated,
		can_download: ctx.perms.can(Action::Create, Resource::ModelFile)
			&& ctx.perms.can(Action::Create, Resource::Model),
		stepped: dialog,
		start: q
			.step
			.as_deref()
			.and_then(|s| s.parse::<u8>().ok())
			.filter(|n| (1..=4).contains(n))
			.unwrap_or(1),
	};
	if dialog {
		return sse::patch(&setup.dialog_content())
			.and(sse::script(
				"const d = document.getElementById('setup-dialog'); if (!d.open) d.showModal(); \
				document.querySelector('#setup-body [autofocus]')?.focus()",
			))
			.into_response();
	}
	let back = view.url(ctx.team.id, |_| {});
	let body = setup.page_content();
	app_page(
		&ctx,
		&model.name,
		&crumbs(
			&ctx,
			vec![("Catalog", Some(back)), (model.name.as_str(), None)],
		),
		&body,
	)
	.into_response()
}
