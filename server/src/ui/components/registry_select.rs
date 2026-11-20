//! Registry picker: the select an image application, a stack or a template deploy offers so
//! a saved login is chosen instead of typed again. It binds the signal `registry_id`; an
//! empty value means "no login" (a public image).

use maud::{Markup, html};

use super::field::{LABEL, SELECT};
use crate::error::ServerError;
use crate::ids::{RegistryId, TeamId};
use crate::registries::Registry;

const HELP: &str = "text-xs font-normal text-muted-foreground";

/// The labelled select, with a link to where registries are managed.
pub fn registry_select(registries: &[Registry], team: TeamId) -> Markup {
	html! {
		label class=(LABEL) {
			"Registry"
			select class=(SELECT) "data-bind:registry_id" {
				button { selectedcontent {} }
				option value="" { "None (public images)" }
				@for registry in registries {
					option value=(registry.id.0) { (registry.name) " \u{b7} " (registry.url) }
				}
			}
			span class=(HELP) {
				@if registries.is_empty() {
					"No saved registries yet. "
				} @else {
					"The login used to pull private images. "
				}
				a class="underline underline-offset-2" href={ "/teams/" (team) "/registries" } { "Manage registries" }
				"."
			}
		}
	}
}

/// The registry a form posted: `None` for an empty value, an error for anything that is not
/// a number. Whether the registry is the team's is the service's check.
pub fn registry_choice(raw: &str) -> Result<Option<RegistryId>, ServerError> {
	match raw.trim() {
		"" => Ok(None),
		raw => raw
			.parse()
			.map(|id| Some(RegistryId(id)))
			.map_err(|_| ServerError::Validation("Unknown registry.".to_owned())),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_empty_choice_is_none_and_junk_is_refused() {
		assert_eq!(registry_choice(" ").unwrap(), None);
		assert_eq!(registry_choice("7").unwrap(), Some(RegistryId(7)));
		assert!(registry_choice("7; drop").is_err());
	}
}
