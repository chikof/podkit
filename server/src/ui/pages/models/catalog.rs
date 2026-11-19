//! `/teams/{t}/models/catalog`: logo cards by tier with a server's fit on each, filters by
//! size, tier and tag, and the Hugging Face search in a second tab. A card's "Set up" opens
//! the create dialog (see `setup`); the card's own link is the same flow as a page.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use proto::Hardware;
use serde::Deserialize;

use super::{
	Target, catalog_url, crumbs, family_logo, fit_badge, human_bytes, models_url, params_text,
};
use crate::auth::permission::{Action, Resource};
use crate::catalog::search::Hit;
use crate::catalog::service::{self as catalog_service, TeamServer};
use crate::catalog::{self, Model, Recipe, SizeClass, Tag, Tier, suggest};
use crate::ids::{ServerId, TeamId};
use crate::models::engines;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_OUTLINE, INPUT, LABEL, LogoSize, SELECT, Size, TabItem, Tone, Variant, badge,
	button_class, card, card_content, card_section, chip, chip_group, empty_state, icon, logo,
	tabs,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/models/catalog", get(page))
		.route("/teams/{team_id}/models/catalog/search", get(search))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CatalogQuery {
	pub server: Option<String>,
	pub size: Option<String>,
	pub tag: Option<String>,
	pub tier: Option<String>,
	pub tab: Option<String>,
	pub project: Option<String>,
	pub environment: Option<String>,
	/// `1`: the answer is the create dialog's content, not a page.
	pub dialog: Option<String>,
	/// The dialog step to open on.
	pub step: Option<String>,
}

/// What the page filters and rates against, kept so links can carry it on.
#[derive(Debug, Clone, Default)]
pub(super) struct View {
	pub server: Option<ServerId>,
	pub size: Option<SizeClass>,
	pub tag: Option<Tag>,
	pub tier: Option<Tier>,
	pub hugging_face: bool,
	pub target: Target,
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

pub(super) fn tag_label(t: Tag) -> &'static str {
	match t {
		Tag::Coding => "Coding",
		Tag::Reasoning => "Reasoning",
		Tag::Vision => "Vision",
	}
}

fn parse_tier(s: &str) -> Option<Tier> {
	match s {
		"s" => Some(Tier::S),
		"a" => Some(Tier::A),
		"b" => Some(Tier::B),
		"c" => Some(Tier::C),
		"d" => Some(Tier::D),
		_ => None,
	}
}

fn tier_key(t: Tier) -> &'static str {
	match t {
		Tier::S => "s",
		Tier::A => "a",
		Tier::B => "b",
		Tier::C => "c",
		Tier::D => "d",
	}
}

pub(super) fn tier_label(t: Option<Tier>) -> &'static str {
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
	pub fn from_query(q: &CatalogQuery, servers: &[TeamServer]) -> Self {
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
			tier: q.tier.as_deref().and_then(parse_tier),
			hugging_face: matches!(q.tab.as_deref(), Some("huggingface" | "hf" | "advanced")),
			target: Target {
				project: q.project.clone(),
				environment: q.environment.clone(),
			},
		}
	}

	/// The catalog URL with this view's filters, `tweak` changing some.
	pub fn url(&self, team: TeamId, tweak: impl FnOnce(&mut Self)) -> String {
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
		if let Some(tier) = v.tier {
			parts.push(format!("tier={}", tier_key(tier)));
		}
		if v.hugging_face {
			parts.push("tab=huggingface".to_owned());
		}
		parts.extend(v.target.pairs());
		with_query(&catalog_url(team), &parts)
	}

	/// The part of a query that follows a model into its set-up page: server, project,
	/// environment.
	pub fn carry(&self) -> Vec<String> {
		let mut parts = Vec::new();
		if let Some(server) = self.server {
			parts.push(format!("server={server}"));
		}
		parts.extend(self.target.pairs());
		parts
	}

	pub fn hardware<'a>(&self, servers: &'a [TeamServer]) -> Option<&'a Hardware> {
		let id = self.server?;
		servers
			.iter()
			.find(|s| s.id == id)
			.and_then(|s| s.hardware.as_ref())
	}
}

