//! `/teams/{t}/models/catalog`: model cards by tier with a server's fit on
//! each, the recipes of one model, and the Hugging Face search.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use proto::Hardware;
use serde::Deserialize;

use super::{catalog_url, crumbs, fit_badge, human_bytes, models_url, params_text};
use crate::auth::permission::{Action, Resource};
use crate::catalog::search::Hit;
use crate::catalog::service::{self as catalog_service, TeamServer};
use crate::catalog::{self, Model, Recipe, SizeClass, Tag, Tier, suggest};
use crate::error::ServerError;
use crate::ids::{ServerId, TeamId};
use crate::models::engines;
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, INPUT, LABEL, Tone, card, card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/models/catalog", get(page))
		.route("/teams/{team_id}/models/catalog/search", get(search))
		.route("/teams/{team_id}/models/catalog/{catalog_id}", get(setup))
}

#[derive(Debug, Default, Deserialize)]
struct CatalogQuery {
	server: Option<String>,
	size: Option<String>,
	tag: Option<String>,
	tab: Option<String>,
}

/// What the page filters and rates against, kept so links can carry it on.
#[derive(Debug, Clone, Default)]
struct View {
	server: Option<ServerId>,
	size: Option<SizeClass>,
	tag: Option<Tag>,
	advanced: bool,
}

fn parse_size(s: &str) -> Option<SizeClass> {
	match s {
		"tiny" => Some(SizeClass::Tiny),
		"small" => Some(SizeClass::Small),
		"medium" => Some(SizeClass::Medium),
		"large" => Some(SizeClass::Large),
		_ => None,
	}
}

fn size_key(s: SizeClass) -> &'static str {
	match s {
		SizeClass::Tiny => "tiny",
		SizeClass::Small => "small",
		SizeClass::Medium => "medium",
		SizeClass::Large => "large",
	}
}

fn size_label(s: SizeClass) -> &'static str {
	match s {
		SizeClass::Tiny => "Tiny",
		SizeClass::Small => "Small",
		SizeClass::Medium => "Medium",
		SizeClass::Large => "Large",
	}
}

fn size_hint(s: SizeClass) -> &'static str {
	match s {
		SizeClass::Tiny => "under 8B",
		SizeClass::Small => "8B to 16B",
		SizeClass::Medium => "16B to 40B",
		SizeClass::Large => "40B and up",
	}
}

fn parse_tag(s: &str) -> Option<Tag> {
	match s {
		"coding" => Some(Tag::Coding),
		"reasoning" => Some(Tag::Reasoning),
		"vision" => Some(Tag::Vision),
		_ => None,
	}
}

fn tag_key(t: Tag) -> &'static str {
	match t {
		Tag::Coding => "coding",
		Tag::Reasoning => "reasoning",
		Tag::Vision => "vision",
	}
}

fn tag_label(t: Tag) -> &'static str {
	match t {
		Tag::Coding => "Coding",
		Tag::Reasoning => "Reasoning",
		Tag::Vision => "Vision",
	}
}

fn tier_label(t: Option<Tier>) -> &'static str {
	match t {
		Some(Tier::S) => "S tier",
		Some(Tier::A) => "A tier",
		Some(Tier::B) => "B tier",
		Some(Tier::C) => "C tier",
		Some(Tier::D) => "D tier",
		None => "Not ranked",
	}
}

const TIERS: [Option<Tier>; 6] = [
	Some(Tier::S),
	Some(Tier::A),
	Some(Tier::B),
	Some(Tier::C),
	Some(Tier::D),
	None,
];

impl View {
	fn from_query(q: &CatalogQuery, servers: &[TeamServer]) -> Self {
		let requested = q
			.server
			.as_deref()
			.and_then(|s| s.parse::<i64>().ok())
			.map(ServerId)
			.filter(|id| servers.iter().any(|s| s.id == *id));
		// With no choice made, rate against the first server that has reported.
		let server =
			requested.or_else(|| servers.iter().find(|s| s.hardware.is_some()).map(|s| s.id));
		Self {
			server,
			size: q.size.as_deref().and_then(parse_size),
			tag: q.tag.as_deref().and_then(parse_tag),
			advanced: q.tab.as_deref() == Some("advanced"),
		}
	}

