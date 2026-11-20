//! What a person does on the Security pages: ignore a finding, stop ignoring it, scan again,
//! download the SBOM. Each handler calls `scanning::service`, which authorizes, scopes to the
//! team and validates, so a forged request is refused exactly like a button the person may
//! not press.

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use serde::Deserialize;
use serde_json::json;

use crate::error::ServerError;
use crate::ids::{ProjectId, ScanId, ScanIgnoreId, TeamId};
use crate::scanning::service::{self, NewIgnore};
use crate::scanning::store::FindingFilter;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::{error, sse};

/// A signal as text, whether Datastar sent it as a string or a number.
fn text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match serde_json::Value::deserialize(d)? {
		serde_json::Value::String(s) => s,
		serde_json::Value::Number(n) => n.to_string(),
		_ => String::new(),
	})
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the dialog's signals, which share the `ig_` prefix"
)]
pub(super) struct IgnoreSignals {
	ig_vuln: String,
	ig_pkg: String,
	ig_scope: String,
	ig_reason: String,
	#[serde(deserialize_with = "text")]
	ig_days: String,
}

fn ignore_error(e: &ServerError) -> Response {
	sse::signals(&json!({ "_ig_error": error::message(e) })).into_response()
}

pub(super) async fn ignore(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<IgnoreSignals>,
) -> Response {
	let Ok(days) = sig.ig_days.trim().parse::<i64>() else {
		return ignore_error(&ServerError::Validation(
			"Pick how long to ignore it.".to_owned(),
		));
	};
	let new = NewIgnore {
		vuln_id: sig.ig_vuln,
		package: (sig.ig_scope == "package").then_some(sig.ig_pkg),
		reason: sig.ig_reason,
		days,
	};
	match service::ignore(&s.db, ctx.user.id, ctx.team.id, new).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => ignore_error(&e),
	}
}

pub(super) async fn remove_ignore(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ScanIgnoreId)>,
) -> Response {
	match service::remove_ignore(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => sse::reload().into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

/// The stored `CycloneDX` document, as a download.
pub(super) async fn sbom(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ScanId)>,
) -> Response {
	match service::sbom(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok((name, json)) => {
			let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
				.unwrap_or_else(|_| HeaderValue::from_static("attachment"));
			(
				StatusCode::OK,
				[
					(
						header::CONTENT_TYPE,
						HeaderValue::from_static("application/vnd.cyclonedx+json"),
					),
					(header::CONTENT_DISPOSITION, disposition),
					(
						header::X_CONTENT_TYPE_OPTIONS,
						HeaderValue::from_static("nosniff"),
					),
				],
				json,
			)
				.into_response()
		}
		Err(e) => error::page(&ctx, &e),
	}
}

/// The poll of a scan that is still running: the same page once it has finished.
pub(super) async fn scan_status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ScanId)>,
) -> Response {
	match service::findings(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		id,
		FindingFilter::default(),
	)
	.await
	{
		Ok((view, _)) if view.scan.status.in_progress() => sse::signals(&json!({})).into_response(),
		Ok(_) => sse::reload().into_response(),
		Err(e) => error::page(&ctx, &e),
	}
}

pub(super) async fn scan_again(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, kind, id)): Path<(TeamId, ProjectId, String, i64)>,
) -> Response {
	let Some(subject) = super::parse_subject(&kind, id) else {
		return sse::toast(&error::message(&ServerError::NotFound)).into_response();
	};
	match service::scan_again(&s, ctx.user.id, ctx.team.id, project, subject).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}