/// `base`, then `?a&b` when there are parts.
pub(super) fn with_query(base: &str, parts: &[String]) -> String {
	if parts.is_empty() {
		base.to_owned()
	} else {
		format!("{base}?{}", parts.join("&"))
	}
}

fn server_form(ctx: &TeamCtx, view: &View, servers: &[TeamServer]) -> Markup {
	let hidden = |name: &str, value: Option<String>| {
		html! { @if let Some(value) = value { input type="hidden" name=(name) value=(value); } }
	};
	html! {
		form method="get" action=(catalog_url(ctx.team.id)) class="flex flex-wrap items-end gap-3" {
			(hidden("size", view.size.map(|s| size_key(s).to_owned())))
			(hidden("tag", view.tag.map(|t| tag_key(t).to_owned())))
			(hidden("tier", view.tier.map(|t| tier_key(t).to_owned())))
			(hidden("tab", view.hugging_face.then(|| "huggingface".to_owned())))
			(hidden("project", view.target.project_id().map(|p| p.to_string())))
			(hidden("environment", view.target.environment_id().map(|e| e.to_string())))
			label class={ (LABEL) " w-full max-w-sm" } {
				"Rate models against"
				select class=(SELECT) name="server" "data-on:change"="el.form.requestSubmit()" {
					@for s in servers {
						option value=(s.id.0) selected[view.server == Some(s.id)] {
							(s.name) @if s.hardware.is_none() { " (hardware not reported yet)" }
						}
					}
				}
			}
			noscript { button type="submit" class=(BUTTON_OUTLINE) { "Apply" } }
		}
	}
}

