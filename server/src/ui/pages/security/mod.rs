//! Security pages: what image scanning found, shown where people already look.
//!
//! - the **Security tab** of an application, a database, a model or a stack (`tab`), whose
//!   page frame each area keeps and which fills the tab with [`content`];
//! - the team's **Security overview** (`overview`), every resource by risk;
//! - the small **badge** and **scan line** the headers, rows and deploy pages embed
//!   ([`header_badge`], [`row_badges`], [`scan_line`]);
//! - the **actions**: ignore a finding, remove an ignore, scan again, the SBOM download
//!   and a server's scan settings (`actions`, [`settings_card`]).
//!
//! Scanning only informs. Nothing here blocks a deploy, and a person without `read scan`
//! sees no badge and no tab. Every call goes through `scanning::service`, which checks the
//! permission and the team, so another team's id is a 404 like any missing one.

use std::collections::HashMap;

use axum::Router;
use axum::routing::{get, post};
use maud::Markup;
use time::OffsetDateTime;

use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, DatabaseId, DeploymentId, ModelId, StackId, TeamId};
use crate::scanning::service::{self, ScanView};
use crate::scanning::{Badge, BadgeState, Subject};
use crate::state::AppState;
use crate::ui::components::security_badge;
use crate::ui::extract::TeamCtx;

mod actions;
mod line;
mod overview;
mod settings;
mod tab;
#[cfg(test)]
mod tests;

pub(in crate::ui) use line::{deployment_scans, scan_line, subject_line};
pub(in crate::ui) use settings::settings_card;
pub(in crate::ui) use tab::{TabQuery, content};

const TEAM: &str = "/teams/{team_id}/security";
const PROJECT: &str = "/teams/{team_id}/projects/{project_id}/security";

pub fn router() -> Router<AppState> {
	let team = |path: &str| format!("{TEAM}{path}");
	let project = |path: &str| format!("{PROJECT}{path}");
	Router::new()
		.route(TEAM, get(overview::page))
		.route(&team("/ignores"), post(actions::ignore))
		.route(
			&team("/ignores/{ignore_id}/remove"),
			post(actions::remove_ignore),
		)
		.route(&team("/scans/{scan_id}/sbom"), get(actions::sbom))
		.route(&team("/scans/{scan_id}/status"), get(actions::scan_status))
		.route(
			&team("/deployments/{deployment_id}/line"),
			get(line::deployment_fragment),
		)
		.route(&team("/servers/{server_id}"), post(settings::update))
		.route(&project("/{kind}/{id}/scan"), post(actions::scan_again))
		.route(&project("/{kind}/{id}/line"), get(line::subject_fragment))
}

// --- urls and small pieces --------------------------------------------------------------

/// The word a URL uses for a kind of subject, and its id.
pub(in crate::ui) const fn slug(subject: Subject) -> (&'static str, i64) {
	match subject {
		Subject::Application(id) => ("application", id.0),
		Subject::Database(id) => ("database", id.0),
		Subject::Model(id) => ("model", id.0),
		Subject::Stack(id) => ("stack", id.0),
	}
}

pub(in crate::ui) fn parse_subject(kind: &str, id: i64) -> Option<Subject> {
	match kind {
		"application" => Some(Subject::Application(ApplicationId(id))),
		"database" => Some(Subject::Database(DatabaseId(id))),
		"model" => Some(Subject::Model(ModelId(id))),
		"stack" => Some(Subject::Stack(StackId(id))),
		_ => None,
	}
}

/// Where a subject's Security tab is.
pub(in crate::ui) fn tab_url(
	team: TeamId,
	project: crate::ids::ProjectId,
	subject: Subject,
) -> String {
	let base = format!("/teams/{team}/projects/{project}");
	match subject {
		Subject::Application(id) => format!("{base}/apps/{id}/security"),
		Subject::Database(id) => format!("{base}/databases/{id}/security"),
		Subject::Model(id) => format!("{base}/models/{id}?tab=security"),
		Subject::Stack(id) => format!("{base}/stacks/{id}/security"),
	}
}

/// `2026-11-18 03:00 UTC`.
pub(in crate::ui) fn when(t: OffsetDateTime) -> String {
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

/// Whether the person may see scan results at all; without it there is no tab and no badge.
pub(in crate::ui) fn can_see(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Read, Resource::Scan)
}

// --- badges -----------------------------------------------------------------------------

/// The badge of one resource's header, linked to its Security tab. Empty when the person may
/// not read scans, when nothing was scanned yet, or when the lookup fails: a badge is a
/// courtesy, never a reason for a page to fail.
pub(in crate::ui) async fn header_badge(
	s: &AppState,
	ctx: &TeamCtx,
	project: crate::ids::ProjectId,
	subject: Subject,
) -> Markup {
	let badges = row_badges(s, ctx, &[subject]).await;
	let badge = badges.get(&subject).copied().unwrap_or(Badge::NONE);
	security_badge(&badge, Some(&tab_url(ctx.team.id, project, subject)))
}

/// Badges for the resources of a list, by subject. Empty without `read scan`.
pub(in crate::ui) async fn row_badges(
	s: &AppState,
	ctx: &TeamCtx,
	subjects: &[Subject],
) -> HashMap<Subject, Badge> {
	if !can_see(ctx) || subjects.is_empty() {
		return HashMap::new();
	}
	service::badges(&s.db, ctx.user.id, ctx.team.id, subjects)
		.await
		.unwrap_or_default()
}

/// Whether a scan is still on its way, so a page keeps asking.
pub(in crate::ui) const fn scanning(badge: &Badge) -> bool {
	matches!(badge.state, BadgeState::Scanning)
}

/// What a finished or running scan says about a deployment, for [`scan_line`].
pub(in crate::ui) type DeploymentScans = HashMap<DeploymentId, ScanView>;
