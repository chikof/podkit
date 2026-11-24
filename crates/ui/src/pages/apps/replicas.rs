//! The Replicas section of the General tab: how many copies of the application run behind its
//! URL, whether a client sticks to one, and how a deploy replaces them.
//!
//! The fields are signals of the General form (`rp_*`), so its save bar and dirty dialog cover
//! them. Nothing here reads the database: `service` hands over [`Settings`].

use maud::{Markup, html};
use serde_json::{Map, Value, json};

use super::form::{AppSignals, HELP};
use crate::components::{CHECKBOX, INPUT, LABEL};
use podkit_core::replicas::ReplicaConfig;
use podkit_core::replicas::service::{ReplicaInput, Settings};

/// The signals the Replicas fields are bound to; the save bar watches them with the others.
pub(super) const FIELDS: &[&str] = &[
	"rp_count",
	"rp_sticky",
	"rp_surge",
	"rp_unavail",
	"rp_shared",
];

/// The initial value of every replica signal.
pub(super) fn signals(config: &ReplicaConfig) -> Map<String, Value> {
	let Value::Object(map) = json!({
		"rp_count": config.replicas.to_string(),
		"rp_sticky": config.sticky,
		"rp_surge": config.max_surge.to_string(),
		"rp_unavail": config.max_unavailable.to_string(),
		"rp_shared": config.shared_volumes,
	}) else {
		return Map::new();
	};
	map
}

/// What the form sent, or `None` when the page predates these fields (nothing to change).
pub(super) fn input(sig: &AppSignals) -> Option<ReplicaInput> {
	if sig.rp_count.is_empty() {
		return None;
	}
	Some(ReplicaInput {
		replicas: sig.rp_count.clone(),
		sticky: sig.rp_sticky,
		max_surge: sig.rp_surge.clone(),
		max_unavailable: sig.rp_unavail.clone(),
		shared_volumes: sig.rp_shared,
	})
}

fn number(label: &str, signal: &str, help: &str) -> Markup {
	html! {
		label class=(LABEL) {
			(label)
			input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]{1,2}" autocomplete="off"
				enterkeyhint="next" "data-bind"=(signal);
			span class=(HELP) { (help) }
		}
	}
}

/// The fields. The rollout limits only show while there is more than one replica to roll.
pub(super) fn fields(settings: &Settings) -> Markup {
	html! {
		div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
			(number(
				"Replicas",
				"rp_count",
				"Copies to run, 1 to 10. Traefik shares requests between them, round robin.",
			))
		}
		div class="flex flex-col gap-4" "data-show"="Number($rp_count) > 1" {
			label class="flex items-center gap-2 text-sm font-normal" {
				input type="checkbox" class=(CHECKBOX) "data-bind:rp_sticky";
				"Keep a visitor on one replica (cookie)"
			}
			@if settings.volumes > 0 {
				label class="flex items-center gap-2 text-sm font-normal" {
					input type="checkbox" class=(CHECKBOX) "data-bind:rp_shared";
					"This application's volumes are safe to share between replicas"
				}
				span class=(HELP) {
					"It has " (settings.volumes) @if settings.volumes == 1 { " volume" } @else { " volumes" }
					". Replicas write the same files, so more than one is refused until you vouch for them."
				}
			}
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
				(number(
					"Extra replicas during an update",
					"rp_surge",
					"Started next to the old ones, then the old ones go.",
				))
				(number(
					"Replicas that may be down",
					"rp_unavail",
					"Used when the extra is 0: old replicas stop first.",
				))
			}
		}
	}
}

/// The settings as text, for someone who may not change them.
pub(super) fn summary(config: &ReplicaConfig) -> Markup {
	html! {
		@if config.is_scaled() {
			dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
				div class="flex flex-col gap-1" {
					dt class="text-muted-foreground" { "Replicas" }
					dd class="font-medium" { (config.replicas) }
				}
				div class="flex flex-col gap-1" {
					dt class="text-muted-foreground" { "Sticky sessions" }
					dd class="font-medium" { @if config.sticky { "On" } @else { "Off" } }
				}
				div class="flex flex-col gap-1" {
					dt class="text-muted-foreground" { "Update" }
					dd class="font-medium" {
						@if config.max_surge > 0 { (config.max_surge) " extra at a time" }
						@else { (config.max_unavailable) " down at a time" }
					}
				}
			}
		} @else {
			p class="text-sm text-muted-foreground" { "One container runs the application." }
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sig(count: &str) -> AppSignals {
		AppSignals {
			rp_count: count.to_owned(),
			rp_surge: "1".to_owned(),
			rp_unavail: "0".to_owned(),
			..AppSignals::default()
		}
	}

	#[test]
	fn replicas_signals_cover_every_field_the_save_bar_watches() {
		let map = signals(&ReplicaConfig::default());
		for field in FIELDS {
			assert!(map.contains_key(*field), "{field}");
		}
		assert_eq!(map.len(), FIELDS.len());
		assert_eq!(map["rp_count"], "1");
		assert_eq!(map["rp_sticky"], false);
	}

	#[test]
	fn replicas_form_an_old_page_changes_nothing_and_bad_numbers_are_refused() {
		assert!(input(&AppSignals::default()).is_none());
		let ok = input(&sig("3")).unwrap().parse().unwrap();
		assert_eq!(ok.replicas, 3);
		assert!(input(&sig("0")).unwrap().parse().is_err());
		assert!(input(&sig("11")).unwrap().parse().is_err());
		assert!(input(&sig("abc")).unwrap().parse().is_err());
		let mut stuck = sig("3");
		stuck.rp_surge = "0".to_owned();
		assert!(input(&stuck).unwrap().parse().is_err());
	}

	#[test]
	fn replicas_fields_offer_the_shared_volume_vouch_only_with_volumes() {
		let none = fields(&Settings {
			config: ReplicaConfig::default(),
			volumes: 0,
		})
		.into_string();
		assert!(!none.contains("rp_shared"), "{none}");
		let some = fields(&Settings {
			config: ReplicaConfig::default(),
			volumes: 2,
		})
		.into_string();
		assert!(some.contains("data-bind:rp_shared"), "{some}");
		assert!(some.contains("2 volumes"), "{some}");
		assert!(!some.contains("type=\"number\""), "{some}");
	}
}
