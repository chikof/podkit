//! The Health section of the General tab: whether a deploy waits for the new container to be
//! healthy before it takes over, how that is checked, and when a release that goes bad right
//! after is rolled back.
//!
//! The fields are signals of the General form (`hc_*`), so its save bar and dirty dialog cover
//! them. Nothing here reads the database: `service` hands over a [`HealthConfig`].

use maud::{Markup, html};
use serde_json::{Map, Value, json};

use super::form::{AppSignals, HELP};
use crate::components::{INPUT, LABEL};
use podkit_core::health_gate::service::HealthInput;
use podkit_core::health_gate::{HealthConfig, Kind};

/// The signals the Health fields are bound to; the save bar watches them with the others.
pub(super) const FIELDS: &[&str] = &[
	"hc_mode",
	"hc_path",
	"hc_status",
	"hc_command",
	"hc_interval",
	"hc_timeout",
	"hc_retries",
	"hc_start",
	"hc_wait",
	"hc_drain",
	"hc_window",
	"hc_after",
];

/// The initial value of every health signal for `config` (`None`: the gate is off, and the
/// fields show the defaults they would start from).
pub(super) fn signals(config: Option<&HealthConfig>) -> Map<String, Value> {
	let c = config.cloned().unwrap_or_default();
	let mode = match (config, c.kind) {
		(None, _) => "off",
		(Some(_), Kind::Http) => "http",
		(Some(_), Kind::Command) => "command",
	};
	let Value::Object(map) = json!({
		"hc_mode": mode,
		"hc_path": c.path,
		"hc_status": c.expect_status.to_string(),
		"hc_command": c.command.unwrap_or_default(),
		"hc_interval": c.interval_s.to_string(),
		"hc_timeout": c.timeout_s.to_string(),
		"hc_retries": c.retries.to_string(),
		"hc_start": c.start_period_s.to_string(),
		"hc_wait": c.gate_timeout_s.to_string(),
		"hc_drain": c.drain_s.to_string(),
		"hc_window": c.rollback_window_min.to_string(),
		"hc_after": c.rollback_after.to_string(),
	}) else {
		return Map::new();
	};
	map
}

/// What the form sent, or `None` when the page predates these fields (nothing to change).
pub(super) fn input(sig: &AppSignals) -> Option<HealthInput> {
	if sig.hc_mode.is_empty() {
		return None;
	}
	Some(HealthInput {
		enabled: sig.hc_mode != "off",
		kind: sig.hc_mode.clone(),
		path: sig.hc_path.clone(),
		expect_status: sig.hc_status.clone(),
		command: sig.hc_command.clone(),
		interval_s: sig.hc_interval.clone(),
		timeout_s: sig.hc_timeout.clone(),
		retries: sig.hc_retries.clone(),
		start_period_s: sig.hc_start.clone(),
		gate_timeout_s: sig.hc_wait.clone(),
		drain_s: sig.hc_drain.clone(),
		rollback_window_min: sig.hc_window.clone(),
		rollback_after: sig.hc_after.clone(),
	})
}

fn number(label: &str, signal: &str, help: &str) -> Markup {
	html! {
		label class=(LABEL) {
			(label)
			input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]{1,5}" autocomplete="off"
				enterkeyhint="next" "data-bind"=(signal);
			span class=(HELP) { (help) }
		}
	}
}

fn mode_choice(value: &str, label: &str) -> Markup {
	html! {
		label class="flex items-center gap-2 text-sm font-normal" {
			input type="radio" name="hc_mode" value=(value) "data-bind:hc_mode";
			(label)
		}
	}
}

