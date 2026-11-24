//! The replicas of a scaled application as the pages show them: a row of status chips under the
//! header, and the table with restarts, CPU and memory on Monitoring.
//!
//! A status is never colour alone: each state has its dot shape and always a word beside it.
//! Nothing here reads the database or the agent: `replicas::service::status` hands over
//! [`Live`].

use maud::{Markup, html};

use super::chrome;
use crate::components::{Status as Shape, table};
use podkit_core::replicas::service::Live;
use podkit_core::replicas::status::{ReplicaRow, ReplicaState};

/// The dot shape of a replica's state.
#[must_use]
pub fn shape(state: ReplicaState) -> Shape {
	match state {
		ReplicaState::Healthy | ReplicaState::Running => Shape::Running,
		ReplicaState::Starting => Shape::Deploying,
		ReplicaState::Restarting => Shape::Pending,
		ReplicaState::Unhealthy | ReplicaState::Crashed => Shape::Failed,
		ReplicaState::Stopped => Shape::Stopped,
	}
}

/// The verdict over all replicas as one badge: `3 of 3 up`, failing when one is not.
fn verdict(live: &Live) -> Markup {
	let shape = if live.rollup.complete() {
		Shape::Running
	} else if live.rollup.up > 0 {
		Shape::Pending
	} else {
		Shape::Failed
	};
	chrome::dot(shape, &live.rollup.label())
}

fn cpu(row: &ReplicaRow) -> String {
	row.cpu
		.map_or_else(|| "\u{2014}".to_owned(), |c| format!("{c:.1}%"))
}

fn memory(row: &ReplicaRow) -> String {
	row.mem.map_or_else(
		|| "\u{2014}".to_owned(),
		|m| chrome::bytes(i64::try_from(m).unwrap_or(i64::MAX)),
	)
}

/// The compact list under the header: the verdict, then one chip per replica.
#[must_use]
pub fn chips(live: &Live) -> Markup {
	html! {
		div class="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm" role="group" aria-label="Replicas" {
			span class="text-muted-foreground" { "Replicas" }
			(verdict(live))
			@for row in &live.rows {
				(chrome::dot(shape(row.state), &format!("{} {}", row.number, row.state.label())))
			}
		}
	}
}

/// `#replicas`: the heading, the verdict and the table of every replica, for Monitoring.
/// `refresh` is the Datastar endpoint that answers with a patch of this section; the section
/// asks for it every 30 seconds while the tab is visible.
#[must_use]
pub fn section(live: &Live, refresh: &str) -> Markup {
	let rows: Vec<Vec<Markup>> = live
		.rows
		.iter()
		.map(|row| {
			vec![
				html! { "Replica " (row.number) " " code class="font-mono text-muted-foreground" { (row.name) } },
				chrome::dot(shape(row.state), row.state.label()),
				html! { (row.restarts) },
				html! { (cpu(row)) },
				html! { (memory(row)) },
			]
		})
		.collect();
	let on_interval = format!(
		"document.hidden || @get({})",
		crate::sse::js_string(refresh)
	);
	html! {
		section id="replicas" aria-labelledby="replicas-title" class="flex flex-col gap-4"
			"data-on-interval__duration.30s"=(on_interval) {
			div class="ui-range-bar" {
				h2 id="replicas-title" class="ui-sec-h" { "Replicas" }
				(verdict(live))
			}
			@if live.rows.is_empty() {
				p class="text-sm text-muted-foreground" {
					"No replica is running yet. They appear here once a deploy has started them."
				}
			} @else {
				(table(
					"replicas-table",
					"Replicas of this application",
					&["Replica", "Status", "Restarts", "CPU", "Memory"],
					&rows,
				))
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use podkit_core::replicas::ReplicaConfig;
	use podkit_core::replicas::status::rollup;

	fn row(number: usize, state: ReplicaState) -> ReplicaRow {
		ReplicaRow {
			number,
			name: format!("podkit-app-1-9-r{number}"),
			state,
			restarts: u32::try_from(number).unwrap(),
			cpu: Some(1.25),
			mem: Some(50_000_000),
		}
	}

	fn live(rows: Vec<ReplicaRow>, wanted: i32) -> Live {
		let rollup = rollup(usize::try_from(wanted).unwrap(), &rows);
		Live {
			config: ReplicaConfig {
				replicas: wanted,
				..ReplicaConfig::default()
			},
			rows,
			rollup,
		}
	}

	#[test]
	fn replicas_chips_name_every_replica_with_a_word() {
		let live = live(
			vec![row(1, ReplicaState::Healthy), row(2, ReplicaState::Crashed)],
			2,
		);
		let html = chips(&live).into_string();
		assert!(html.contains("1 of 2 up"), "{html}");
		assert!(html.contains("1 Healthy"), "{html}");
		assert!(html.contains("2 Crashed"), "{html}");
		assert!(html.contains("data-status=\"failed\""), "{html}");
	}

	#[test]
	fn replicas_table_shows_restarts_cpu_and_memory() {
		let live = live(vec![row(1, ReplicaState::Running)], 1);
		let html = section(&live, "/x/monitoring/replicas").into_string();
		assert!(html.contains("Restarts"), "{html}");
		assert!(html.contains("1.2%") || html.contains("1.3%"), "{html}");
		assert!(html.contains("50.0 MB"), "{html}");
		assert!(html.contains("podkit-app-1-9-r1"), "{html}");
		assert_eq!(html.matches("<h1").count(), 0);
		// Nothing running yet is said, not an empty table.
		let none = section(&live_empty(), "/x").into_string();
		assert!(none.contains("No replica is running yet"), "{none}");
		assert!(!none.contains("<table"), "{none}");
	}

	fn live_empty() -> Live {
		live(Vec::new(), 3)
	}
}