	/// The catalog URL with this view's filters, `tweak` changing some.
	fn url(&self, team: TeamId, tweak: impl FnOnce(&mut Self)) -> String {
		let mut v = self.clone();
		tweak(&mut v);
		let mut parts = Vec::new();
		if let Some(server) = v.server {
			parts.push(format!("server={server}"));
		}
		if let Some(size) = v.size {
			parts.push(format!("size={}", size_key(size)));
		}
		if let Some(tag) = v.tag {
			parts.push(format!("tag={}", tag_key(tag)));
		}
		if v.advanced {
			parts.push("tab=advanced".to_owned());
		}
		if parts.is_empty() {
			catalog_url(team)
		} else {
			format!("{}?{}", catalog_url(team), parts.join("&"))
		}
	}

	fn hardware<'a>(&self, servers: &'a [TeamServer]) -> Option<&'a Hardware> {
		let id = self.server?;
		servers
			.iter()
			.find(|s| s.id == id)
			.and_then(|s| s.hardware.as_ref())
	}
}

const CHIP: &str = "inline-flex h-8 items-center rounded-full border border-border px-3 text-sm font-medium hover:bg-foreground/5";
const CHIP_ON: &str = "inline-flex h-8 items-center rounded-full border border-transparent bg-primary px-3 text-sm font-medium text-primary-foreground";

fn chip(label: &str, href: &str, on: bool) -> Markup {
	html! {
		a class=(if on { CHIP_ON } else { CHIP }) href=(href) aria-current=[on.then_some("true")] { (label) }
	}
}

fn server_select(ctx: &TeamCtx, view: &View, servers: &[TeamServer]) -> Markup {
	let prefix = view.url(ctx.team.id, |v| v.server = None);
	let joiner = if prefix.contains('?') {
		"&server="
	} else {
		"?server="
	};
	let on_change = format!(
		"window.location.assign({} + el.value)",
		js_string(&format!("{prefix}{joiner}"))
	);
	html! {
		label class=(LABEL) {
			"Rate models against"
			select class=(INPUT) "data-on:change"=(on_change) {
				@for s in servers {
					option value=(s.id.0) selected[view.server == Some(s.id)] {
						(s.name) @if s.hardware.is_none() { " (hardware not reported yet)" }
					}
				}
			}
		}
	}
}

fn filters(ctx: &TeamCtx, view: &View) -> Markup {
	let team = ctx.team.id;
	html! {
		div class="flex flex-col gap-3" {
			div class="flex flex-wrap items-center gap-2" {
				span class="w-12 text-sm text-muted-foreground" { "Size" }
				(chip("All", &view.url(team, |v| v.size = None), view.size.is_none()))
				@for size in [SizeClass::Tiny, SizeClass::Small, SizeClass::Medium, SizeClass::Large] {
					a class=(if view.size == Some(size) { CHIP_ON } else { CHIP })
						href=(view.url(team, |v| v.size = Some(size))) title=(size_hint(size)) { (size_label(size)) }
				}
			}
			div class="flex flex-wrap items-center gap-2" {
				span class="w-12 text-sm text-muted-foreground" { "Tag" }
				(chip("All", &view.url(team, |v| v.tag = None), view.tag.is_none()))
				@for tag in [Tag::Coding, Tag::Reasoning, Tag::Vision] {
					(chip(tag_label(tag), &view.url(team, |v| v.tag = Some(tag)), view.tag == Some(tag)))
				}
			}
		}
	}
}

fn tabs(ctx: &TeamCtx, view: &View) -> Markup {
	let team = ctx.team.id;
	html! {
		div class="flex gap-2" {
			(chip("Catalog", &view.url(team, |v| v.advanced = false), !view.advanced))
			(chip("Advanced", &view.url(team, |v| v.advanced = true), view.advanced))
		}
	}
}

/// The recipe a card is about: the best for the server, else the smallest download.
fn card_recipe(
	model: &Model,
	hardware: Option<&Hardware>,
) -> (Recipe, Option<crate::catalog::Fit>) {
	if let Some((recipe, fit)) = hardware.and_then(|hw| suggest(hw, model).into_iter().next()) {
		return (recipe, Some(fit));
	}
	let recipe = model
		.recipes
		.iter()
		.min_by_key(|r| r.total_bytes)
		.cloned()
		.unwrap_or_else(|| model.recipes[0].clone());
	(recipe, None)
}

