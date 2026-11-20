//! The one-line answer a deploy page gives about its scan: "Scanning image…", then
//! "Scanned: 2 high (1 act now)". Information only; the deploy never waits for it, and a
//! failed scan says so without calling the deploy a failure.
//!
//! The line asks again every few seconds while the scan is on its way and stops when it is
//! done, so a deploy page that outlives its deployment still shows the scan finishing.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::{can_see, parse_subject, scanning, slug};
use crate::ids::{DeploymentId, ProjectId, TeamId};
use crate::scanning::service::{self, ScanView};
use crate::scanning::{Badge, BadgeState, Subject};
use crate::state::AppState;
use crate::ui::components::icon;
use crate::ui::extract::TeamCtx;
use crate::ui::status::poll;
use crate::ui::{error, sse};

/// The scans of these deployments, by deployment. Empty without `read scan` or when the
/// lookup fails: the line is a courtesy.
pub(in crate::ui) async fn deployment_scans(
	s: &AppState,
	ctx: &TeamCtx,
	ids: &[DeploymentId],
) -> HashMap<DeploymentId, ScanView> {
	if !can_see(ctx) || ids.is_empty() {
		return HashMap::new();
	}
	service::deployment_scans(&s.db, ctx.user.id, ctx.team.id, ids)
		.await
		.unwrap_or_default()
}

fn dom_id_of_deployment(id: DeploymentId) -> String {
	format!("scan-line-deployment-{id}")
}

fn dom_id_of_subject(subject: Subject) -> String {
	let (kind, id) = slug(subject);
	format!("scan-line-{kind}-{id}")
}

fn line(dom_id: &str, badge: &Badge, poll_url: &str) -> Markup {
	let text = match badge.state {
		BadgeState::None => return html! {},
		BadgeState::Scanning => "Scanning image\u{2026}".to_owned(),
		BadgeState::Failed => "Image scan failed. The deploy was not affected.".to_owned(),
		BadgeState::Clean | BadgeState::Found(_) => format!("Scanned: {}", badge.counts.summary()),
	};
	let busy = scanning(badge);
	html! {
		p id=(dom_id) class="ui-meta" role="status" {
			(poll(poll_url, busy))
			(icon(if busy { "loader-circle" } else { "shield" }))
			span { (text) }
		}
	}
}

/// The line of one deployment's scan; nothing when it has none (an older deployment, or
/// scanning off).
pub(in crate::ui) fn scan_line(ctx: &TeamCtx, id: DeploymentId, scan: Option<&ScanView>) -> Markup {
	let Some(scan) = scan else {
		return html! {};
	};
	let url = format!("/teams/{}/security/deployments/{id}/line", ctx.team.id);
	line(&dom_id_of_deployment(id), &scan.badge, &url)
}

/// The line of a whole subject (a stack's services together), for a deploy page that has no
/// deployment of its own to ask about.
pub(in crate::ui) async fn subject_line(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	subject: Subject,
) -> Markup {
	if !can_see(ctx) {
		return html! {};
	}
	let Ok(found) = service::subject_scans(&s.db, ctx.user.id, ctx.team.id, project, subject).await
	else {
		return html! {};
	};
	line(
		&dom_id_of_subject(subject),
		&found.badge,
		&subject_url(ctx.team.id, project, subject),
	)
}

fn subject_url(team: TeamId, project: ProjectId, subject: Subject) -> String {
	let (kind, id) = slug(subject);
	format!("/teams/{team}/projects/{project}/security/{kind}/{id}/line")
}

pub(super) async fn deployment_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, DeploymentId)>,
) -> Response {
	match service::deployment_scans(&s.db, ctx.user.id, ctx.team.id, &[id]).await {
		Ok(mut found) => {
			sse::patch(&scan_line(&ctx, id, found.remove(&id).as_ref())).into_response()
		}
		Err(e) => error::page(&ctx, &e),
	}
}

pub(super) async fn subject_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, kind, id)): Path<(TeamId, ProjectId, String, i64)>,
) -> Response {
	let Some(subject) = parse_subject(&kind, id) else {
		return error::page(&ctx, &crate::error::ServerError::NotFound);
	};
	match service::subject_scans(&s.db, ctx.user.id, ctx.team.id, project, subject).await {
		Ok(found) => sse::patch(&line(
			&dom_id_of_subject(subject),
			&found.badge,
			&subject_url(ctx.team.id, project, subject),
		))
		.into_response(),
		Err(e) => error::page(&ctx, &e),
	}
}

#[cfg(test)]
pub(super) fn line_for_test(badge: &Badge) -> String {
	line("scan-line-test", badge, "/poll").into_string()
}
