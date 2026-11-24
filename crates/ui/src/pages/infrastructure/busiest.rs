//! "Busiest resources" on the Infrastructure overview: the few resources of the team that use
//! the most CPU or memory, answer the most 5xx responses, or restart the most. It reads
//! `metrics::service::busiest`, which scopes to the team and to the kinds the person may read.

use maud::{Markup, html};

use super::overview::{kind_label, resource_href};
use crate::charts::{Unit, format_value};
use crate::extract::TeamCtx;
use podkit_core::metrics::overview::{Busiest, Busy};

fn row(ctx: &TeamCtx, r: &Busy, figure: &str) -> Markup {
	html! {
		li {
			a href=(resource_href(ctx.team.id, r.project_id, &r.kind, r.id)) {
				(r.name) small { (kind_label(&r.kind)) }
			}
			b { (figure) }
		}
	}
}

fn list(ctx: &TeamCtx, title: &str, rows: &[Busy], figure: impl Fn(&Busy) -> String) -> Markup {
	html! {
		div class="ui-busy-card" {
			h3 { (title) }
			@if rows.is_empty() {
				p class="ui-busy-none" { "Nothing to show." }
			} @else {
				ol class="ui-busy-list" {
					@for r in rows { (row(ctx, r, &figure(r))) }
				}
			}
		}
	}
}

/// The share of requests that were 5xx, for the figure next to the rate.
fn error_figure(r: &Busy) -> String {
	let rate = r.r5xx_rate.unwrap_or(0.0);
	let rate_text = format_value(Unit::Rate("req/s"), rate);
	match r.req_rate.filter(|q| *q > 0.0) {
		Some(req) => format!(
			"{rate_text} · {}",
			format_value(Unit::Percent, rate * 100.0 / req)
		),
		None => rate_text,
	}
}

/// The section, or nothing when no resource has a figure to show.
pub(super) fn section(ctx: &TeamCtx, top: &Busiest) -> Markup {
	html! {
		@if !top.is_empty() {
			section class="ui-infra-where" aria-labelledby="infra-busy" {
				h2 id="infra-busy" class="ui-infra-h2" { "Busiest resources" }
				p class="ui-lede" { "Averages over the last hour; restarts over the last 24 hours." }
				div class="ui-busy" {
					(list(ctx, "CPU", &top.by_cpu, |r| format_value(Unit::Percent, r.cpu.unwrap_or(0.0))))
					(list(ctx, "Memory", &top.by_memory, |r| format_value(Unit::Bytes, r.mem.unwrap_or(0.0))))
					(list(ctx, "5xx responses", &top.by_errors, error_figure))
					(list(ctx, "Crashes and restarts", &top.by_restarts, |r| r.restarts.to_string()))
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn busy(name: &str) -> Busy {
		Busy {
			kind: "application".to_owned(),
			id: 7,
			project_id: 3,
			name: name.to_owned(),
			cpu: Some(12.34),
			mem: Some(256.0 * 1024.0 * 1024.0),
			req_rate: Some(20.0),
			r5xx_rate: Some(1.0),
			restarts: 3,
		}
	}

	#[test]
	fn the_error_figure_is_a_rate_and_a_share() {
		assert_eq!(error_figure(&busy("a")), "1 req/s · 5%");
		let quiet = Busy {
			req_rate: None,
			..busy("a")
		};
		assert_eq!(error_figure(&quiet), "1 req/s");
	}
}
