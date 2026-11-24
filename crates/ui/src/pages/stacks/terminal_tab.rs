//! The stack's Terminal tab: pick a service, open a shell in its container.

use axum::extract::{Path, Query, State};
use axum::response::Response;
use maud::{Markup, html};
use serde::Deserialize;

use super::frame::{self, Tab};
use crate::components::{SELECT, Size, Variant, button_class};
use crate::error;
use crate::extract::TeamCtx;
use crate::pages::terminal::{self, allowed};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::state::AppState;
use podkit_core::terminal::service::Subject;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Params {
	service: String,
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(params): Query<Params>,
) -> Result<Markup, Response> {
	if !allowed(&ctx) {
		return Err(error::page(
			&ctx,
			&podkit_core::error::ServerError::Forbidden,
		));
	}
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

	let panel = match chosen {
		Some(service) => {
			let name = format!("{} / {service}", l.stack().name);
			terminal::panel(
				&s,
				&ctx,
				l.project.id,
				&Subject::Stack(l.stack().id, service.to_owned()),
				"service",
				&name,
				true,
			)
			.await
		}
		None => {
			terminal::panel(
				&s,
				&ctx,
				l.project.id,
				&Subject::Stack(l.stack().id, String::new()),
				"stack",
				&l.stack().name,
				false,
			)
			.await
		}
	};
	let content = html! {
		@if running.len() > 1 {
			form class="flex flex-wrap items-end gap-3" method="get" action={ (base) "/terminal" } {
				label class="flex min-w-40 flex-col gap-1 text-sm font-medium" {
					"Service"
					select class=(SELECT) name="service" "data-on:change"="el.form.requestSubmit()" {
						button { selectedcontent {} }
						@for name in &running {
							option value=(name) selected[Some(*name) == chosen] { (name) }
						}
					}
				}
				noscript { button type="submit" class=(button_class(Variant::Outline, Size::Md)) { "Open" } }
			}
		}
		(panel)
	};
	Ok(frame::page(&ctx, &l, Tab::Terminal, &content))
}
