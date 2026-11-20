//! The repository picker of the create-application page: a provider, a repository found by a
//! server-side search, and a branch. The search box asks podkit, which asks the provider; the
//! answer is a patch that replaces the result list. Nothing here holds a credential.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;

use crate::git_providers::api::{Repo, RepoPage};
use crate::git_providers::service::{self, ProviderView};
use crate::ids::{GitProviderId, TeamId};
use crate::state::AppState;
use crate::ui::components::{INPUT, LABEL, SELECT, Tone, icon};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::sse::{self, js_string};

const HELP: &str = "text-xs font-normal text-muted-foreground";

/// The id of the element the repository results replace.
pub(in crate::ui::pages) const RESULTS: &str = "repo-results";
/// The id of the branch suggestions.
pub(in crate::ui::pages) const BRANCHES: &str = "branch-list";

fn base(team: TeamId) -> String {
	format!("/teams/{team}/git-providers")
}

/// The address the search box asks, built in the browser from the chosen provider.
fn repos_expr(team: TeamId, page: &str) -> String {
	format!(
		"@get({} + $git_provider_id + '/repos?q=' + encodeURIComponent($_repo_q) + '&page=' + {page})",
		js_string(&format!("{}/", base(team)))
	)
}

/// Provider, repository and branch, for the "Git provider" source of a new application.
pub(in crate::ui::pages) fn provider_source_fields(
	team: TeamId,
	providers: &[ProviderView],
) -> Markup {
	if providers.is_empty() {
		return html! {
			p class="text-sm text-muted-foreground" {
				"No git provider is connected yet. "
				a class="underline underline-offset-4" href=(base(team)) { "Connect one" }
				", or use a repository URL."
			}
		};
	}
	let first_page = repos_expr(team, "1");
	let on_provider = format!("$git_repo = ''; $_repo_q = ''; {first_page}");
	html! {
		label class=(LABEL) {
			"Git provider"
			select class=(SELECT) required "data-bind:git_provider_id" "data-on:change"=(on_provider) {
				button { selectedcontent {} }
				option value="" { "Choose a provider\u{2026}" }
				@for p in providers {
					option value=(p.id.0) { (p.name) " \u{b7} " (p.kind.label()) }
				}
			}
		}
		div class="flex flex-col gap-2" "data-show"="$git_provider_id !== ''" style="display: none" {
			label class=(LABEL) "data-show"="$git_repo === ''" {
				"Repository"
				input class=(INPUT) type="search" autocomplete="off" spellcheck="false" autocapitalize="off"
					placeholder="Search repositories\u{2026}" role="combobox" aria-controls=(RESULTS) aria-expanded="true"
					"data-bind:_repo_q" "data-on:input__debounce.250ms"=(first_page);
				span class=(HELP) { "The provider searches; podkit shows what its credentials can see." }
			}
			div id=(RESULTS) class="ui-repo-results" "data-show"="$git_repo === ''" {}
			div class="flex flex-col gap-2" "data-show"="$git_repo !== ''" style="display: none" {
				span class="text-sm font-medium" { "Repository" }
				div class="ui-repo-chosen" {
					(icon("git-branch"))
					code class="font-mono break-all" "data-text"="$git_repo" {}
					button type="button" class="ui-repo-change" "data-on:click"="$git_repo = ''; $_repo_q = ''" { "Change" }
				}
			}
		}
		label class=(LABEL) "data-show"="$git_repo !== ''" style="display: none" {
			"Branch, tag or ref"
			input class={ (INPUT) " font-mono" } type="text" list=(BRANCHES) placeholder="main" autocomplete="off"
				spellcheck="false" "data-bind:git_ref";
			datalist id=(BRANCHES) {}
			span class=(HELP) { "Pick a branch from the list or type a tag. A push to this branch deploys the application." }
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct RepoQuery {
	q: String,
	page: Option<u32>,
}

fn repo_item(team: TeamId, repo: &Repo) -> Markup {
	// Choosing a repository makes its default branch the ref and asks for its other branches.
	let pick = format!(
		"$git_repo = {}; $git_ref = {}; @get({} + $git_provider_id + '/branches?repo=' + encodeURIComponent($git_repo))",
		js_string(&repo.full_name),
		js_string(&repo.default_branch),
		js_string(&format!("{}/", base(team)))
	);
	html! {
		li {
			button type="button" class="ui-repo-item" role="option" "data-on:click"=(pick) {
				span class="ui-repo-name" { (repo.full_name) }
				span class=(Tone::Neutral.badge_class()) { @if repo.private { "Private" } @else { "Public" } }
			}
		}
	}
}

/// The result list for one page, with previous and next when there are more.
pub(super) fn results(team: TeamId, page: u32, found: &RepoPage) -> Markup {
	html! {
		div id=(RESULTS) class="ui-repo-results" "data-show"="$git_repo === ''" {
			@if found.repos.is_empty() {
				p class="ui-repo-empty" { "No repository matches." }
			} @else {
				ul class="ui-repo-list" role="listbox" aria-label="Repositories" {
					@for repo in &found.repos { (repo_item(team, repo)) }
				}
			}
			@if page > 1 || found.next_page.is_some() {
				div class="ui-repo-pager" {
					button type="button" class="ui-repo-change" disabled[page <= 1]
						"data-on:click"=(repos_expr(team, &(page.saturating_sub(1)).to_string())) { "Previous" }
					span { "Page " (page) }
					button type="button" class="ui-repo-change" disabled[found.next_page.is_none()]
						"data-on:click"=(repos_expr(team, &(page + 1).to_string())) { "Next" }
				}
			}
		}
	}
}

fn results_error(message: &str) -> Markup {
	html! {
		div id=(RESULTS) class="ui-repo-results" "data-show"="$git_repo === ''" {
			p class="ui-repo-empty text-destructive" role="alert" { (message) }
		}
	}
}

/// `GET .../{provider}/repos?q=&page=`: one page of search results as a patch.
pub(super) async fn repos(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, GitProviderId)>,
	Query(q): Query<RepoQuery>,
) -> Response {
	let page = q.page.unwrap_or(1).max(1);
	match service::repos(&s, ctx.user.id, ctx.team.id, id, &q.q, page).await {
		Ok(found) => sse::patch(&results(ctx.team.id, page, &found)).into_response(),
		Err(e) => sse::patch(&results_error(&error::message(&e))).into_response(),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct BranchQuery {
	repo: String,
}

/// `GET .../{provider}/branches?repo=`: the branch suggestions of one repository.
pub(super) async fn branches(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, GitProviderId)>,
	Query(q): Query<BranchQuery>,
) -> Response {
	let names = service::branches(&s, ctx.user.id, ctx.team.id, id, &q.repo)
		.await
		.unwrap_or_default();
	sse::patch(&html! {
		datalist id=(BRANCHES) {
			@for name in &names { option value=(name) {} }
		}
	})
	.into_response()
}
