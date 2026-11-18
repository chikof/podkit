//! Stepper: where the person is in a short sequence (model, server, recipe, ...). An
//! ordered list, so the order is announced; the current step carries `aria-current="step"`.
//! Steps before it read as done through CSS (`:has(~ [aria-current])`), so state is a
//! number fill and weight, and the labels stay real text in the DOM.
//!
//! The current step is either fixed by the server (`Current::At`) or follows a Datastar
//! signal that holds the 1-based step (`Current::Signal`), for a dialog that moves
//! between its steps without a page load.

use maud::{Markup, html};

pub enum Current<'a> {
	/// 1-based index of the current step, rendered by the server.
	At(usize),
	/// Name of a signal holding the 1-based current step (without the `$`).
	Signal(&'a str),
}

pub fn stepper(label: &str, steps: &[&str], current: &Current<'_>) -> Markup {
	html! {
		ol class="ui-steps" aria-label=(label) {
			@for (i, step) in steps.iter().enumerate() {
				@let n = i + 1;
				@match current {
					Current::At(at) => {
						li class="ui-step" aria-current=[(*at == n).then_some("step")] {
							span class="ui-step-num" aria-hidden="true" { (n) }
							span { (step) }
						}
					}
					Current::Signal(signal) => {
						li class="ui-step" "data-attr:aria-current"={ "$" (signal) " == " (n) " ? 'step' : null" } {
							span class="ui-step-num" aria-hidden="true" { (n) }
							span { (step) }
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

	#[test]
	fn exactly_the_current_step_is_marked_and_the_list_is_named() {
		let html = stepper(
			"Create a model",
			&["Model", "Server", "Start"],
			&Current::At(2),
		)
		.into_string();
		assert_eq!(html.matches("aria-current=\"step\"").count(), 1, "{html}");
		assert!(
			html.contains("<ol class=\"ui-steps\" aria-label=\"Create a model\">"),
			"{html}"
		);
		let (_, after) = html.split_once("aria-current=\"step\"").unwrap();
		assert!(after.contains(">Server<"), "{html}");
		assert!(!after.starts_with(">Model"), "{html}");
	}

	#[test]
	fn a_signal_driven_stepper_binds_each_step_and_escapes_labels() {
		let html = stepper("x", &["<i>a</i>", "b"], &Current::Signal("step")).into_string();
		assert!(
			html.contains("data-attr:aria-current=\"$step == 1 ? 'step' : null\""),
			"{html}"
		);
		assert!(
			html.contains("data-attr:aria-current=\"$step == 2 ? 'step' : null\""),
			"{html}"
		);
		assert!(!html.contains("<i>"), "{html}");
	}
}