/// The fields. The checks and the rollback only show while the gate is on.
pub(super) fn fields() -> Markup {
	html! {
		fieldset class="m-0 flex min-w-0 flex-col gap-2 border-0 p-0" {
			legend class="mb-2 text-sm font-medium" { "Wait for the new container" }
			div class="flex flex-wrap gap-x-6 gap-y-2" {
				(mode_choice("off", "Don't wait"))
				(mode_choice("http", "HTTP check"))
				(mode_choice("command", "Command check"))
			}
			span class=(HELP) {
				"With a check, a deploy starts the new container and keeps the old one serving until the check passes. "
				"If it never does, the deploy fails and the old container stays."
			}
		}
		div class="flex flex-col gap-4" "data-show"="$hc_mode !== 'off'" {
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" "data-show"="$hc_mode === 'http'" {
				label class=(LABEL) {
					"Path"
					input class={ (INPUT) " font-mono" } type="text" placeholder="/health" autocomplete="off" autocapitalize="none"
						autocorrect="off" spellcheck="false" "data-bind:hc_path";
					span class=(HELP) { "Requested on the container's port, from inside the container." }
				}
				(number("Expected status", "hc_status", "The status that means healthy, such as 200."))
			}
			label class=(LABEL) "data-show"="$hc_mode === 'command'" {
				"Command"
				input class={ (INPUT) " font-mono" } type="text" placeholder="pg_isready -q" autocomplete="off" autocapitalize="none"
					autocorrect="off" spellcheck="false" "data-bind:hc_command";
				span class=(HELP) { "Run in the container with sh -c; exit code 0 is healthy. The image needs a shell." }
			}
			div class="grid grid-cols-2 gap-4 sm:grid-cols-4" {
				(number("Interval (s)", "hc_interval", "Between checks."))
				(number("Timeout (s)", "hc_timeout", "One check may take."))
				(number("Retries", "hc_retries", "Failures in a row."))
				(number("Start period (s)", "hc_start", "Failures don't count yet."))
			}
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
				(number("Deploy waits up to (s)", "hc_wait", "Then the deploy fails and the old container stays."))
				(number("Drain the old container for (s)", "hc_drain", "It keeps serving after the new one takes over."))
			}
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
				(number("Roll back for (minutes)", "hc_window", "After a deploy, a bad release is replaced by the previous image. 0 turns it off."))
				(number("After failed checks", "hc_after", "In a row, or when the app crashes."))
			}
		}
	}
}

/// The settings as text, for someone who may not change them.
pub(super) fn summary(config: Option<&HealthConfig>) -> Markup {
	html! {
		@match config {
			None => { p class="text-sm text-muted-foreground" { "Deploys don't wait for a health check." } }
			Some(c) => {
				dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Check" }
						dd class="font-medium break-words" {
							@match c.kind {
								Kind::Http => { code class="font-mono" { "GET " (c.path) } " expects " (c.expect_status) }
								Kind::Command => { code class="font-mono" { (c.command.as_deref().unwrap_or_default()) } }
							}
						}
					}
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Deploy waits up to" }
						dd class="font-medium" { (c.gate_timeout_s) " s, then drains " (c.drain_s) " s" }
					}
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Automatic rollback" }
						dd class="font-medium" {
							@if c.rolls_back() { (c.rollback_window_min) " min, after " (c.rollback_after) " failed checks" }
							@else { "Off" }
						}
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sig(mode: &str) -> AppSignals {
		AppSignals {
			hc_mode: mode.to_owned(),
			hc_path: "/up".to_owned(),
			hc_status: "204".to_owned(),
			..AppSignals::default()
		}
	}

	#[test]
	fn health_signals_cover_every_field_the_save_bar_watches() {
		let map = signals(None);
		for field in FIELDS {
			assert!(map.contains_key(*field), "{field}");
		}
		assert_eq!(map.len(), FIELDS.len());
		assert_eq!(map["hc_mode"], "off");
		let on = signals(Some(&HealthConfig::default()));
		assert_eq!(on["hc_mode"], "http");
		assert_eq!(on["hc_interval"], "5");
	}

	#[test]
	fn health_form_off_means_no_gate_and_an_old_page_changes_nothing() {
		assert!(input(&AppSignals::default()).is_none());
		let off = input(&sig("off")).unwrap();
		assert!(off.parse().unwrap().is_none());
		let on = input(&sig("http")).unwrap().parse().unwrap().unwrap();
		assert_eq!(on.kind, Kind::Http);
		assert_eq!(on.path, "/up");
		assert_eq!(on.expect_status, 204);
		// What the form left empty takes the default.
		assert_eq!(on.gate_timeout_s, HealthConfig::default().gate_timeout_s);
	}

	#[test]
	fn health_form_refuses_what_cannot_run() {
		let mut bad = input(&sig("http")).unwrap();
		bad.path = "health".to_owned();
		assert!(bad.parse().is_err());
		let mut bad = input(&sig("http")).unwrap();
		bad.retries = "abc".to_owned();
		assert!(bad.parse().is_err());
		let mut bad = input(&sig("command")).unwrap();
		bad.command = "  ".to_owned();
		assert!(bad.parse().is_err());
		let mut bad = input(&sig("http")).unwrap();
		bad.gate_timeout_s = "1".to_owned();
		assert!(bad.parse().is_err());
	}

	#[test]
	fn health_summary_escapes_the_command() {
		let config = HealthConfig {
			kind: Kind::Command,
			command: Some("<script>alert(1)</script>".to_owned()),
			..HealthConfig::default()
		};
		let html = summary(Some(&config)).into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("&lt;script&gt;"), "{html}");
		assert!(
			summary(None)
				.into_string()
				.contains("wait for a health check")
		);
	}
}
