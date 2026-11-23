//! The dialogs of the Git providers page: connect GitHub, GitLab or Gitea, and delete. Delete
//! is one dialog filled from the signals the row menu sets (`_p_id`, `_p_name`, `_p_uses`).
//! A token typed here lives in a signal until the request that stores it, then is cleared.

use maud::{Markup, html};

use super::providers_url;
use crate::git_providers::Kind;
use crate::ids::TeamId;
pub(super) use crate::ui::components::dialog_open_button as open_button;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, INPUT, LABEL, Size, Variant, banner, button_class, dialog,
	dialog_cancel,
};
use crate::ui::layout::form_error;
use crate::ui::sse::js_string;

pub(super) const GITHUB: &str = "connect-github";
pub(super) const GITLAB: &str = "connect-gitlab";
pub(super) const GITEA: &str = "connect-gitea";
pub(super) const DELETE: &str = "delete-provider";

const FOOT: &str = "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end";
const HELP: &str = "ui-hint";

/// Connect a GitHub App: name it, then post GitHub its manifest. Two steps in one dialog: the
/// form, then (after podkit has built the manifest) the button that goes to GitHub.
pub(super) fn github_dialog(team: TeamId, public: bool) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/github/start", providers_url(team)))
	);
	let body = html! {
		form id="github-form" class="flex flex-col gap-4" "data-show"="!$_gh_ready" "data-on:submit"=(post) "data-indicator:_busy" {
			label class=(LABEL) {
				"App name"
				input class=(INPUT) type="text" required maxlength="34" autocomplete="off" placeholder="podkit" autofocus "data-bind:gh_name";
				span class=(HELP) { "GitHub app names are unique across GitHub, so make it your own." }
			}
			label class=(LABEL) {
				"Organization"
				input class=(INPUT) type="text" maxlength="39" autocomplete="off" spellcheck="false" autocapitalize="off" placeholder="Optional" "data-bind:gh_org";
				span class=(HELP) { "Create the app under an organization. Leave empty for your own account." }
			}
			label class=(LABEL) {
				"GitHub Enterprise address"
				input class=(INPUT) type="url" autocomplete="off" spellcheck="false" autocapitalize="off" placeholder="Optional: https://github.example.com" "data-bind:gh_url";
				span class=(HELP) { "Only for GitHub Enterprise Server. Leave empty for github.com." }
			}
			@if !public {
				(banner(
					BannerKind::Warning,
					Some("GitHub cannot call podkit."),
					"podkit has no public address, so the app is made without a webhook and pushes will not deploy by themselves. Deploy from an application's page.",
					None,
				))
			}
			(form_error())
		}
		div id="github-go" class="flex flex-col gap-3" "data-show"="$_gh_ready" style="display: none" {}
	};
	let footer = html! {
		div class=(FOOT) "data-show"="!$_gh_ready" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form="github-form" class=(BUTTON) "data-attr:disabled"="$_busy" { "Continue" }
		}
		div class=(FOOT) "data-show"="$_gh_ready" style="display: none" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form="github-go-form" class=(BUTTON) { "Continue to GitHub" }
		}
	};
	dialog(
		GITHUB,
		"Connect GitHub",
		Some(
			"podkit creates a GitHub App that can read your repositories. GitHub shows you what it asks for before anything is made.",
		),
		&body,
		&footer,
	)
}

/// What the page patches into the dialog once the manifest is built: the form that sends the
/// browser to GitHub. The manifest is plain JSON the user is about to confirm there; it holds no
/// secret.
pub(super) fn github_go(action: &str, manifest: &str) -> Markup {
	html! {
		div id="github-go" class="flex flex-col gap-3" "data-show"="$_gh_ready" {
			form id="github-go-form" method="post" action=(action) class="flex flex-col gap-3" {
				input type="hidden" name="manifest" value=(manifest);
				p class="text-sm" {
					"Ready. GitHub opens next and asks you to confirm the app. After that you install it on the accounts and repositories podkit may read, and GitHub sends you back here."
				}
			}
		}
	}
}

/// Connect a GitLab or a Gitea with an access token.
pub(super) fn token_dialog(team: TeamId, kind: Kind) -> Markup {
	let (id, prefix, slug) = match kind {
		Kind::Gitlab => (GITLAB, "gl", "gitlab"),
		_ => (GITEA, "ge", "gitea"),
	};
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/{slug}", providers_url(team)))
	);
	let form_id = format!("{slug}-form");
	let (address_hint, scopes, placeholder) = if kind == Kind::Gitlab {
		(
			"Leave empty for gitlab.com. Self-managed: the address of your GitLab.",
			"A personal or group access token with the read_repository and api scopes. api lets podkit create the push webhook.",
			"https://gitlab.com",
		)
	} else {
		(
			"The address of your Gitea or Forgejo, like https://git.example.com.",
			"An access token with read access to repositories (read:repository) and write access to repository hooks (write:repository).",
			"https://git.example.com",
		)
	};
	let body = html! {
		form id=(form_id) class="flex flex-col gap-4" "data-on:submit"=(post) "data-indicator:_busy" {
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus
					placeholder=(if kind == Kind::Gitlab { "gitlab.com" } else { "home" }) "data-bind"=(format!("{prefix}_name"));
			}
			label class=(LABEL) {
				"Address"
				input class=(INPUT) type="url" required[kind == Kind::Gitea] autocomplete="off" spellcheck="false" autocapitalize="off"
					placeholder=(placeholder) "data-bind"=(format!("{prefix}_url"));
				span class=(HELP) { (address_hint) }
			}
			label class=(LABEL) {
				"Access token"
				input class=(INPUT) type="password" required autocomplete="off" spellcheck="false" "data-bind"=(format!("{prefix}_token"));
				span class=(HELP) { (scopes) " Stored encrypted and never shown again." }
			}
			(form_error())
		}
	};
	let footer = html! {
		div class=(FOOT) {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form=(form_id) class=(BUTTON) "data-attr:disabled"="$_busy" { "Connect " (kind.label()) }
		}
	};
	dialog(
		id,
		&format!("Connect {}", kind.label()),
		Some("podkit checks the token against the server before it keeps it."),
		&body,
		&footer,
	)
}

pub(super) fn delete_dialog(team: TeamId) -> Markup {
	let post = format!(
		"@post({} + $_p_id + '/delete')",
		js_string(&format!("{}/", providers_url(team)))
	);
	dialog(
		DELETE,
		"Delete this provider?",
		Some(
			"podkit forgets its credentials. The GitHub App or token itself stays where you made it.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Delete " strong class="break-all" "data-text"="$_p_name" {} "?" }
				p class="text-sm text-destructive" "data-show"="$_p_uses !== ''" style="display: none" {
					"It is in use by " span "data-text"="$_p_uses" {} ". Move them to another source first."
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(post)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting || $_p_uses !== ''" { "Delete provider" }
		},
	)
}