fn model_card(ctx: &TeamCtx, view: &View, model: &Model, hardware: Option<&Hardware>) -> Markup {
	let (recipe, fit) = card_recipe(model, hardware);
	let engine = fit.as_ref().and_then(|f| f.engine).unwrap_or(recipe.engine);
	let href = format!(
		"{}/{}{}",
		catalog_url(ctx.team.id),
		model.id,
		view.server
			.map_or(String::new(), |s| format!("?server={s}"))
	);
	html! {
		div class="flex flex-col justify-between gap-4 rounded-xl border border-border bg-card p-4 text-card-foreground" {
			div class="flex flex-col gap-2" {
				div class="flex items-start justify-between gap-2" {
					a class="font-semibold hover:underline" href=(href) { (model.name) }
					span class=(Tone::Neutral.badge_class()) { (size_label(model.size_class)) }
				}
				p class="text-xs text-muted-foreground" {
					(model.family) " · " (params_text(model.params_b, model.active_params_b)) " · " (model.license)
				}
				@if !model.tags.is_empty() {
					div class="flex flex-wrap gap-1" {
						@for tag in &model.tags {
							span class=(Tone::Neutral.badge_class()) { (tag_label(*tag)) }
						}
					}
				}
			}
			div class="flex flex-col gap-2" {
				p class="text-xs text-muted-foreground" {
					(recipe.quant) " · " (engines::spec(engine).label) " · " (human_bytes(recipe.total_bytes))
				}
				div class="flex items-center justify-between gap-2" {
					@if let Some(fit) = &fit { (fit_badge(fit)) } @else { span class="text-xs text-muted-foreground" { "No server picked" } }
					a class=(BUTTON_OUTLINE) href=(href) { "Set up" }
				}
			}
		}
	}
}

fn tier_sections(ctx: &TeamCtx, view: &View, hardware: Option<&Hardware>) -> Markup {
	let shown: Vec<&Model> = catalog::models()
		.iter()
		.filter(|m| view.size.is_none_or(|s| m.size_class == s))
		.filter(|m| view.tag.is_none_or(|t| m.tags.contains(&t)))
		.collect();
	html! {
		@if shown.is_empty() {
			p class="text-sm text-muted-foreground" { "No model matches these filters." }
		}
		@for tier in TIERS {
			@let in_tier: Vec<&&Model> = shown.iter().filter(|m| m.tier == tier).collect();
			@if !in_tier.is_empty() {
				section class="flex flex-col gap-3" {
					h2 class="text-sm font-semibold text-muted-foreground" { (tier_label(tier)) " · " (in_tier.len()) }
					div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
						@for model in in_tier { (model_card(ctx, view, model, hardware)) }
					}
				}
			}
		}
	}
}

