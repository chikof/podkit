//! The team's Security overview: every resource that can be scanned, the riskiest first.
//! A **Monitor** surface: what is the state, which resource needs a look. It lists; the
//! resource's own Security tab has the findings.

use axum::extract::State;
use axum::response::Response;
use maud::{Markup, html};

use super::tab_url;
use crate::components::{empty_state, icon, security_badge, table};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::app_page;
use podkit_core::scanning::BadgeState;
use podkit_core::scanning::service::{self, OverviewRow};
use podkit_core::state::AppState;

const fn kind_icon(label: &str) -> &'static str {
	// `const fn` cannot compare strings; the labels are the four the service returns.
	match label.as_bytes() {
		b"Application" => "git-branch",
		b"Database" => "database",
		b"Model" => "brain",
		_ => "layers",
	}
}

fn row(ctx: &TeamCtx, r: &OverviewRow) -> Vec<Markup> {
	let href = tab_url(ctx.team.id, r.project_id, r.subject);
	vec![
		html! { a class="font-medium underline-offset-4 hover:underline" href=(href) { (r.name) } },
		html! { span class="inline-flex items-center gap-2" { (icon(kind_icon(r.kind_label))) (r.kind_label) } },
		html! { (r.project_name) },
		html! {
			@if r.badge.state == BadgeState::None {
				span class="text-muted-foreground" { "Not scanned yet" }
			} @else {
				(security_badge(&r.badge, Some(&href)))
			}
		},
	]
}

/// A sentence for the top of the page: how many need attention first.
fn summary(rows: &[OverviewRow]) -> String {
	let act = rows.iter().filter(|r| r.badge.counts.act_now > 0).count();
	let scanned = rows
		.iter()
		.filter(|r| r.badge.state != BadgeState::None)
		.count();
	match (act, scanned) {
		(0, 0) => "Nothing has been scanned yet.".to_owned(),
		(0, n) => format!("{n} scanned, none that needs action now."),
		(a, n) => format!("{a} of {n} scanned need action now."),
	}
}

pub(super) async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let rows = service::overview(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let cells: Vec<Vec<Markup>> = rows.iter().map(|r| row(&ctx, r)).collect();
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Security" }
					p class="ui-lede" {
						"Every image is scanned when it deploys and again each day. A finding warns; it never blocks a deploy. "
						(summary(&rows))
					}
				}
			}
			@if cells.is_empty() {
				(empty_state(
					"shield",
					"Nothing to scan yet",
					Some("Applications, databases, models and stacks show up here once they exist."),
					None,
				))
			} @else {
				(table(
					"security-overview",
					"Resources by security risk",
					&["Resource", "Type", "Project", "Security"],
					&cells,
				))
			}
		}
	};
	Ok(app_page(&ctx, "Security", &[("Security", None)], &body))
}
