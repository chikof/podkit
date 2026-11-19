//! The Deployments tab (the newest job of the stack with its live log) and the Logs tab (a
//! service's container output).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::frame::{self, Loaded, Tab};
use crate::ids::{ProjectId, StackId, TeamId};
use crate::jobs::JobStatus;
use crate::stacks::service;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_OUTLINE, SELECT, Status as Shape, card, card_content, card_heading, empty_state,
	log_viewer_titled,
};
use crate::ui::extract::TeamCtx;
use crate::ui::live::{container_log, error_line, job_log};
use crate::ui::pages::apps::chrome;
use crate::ui::{error, sse};

/// The lines a Logs view may ask for.
const TAILS: [u32; 3] = [200, 1000, 5000];

fn job_badge(status: JobStatus) -> Markup {
	let (shape, text) = match status {
		JobStatus::Queued => (Shape::Pending, "Queued"),
		JobStatus::Running => (Shape::Building, "Running"),
		JobStatus::Succeeded => (Shape::Running, "Succeeded"),
		JobStatus::Failed => (Shape::Failed, "Failed"),
		JobStatus::Cancelled => (Shape::Stopped, "Cancelled"),
	};
	chrome::dot(shape, text)
}

/// The job's state, patched on its own while the stack is busy so the log stays put.
pub(super) fn job_status_line(l: &Loaded) -> Markup {
	html! {
		div id="stack-job-status" class="flex flex-wrap items-center gap-3 text-sm" {
			@if let Some((job, status)) = l.detail.job {
				(job_badge(status))
				span class="font-mono text-xs text-muted-foreground" { "job " (job.0) }
			}
		}
	}
}

/// The newest job and its live log.
pub(super) fn job_card(ctx: &TeamCtx, l: &Loaded) -> Markup {
	let base = l.base(ctx.team.id);
	html! {
		div id="stack-job" {
			@match l.detail.job {
				Some(_) => {
					div class="flex flex-col gap-4" {
						(job_status_line(l))
						(log_viewer_titled(
							"deploy-log",
							"Deploy log",
							&format!("{base}/deployments/log"),
							Some(&html! { "Latest deployment" }),
						))
					}
				}
				None => {
					(empty_state(
						"rocket",
						"Not deployed yet",
						Some("Deploy the stack and its output shows up here as it happens."),
						None,
					))
				}
			}
		}
	}
}

pub(super) async fn deployments(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	Ok(frame::page(&ctx, &l, Tab::Deployments, &job_card(&ctx, &l)))
}

pub(super) async fn deployment_log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Response {
	match service::detail(&s, ctx.user.id, ctx.team.id, project, id).await {
		Ok(d) => job_log(&s, d.job.map(|(job, _)| job), "deploy-log"),
		Err(e) => stream_failure(&e, "deploy-log"),
	}
}

/// Why a stream could not start. Not found and not allowed are plain status codes; anything
/// else (nothing running, the agent down) is written into the log itself.
fn stream_failure(e: &crate::error::ServerError, target: &str) -> Response {
	let status = e.status();
	if status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN {
		return status.into_response();
	}
	let line = error_line(&error::message(e));
	sse::patch_into(&format!("#{target}"), ElementPatchMode::Append, &line)
		.and(sse::signals(&json!({ "_live": false })))
		.into_response()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct LogParams {
	service: String,
	tail: Option<u32>,
	follow: Option<String>,
}

impl LogParams {
	fn tail(&self) -> u32 {
		self.tail.filter(|t| TAILS.contains(t)).unwrap_or(TAILS[0])
	}

	fn follow(&self) -> bool {
		self.follow.as_deref() != Some("0")
	}
}

pub(super) async fn logs(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(params): Query<LogParams>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let base = l.base(ctx.team.id);
	let running: Vec<&str> = l
		.detail
		.services
		.iter()
		.filter(|svc| svc.container_id.is_some())
		.map(|svc| svc.name.as_str())
		.collect();
	let chosen = running
		.iter()
		.copied()
		.find(|n| *n == params.service)
		.or_else(|| running.first().copied());
	let (tail, follow) = (params.tail(), params.follow());
	let content = card(&html! {
		(card_heading("Logs", Some("Output of one service's container.")))
		(card_content(&html! {
			@if let Some(service) = chosen {
				div class="flex flex-col gap-4" {
					form class="flex flex-wrap items-end gap-3" method="get" action={ (base) "/logs" } {
						label class="flex min-w-40 flex-col gap-1 text-sm font-medium" {
							"Service"
							select class=(SELECT) name="service" "data-on:change"="el.form.requestSubmit()" {
								button { selectedcontent {} }
								@for name in &running {
									option value=(name) selected[*name == service] { (name) }
								}
							}
						}
						label class="flex flex-col gap-1 text-sm font-medium" {
							"Lines"
							select class=(SELECT) name="tail" "data-on:change"="el.form.requestSubmit()" {
								button { selectedcontent {} }
								@for t in TAILS {
									option value=(t) selected[t == tail] { "Last " (t) }
								}
							}
						}
						label class="flex flex-col gap-1 text-sm font-medium" {
							"Follow"
							select class=(SELECT) name="follow" "data-on:change"="el.form.requestSubmit()" {
								button { selectedcontent {} }
								option value="1" selected[follow] { "Stream new lines" }
								option value="0" selected[!follow] { "Snapshot only" }
							}
						}
						noscript { button type="submit" class=(BUTTON_OUTLINE) { "Apply" } }
					}
					(log_viewer_titled(
						"stack-log",
						"Service log",
						&format!(
							"{base}/logs/stream?service={}&tail={tail}&follow={}",
							url::form_urlencoded::byte_serialize(service.as_bytes()).collect::<String>(),
							u8::from(follow)
						),
						Some(&html! { "Service log " span class="ui-logbar-id" { (service) } }),
					))
				}
			} @else {
				div class="flex flex-col gap-2 py-6 text-center" {
					p class="font-medium" { "No service is running." }
					p class="text-sm text-muted-foreground" { "Deploy the stack and each service's output shows up here." }
				}
			}
		}))
	});
	Ok(frame::page(&ctx, &l, Tab::Logs, &content))
}

pub(super) async fn logs_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(params): Query<LogParams>,
) -> Response {
	match service::service_logs(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		&params.service,
		params.tail(),
		params.follow(),
	)
	.await
	{
		Ok(events) => container_log(events, "stack-log", s.shutdown.clone()),
		Err(e) => stream_failure(&e, "stack-log"),
	}
}