fn advanced_tab(ctx: &TeamCtx, view: &View) -> Markup {
	let search = format!(
		"evt.preventDefault(); @get({})",
		js_string(&format!(
			"{}/search{}",
			catalog_url(ctx.team.id),
			view.server
				.map_or(String::new(), |s| format!("?server={s}"))
		))
	);
	card(&html! {
		(card_header("Search Hugging Face", Some("Any repo with GGUF files. Pick a quantization and podkit rates it against the server like a catalog model.")))
		(card_content(&html! {
			form class="flex flex-col gap-4" "data-on:submit"=(search) {
				label class=(LABEL) {
					"Search"
					input class=(INPUT) type="text" required maxlength="100" placeholder="qwen3 4b gguf" autocomplete="off" "data-bind:q";
				}
				(form_error())
				div {
					button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Search" }
				}
			}
			div id="hf-results" class="mt-4" {}
		}))
	})
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(q): Query<CatalogQuery>,
) -> Result<Response, Response> {
	let servers = catalog_service::team_servers(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let view = View::from_query(&q, &servers);
	let hardware = view.hardware(&servers);

	let signals = format!(
		"{{q: '', hfRepo: '', hfPath: '', hfSelection: '', hfEngine: '', server: {}, _error: ''}}",
		js_string(&view.server.map_or(String::new(), |s| s.to_string()))
	);
	let body = html! {
		div class="flex flex-col gap-6" data-signals=(signals) {
			div class="flex flex-wrap items-center justify-between gap-3" {
				h1 class="text-xl font-semibold" { "Model catalog" }
				a class=(BUTTON_OUTLINE) href=(models_url(ctx.team.id)) { "Your models" }
			}
			(tabs(&ctx, &view))
			@if servers.is_empty() {
				p class="text-sm text-muted-foreground" { "This team has no server yet. Add one to see which models fit." }
			} @else {
				(server_select(&ctx, &view, &servers))
				@if hardware.is_none() {
					p class="text-sm text-muted-foreground" { "This server hasn't reported its hardware yet, so fit can't be rated. Try again in a minute." }
				}
			}
			@if view.advanced {
				(advanced_tab(&ctx, &view))
			} @else {
				(filters(&ctx, &view))
				(tier_sections(&ctx, &view, hardware))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Model catalog",
		&crumbs(&ctx, vec![("Catalog", None)]),
		&body,
	)
	.into_response())
}

fn hit_rows(ctx: &TeamCtx, hits: &[Hit], can_download: bool) -> Markup {
	let download = format!("@post('{}/downloads/hf')", super::models_url(ctx.team.id));
	html! {
		div id="hf-results" class="mt-4 flex flex-col gap-4" {
			@if hits.is_empty() {
				p class="text-sm text-muted-foreground" { "No repo with GGUF files matches that." }
			}
			@for hit in hits {
				div class="flex flex-col gap-2 rounded-lg border border-border p-3" {
					div class="flex flex-wrap items-baseline justify-between gap-2" {
						strong class="break-all" { (hit.repo) }
						span class="text-xs text-muted-foreground" { (hit.downloads) " downloads · " (hit.likes) " likes" }
					}
					@if let Some(err) = &hit.error {
						p class="text-sm text-muted-foreground" { "Can't list the files: " (err) }
					}
					ul {
						@for v in &hit.variants {
							li class="flex flex-wrap items-center justify-between gap-2 border-t border-border py-2 text-sm first:border-t-0" {
								span { (v.quant) " · " (human_bytes(v.total_bytes))
									@if v.shards > 1 { " · " (v.shards) " files" } }
								div class="flex items-center gap-2" {
									@if let Some(fit) = &v.fit { (fit_badge(fit)) }
									@if can_download {
										@let selection = serde_json::to_string(&v.selection).unwrap_or_default();
										@let engine = v.fit.as_ref().and_then(|f| f.engine).map_or(String::new(), |e| serde_json::to_value(e).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
										button type="button" class=(BUTTON_OUTLINE)
											data-repo=(hit.repo) data-path=(v.model_path) data-selection=(selection) data-engine=(engine)
											"data-on:click"={ "$hfRepo = el.dataset.repo; $hfPath = el.dataset.path; $hfSelection = el.dataset.selection; $hfEngine = el.dataset.engine; " (download) } { "Download" }
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

#[derive(Deserialize)]
struct SearchSignals {
	q: String,
}

#[derive(Debug, Default, Deserialize)]
struct ServerQuery {
	server: Option<String>,
}

async fn search(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(sq): Query<ServerQuery>,
	ReadSignals(sig): ReadSignals<SearchSignals>,
) -> Response {
	let result = async {
		let servers = catalog_service::team_servers(&s, ctx.user.id, ctx.team.id).await?;
		let view = View::from_query(
			&CatalogQuery {
				server: sq.server,
				..CatalogQuery::default()
			},
			&servers,
		);
		catalog_service::hf_search(
			&s,
			ctx.user.id,
			ctx.team.id,
			&sig.q,
			view.hardware(&servers),
		)
		.await
	}
	.await;
	match result {
		Ok(hits) => {
			let can = ctx.perms.can(Action::Create, Resource::ModelFile);
			sse::patch(&hit_rows(&ctx, &hits, can))
				.and(sse::signals(&serde_json::json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

/// The set-up page of one catalog model.
async fn setup(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, catalog_id)): Path<(TeamId, String)>,
	Query(q): Query<CatalogQuery>,
) -> Result<Response, Response> {
	let model = catalog::model(&catalog_id)
		.ok_or_else(|| error::page(&ctx, &ServerError::ModelNotFound))?;
	let servers = catalog_service::team_servers(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let view = View::from_query(&q, &servers);
	let hardware = view.hardware(&servers);
	let projects = projects::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;

	let rated: Vec<(Recipe, Option<crate::catalog::Fit>)> = match hardware {
		Some(hw) => suggest(hw, model)
			.into_iter()
			.map(|(r, f)| (r, Some(f)))
			.collect(),
		None => model.recipes.iter().cloned().map(|r| (r, None)).collect(),
	};
	let first = rated.first().map(|(r, _)| r.id.clone()).unwrap_or_default();
	let can_download = ctx.perms.can(Action::Create, Resource::ModelFile)
		&& ctx.perms.can(Action::Create, Resource::Model);
	let signals = format!(
		"{{catalog: {}, recipe: {}, server: {}, project: {}, name: {}, served: '', ctx: '8192', _error: ''}}",
		js_string(&model.id),
		js_string(&first),
		js_string(&view.server.map_or(String::new(), |s| s.to_string())),
		js_string(
			&projects
				.first()
				.map_or(String::new(), |p| p.project.id.to_string())
		),
		js_string(&model.name),
	);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/downloads/recipe", models_url(ctx.team.id)))
	);
	let back = view.url(ctx.team.id, |_| {});
	let server_prefix = format!("{}/{}?server=", catalog_url(ctx.team.id), model.id);

	let body = html! {
		div class="flex flex-col gap-6" data-signals=(signals) {
			div class="flex flex-col gap-1" {
				h1 class="text-xl font-semibold" { (model.name) }
				p class="text-sm text-muted-foreground" {
					(model.family) " · " (params_text(model.params_b, model.active_params_b)) " · " (model.license) " · " (tier_label(model.tier))
				}
			}
			@if servers.is_empty() {
				p class="text-sm text-muted-foreground" { "This team has no server yet." }
			} @else {
				label class=(LABEL) {
					"Server"
					select class=(INPUT) "data-on:change"={ "window.location.assign(" (js_string(&server_prefix)) " + el.value)" } {
						@for srv in &servers {
							option value=(srv.id.0) selected[view.server == Some(srv.id)] { (srv.name) }
						}
					}
				}
			}
			form class="flex flex-col gap-6" "data-on:submit"=(post) {
				(card(&html! {
					(card_header("Recipe", Some("What to download and how to run it. Best fit first.")))
					(card_content(&html! {
						ul class="flex flex-col gap-3" {
							@for (recipe, fit) in &rated { (recipe_row(recipe, fit.as_ref())) }
						}
					}))
				}))
				(overrides_card(&projects))
				(form_error())
				div class="flex gap-3" {
					@if can_download {
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy || !$server" { "Download" }
					}
					a class=(BUTTON_OUTLINE) href=(back) { "Back to the catalog" }
				}
			}
		}
	};
	Ok(app_page(
		&ctx,
		&model.name,
		&crumbs(
			&ctx,
			vec![("Catalog", Some(back)), (model.name.as_str(), None)],
		),
		&body,
	)
	.into_response())
}

fn overrides_card(projects: &[crate::projects::service::ProjectSummary]) -> Markup {
	card(&html! {
		(card_header("Overrides", Some("Optional. The defaults are fine for most models.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				label class=(LABEL) {
					"Project"
					select class=(INPUT) required "data-bind:project" {
						@for p in projects { option value=(p.project.id.0) { (p.project.name) } }
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
		}))
	})
}

fn recipe_row(recipe: &Recipe, fit: Option<&crate::catalog::Fit>) -> Markup {
	let engine = fit.and_then(|f| f.engine).unwrap_or(recipe.engine);
	html! {
		li {
			label class="flex cursor-pointer flex-col gap-2 rounded-lg border border-border p-3 text-sm hover:bg-foreground/5" {
				div class="flex flex-wrap items-center justify-between gap-2" {
					span class="flex items-center gap-2 font-medium" {
						input type="radio" name="recipe" value=(recipe.id) "data-bind:recipe";
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
