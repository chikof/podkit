//! The Repository section of an application that clones through a git provider: the provider
//! and repository are fixed (shown, not edited), the branch can change, and there is no deploy
//! key or token to manage: the provider's own access does the cloning.

use maud::{Markup, html};

use crate::applications::service::ApplicationView;
use crate::ui::components::{INPUT, LABEL};

const HELP: &str = "text-xs font-normal text-muted-foreground";

/// The provider's name for the page, or a plain stand-in for someone who may not read providers.
#[must_use]
pub(super) fn provider_label(name: Option<&str>) -> &str {
	name.unwrap_or("a git provider")
}

/// Read-only repository and an editable branch.
pub(super) fn provider_fields(app: &ApplicationView, provider: Option<&str>) -> Markup {
	html! {
		div class="flex flex-col gap-1 text-sm font-medium" {
			span { "Repository" }
			code class="font-mono break-all font-normal" { (app.git_repo.as_deref().unwrap_or_default()) }
			span class=(HELP) {
				"Cloned through " strong { (provider_label(provider)) } ". The provider and repository can't be changed; make a new application to use another."
			}
		}
		label class=(LABEL) {
			"Branch, tag or ref"
			input class={ (INPUT) " font-mono" } type="text" placeholder="main" autocomplete="off" spellcheck="false" "data-bind:git_ref";
			span class=(HELP) { "A push to this branch deploys the application." }
		}
	}
}