fn filters(ctx: &TeamCtx, view: &View) -> Markup {
	let team = ctx.team.id;
	html! {
		div class="flex flex-col gap-3" {
			(chip_group("Size", &html! {
				(chip("All", &view.url(team, |v| v.size = None), view.size.is_none(), None))
				@for size in [SizeClass::Tiny, SizeClass::Small, SizeClass::Medium, SizeClass::Large] {
					(chip(size_label(size), &view.url(team, |v| v.size = Some(size)), view.size == Some(size), Some(size_hint(size))))
				}
			}))
			(chip_group("Tier", &html! {
				(chip("All", &view.url(team, |v| v.tier = None), view.tier.is_none(), None))
				@for tier in [Tier::S, Tier::A, Tier::B, Tier::C, Tier::D] {
					(chip(tier_key(tier).to_uppercase().as_str(), &view.url(team, |v| v.tier = Some(tier)), view.tier == Some(tier), Some(tier_label(Some(tier)))))
				}
			}))
			(chip_group("Tag", &html! {
				(chip("All", &view.url(team, |v| v.tag = None), view.tag.is_none(), None))
				@for tag in [Tag::Coding, Tag::Reasoning, Tag::Vision] {
					(chip(tag_label(tag), &view.url(team, |v| v.tag = Some(tag)), view.tag == Some(tag), None))
				}
			}))
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

/// The link to a model's set-up, which also opens the dialog when script runs.
fn setup_href(ctx: &TeamCtx, view: &View, model: &Model) -> String {
	with_query(
		&format!("{}/{}", catalog_url(ctx.team.id), model.id),
		&view.carry(),
	)
}

fn model_card(ctx: &TeamCtx, view: &View, model: &Model, hardware: Option<&Hardware>) -> Markup {
	let (recipe, fit) = card_recipe(model, hardware);
	let engine = fit.as_ref().and_then(|f| f.engine).unwrap_or(recipe.engine);
	let href = setup_href(ctx, view, model);
	let description = format!(
		"{} · {} · {}",
		model.family,
		params_text(model.params_b, model.active_params_b),
		model.license
	);
	// The same card as `link_card` (one link stretched over it) with a footer under the
	// logo row: the fit, what would be downloaded, and the button.
	html! {
		article class="ui-card ui-link-card flex flex-col gap-4 rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs" {
			div class="ui-link-card-body" {
				(family_logo(&model.family, LogoSize::Md))
				div class="flex min-w-0 flex-1 flex-col gap-1" {
					h2 class="truncate text-sm leading-tight font-semibold" {
						a href=(href) class="ui-link-card-link outline-hidden" { (model.name) }
					}
					p class="line-clamp-2 text-sm text-muted-foreground" { (description) }
				}
			}
			div class="flex flex-wrap items-center gap-1.5" {
				@if let Some(fit) = &fit { (fit_badge(fit)) } @else { span class="text-xs text-muted-foreground" { "No server picked" } }
				(badge(Tone::Neutral, size_label(model.size_class)))
				@for tag in &model.tags { (badge(Tone::Neutral, tag_label(*tag))) }
			}
			div class="mt-auto flex flex-wrap items-center justify-between gap-2" {
				p class="text-xs text-muted-foreground" {
					(recipe.quant) " · " (engines::spec(engine).label) " · " (human_bytes(recipe.total_bytes))
				}
				a class={ (button_class(Variant::Outline, Size::Sm)) " ui-setup" } href=(href) { "Set up" }
			}
		}
	}
}

fn tier_sections(ctx: &TeamCtx, view: &View, hardware: Option<&Hardware>) -> Markup {
	let shown: Vec<&Model> = catalog::models()
		.iter()
		.filter(|m| view.size.is_none_or(|s| m.size_class == s))
		.filter(|m| view.tag.is_none_or(|t| m.tags.contains(&t)))
		.filter(|m| view.tier.is_none_or(|t| m.tier == Some(t)))
		.collect();
	// A click on a card or its button opens the create dialog; with a modifier key, or
	// without script, the link is the same flow as a page.
	let open = "const a = evt.target.closest('a.ui-link-card-link, a.ui-setup'); \
		if (a && evt.button === 0 && !evt.metaKey && !evt.ctrlKey && !evt.shiftKey) { \
		evt.preventDefault(); const h = a.getAttribute('href'); \
		@get(h + (h.includes('?') ? '&' : '?') + 'dialog=1') }";
	html! {
		div class="flex flex-col gap-8" "data-on:click"=(open) {
			@if shown.is_empty() {
				(empty_state(
					"search",
					"No model matches these filters",
					Some("Widen the size, tier or tag."),
					Some(&html! { a class=(button_class(Variant::Outline, Size::Md)) href=(view.url(ctx.team.id, |v| { v.size = None; v.tag = None; v.tier = None; })) { "Clear the filters" } }),
				))
			}
			@for tier in TIERS {
				@let in_tier: Vec<&&Model> = shown.iter().filter(|m| m.tier == tier).collect();
				@if !in_tier.is_empty() {
					section class="flex flex-col gap-3" {
						h2 class="text-sm font-semibold text-muted-foreground" { (tier_label(tier)) " · " (in_tier.len()) }
						div class="ui-card-grid" {
							@for model in in_tier { (model_card(ctx, view, model, hardware)) }
						}
					}
				}
			}
		}
	}
}

/// The create dialog's frame. Its content arrives with the click that opens it.
fn setup_dialog() -> Markup {
	html! {
		dialog id="setup-dialog" class="ui-dialog ui-dialog-wide" closedby="closerequest"
			aria-label="Set up a model" aria-labelledby="setup-title" {
			div id="setup-body" class="ui-dlg-panel" {
				div class="ui-dialog-head" {
					div { h2 id="setup-title" class="ui-dialog-title" { "Set up a model" } }
					form method="dialog" {
						button class="ui-icon-btn" aria-label="Close" { (icon("x")) }
					}
				}
				div class="ui-dialog-body" {
					p class="ui-hint" { "Loading…" }
				}
			}
		}
	}
}

fn huggingface_tab(ctx: &TeamCtx, view: &View) -> Markup {
	let search = format!(
		"evt.preventDefault(); @get({})",
		js_string(&with_query(
			&format!("{}/search", catalog_url(ctx.team.id)),
			&view
				.server
				.map(|s| format!("server={s}"))
				.into_iter()
				.collect::<Vec<_>>()
		))
	);
	card(&html! {
		(card_section("Search Hugging Face", Some("Any repo with GGUF files. Pick a quantization and podkit rates it against the server like a catalog model."), None))
		(card_content(&html! {
			form class="flex flex-col gap-4" "data-on:submit"=(search) {
				label class=(LABEL) {
					"Search"
					input class=(INPUT) type="text" required maxlength="100" placeholder="qwen3 4b gguf" autocomplete="off" "data-bind:q";
				}
				(form_error())
				div {
					button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("search")) "Search" }
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
	let catalog_tab = view.url(ctx.team.id, |v| v.hugging_face = false);
	let hf_tab = view.url(ctx.team.id, |v| v.hugging_face = true);
	let body = html! {
		div class="ui-page-body" data-signals=(signals) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Model catalog" }
					p class="ui-lede" {
						"Models chiko has run and ranked, rated against one of your servers. Pick one to download its weights and serve it."
					}
				}
				div class="ui-page-actions" {
					a class=(button_class(Variant::Outline, Size::Md)) href=(models_url(ctx.team.id)) { "Your models" }
				}
			}
			(tabs("Catalog sections", &[
				TabItem { label: "Catalog", href: &catalog_tab, current: !view.hugging_face },
				TabItem { label: "Hugging Face", href: &hf_tab, current: view.hugging_face },
			]))
			@if servers.is_empty() {
				p class="text-sm text-muted-foreground" { "This team has no server yet. Add one to see which models fit." }
			} @else {
				(server_form(&ctx, &view, &servers))
				@if hardware.is_none() {
					p class="text-sm text-muted-foreground" { "This server hasn't reported its hardware yet, so fit can't be rated. Try again in a minute." }
				}
			}
			@if view.hugging_face {
				(huggingface_tab(&ctx, &view))
			} @else {
				(filters(&ctx, &view))
				(tier_sections(&ctx, &view, hardware))
				(setup_dialog())
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
				div class="flex flex-col gap-3 rounded-xl border border-border bg-card p-4" {
					div class="flex flex-wrap items-center justify-between gap-2" {
						div class="flex min-w-0 items-center gap-3" {
							(logo("huggingface", LogoSize::Sm))
							strong class="text-sm break-all" { (hit.repo) }
						}
						span class="text-xs text-muted-foreground" { (hit.downloads) " downloads · " (hit.likes) " likes" }
					}
					@if let Some(err) = &hit.error {
						p class="text-sm text-muted-foreground" { "Can't list the files: " (err) }
					}
					ul class="flex flex-col" {
						@for v in &hit.variants {
							li class="flex flex-wrap items-center justify-between gap-2 border-t border-border py-2 text-sm first:border-t-0" {
								span { (v.quant) " · " (human_bytes(v.total_bytes))
									@if v.shards > 1 { " · " (v.shards) " files" } }
								div class="flex items-center gap-2" {
									@if let Some(fit) = &v.fit { (fit_badge(fit)) }
									@if can_download {
										@let selection = serde_json::to_string(&v.selection).unwrap_or_default();
										@let engine = v.fit.as_ref().and_then(|f| f.engine).map_or(String::new(), |e| serde_json::to_value(e).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
										button type="button" class=(button_class(Variant::Outline, Size::Sm))
											data-repo=(hit.repo) data-path=(v.model_path) data-selection=(selection) data-engine=(engine)
											"data-on:click"={ "$hfRepo = el.dataset.repo; $hfPath = el.dataset.path; $hfSelection = el.dataset.selection; $hfEngine = el.dataset.engine; " (download) } { (icon("download")) "Download" }
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
