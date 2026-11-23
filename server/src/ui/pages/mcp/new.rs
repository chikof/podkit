//! The form that makes an MCP server: an image or a git repository, how it is reached,
//! and the server it runs on. Templates are the other way in; a link says so.

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::{mcp_url, new_url};
use crate::applications::service as applications;
use crate::error::ServerError;
use crate::ids::{EnvironmentId, ProjectId, ServerId, TeamId};
use crate::mcp_servers::Transport;
use crate::mcp_servers::service::{self, CreateInput, Settings};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	Size, Variant, button_class, card, card_content, card_section, checkbox_field, link_button,
	select_field, text_field, textarea_field,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/mcp/new",
		get(page).post(create),
	)
}

#[derive(Deserialize, Default)]
struct NewQuery {
	environment: Option<i64>,
}

fn group(title: &str, description: Option<&str>, fields: &Markup) -> Markup {
	card(&html! {
		(card_section(title, description, None))
		(card_content(&html! { div class="flex flex-col gap-4" { (fields) } }))
	})
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers = applications::servers_for_create(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;
	let environment = q.environment.map(|e| e.to_string()).unwrap_or_default();
	let back = format!("/teams/{}/projects/{}", ctx.team.id, project.id);
	let templates = format!(
		"/teams/{}/projects/{}/templates?q=mcp{}",
		ctx.team.id,
		project.id,
		if environment.is_empty() {
			String::new()
		} else {
			format!("&environment={environment}")
		}
	);
	let server_options: Vec<(String, String)> = servers
		.iter()
		.map(|s| (s.id.0.to_string(), s.name.clone()))
		.collect();
	let server_options: Vec<(&str, &str)> = server_options
		.iter()
		.map(|(v, t)| (v.as_str(), t.as_str()))
		.collect();
	let post = format!(
		"evt.preventDefault(); @post({}, {{contentType: 'form'}})",
		crate::ui::sse::js_string(&format!(
			"{}{}",
			new_url(ctx.team.id, project.id),
			if environment.is_empty() {
				String::new()
			} else {
				format!("?environment={environment}")
			}
		))
	);
	let body = html! {
		div class="flex flex-col gap-6" data-signals="{_error: '', _busy: false}" {
			div class="flex flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "New MCP server" }
				p class="text-sm text-muted-foreground" {
					"A server that gives agents tools. Run an image or a repository here, then link it to an agent application; it is never exposed to the internet. Or "
					a class="underline" href=(templates) { "start from a template" } "."
				}
			}
			form class="flex flex-col gap-6" "data-on:submit"=(post) {
				(group("Server", None, &html! {
					(text_field("mcp-name", "name", "Name", "text", None, Some("It is also the name in the gateway URL."), None))
					(select_field("mcp-server", "server_id", "Runs on", &server_options, server_options.first().map(|(v, _)| *v), None, None))
				}))
				(group("Source", Some("Fill in one. A private image or repository takes its credentials on the application's page afterwards."), &html! {
					(text_field("mcp-image", "image", "Image", "text", None, Some("For example ghcr.io/acme/mcp-server:1.2"), None))
					(text_field("mcp-repo", "repo_url", "Git repository", "url", None, Some("Built from its Dockerfile on the main branch."), None))
				}))
				(group("How it is reached", None, &html! {
					(select_field("mcp-transport", "transport", "Transport", &[("stdio", "stdio, wrapped by podkit's bridge"), ("http", "Streamable HTTP, served by the image")], Some("stdio"), None, None))
					(text_field("mcp-port", "container_port", "Port", "number", Some("8080"), Some("What the server, or the bridge for a stdio server, listens on."), None))
					(text_field("mcp-path", "path", "Endpoint path", "text", Some("/mcp"), None, None))
					(textarea_field("mcp-command", "command", "Command", None, Some("stdio only: one argument per line. Empty runs the image's own command."), None))
				}))
				(checkbox_field("mcp-deploy", "deploy", "Deploy it now", true, None))
				(form_error())
				div class="flex flex-wrap items-center justify-end gap-2" {
					(link_button(Variant::Outline, Size::Md, &back, &html! { "Cancel" }))
					button type="submit" class=(button_class(Variant::Default, Size::Md))
						"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create MCP server" }
				}
			}
		}
	};
	Ok(app_page(
		&ctx,
		"New MCP server",
		&[
			("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
			(project.name.as_str(), Some(back)),
			("New MCP server", None),
		],
		&body,
	))
}

#[derive(Deserialize)]
struct Fields {
	name: String,
	server_id: String,
	#[serde(default)]
	image: String,
	#[serde(default)]
	repo_url: String,
	#[serde(default)]
	transport: String,
	#[serde(default)]
	container_port: String,
	#[serde(default)]
	path: String,
	#[serde(default)]
	command: String,
	#[serde(default)]
	deploy: Option<String>,
}

fn input(fields: Fields, environment: Option<i64>) -> Result<CreateInput, ServerError> {
	let bad = |m: &str| ServerError::Validation(m.to_owned());
	let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
	let port = match some(&fields.container_port) {
		Some(p) => Some(p.parse::<i32>().map_err(|_| bad("The port is a number."))?),
		None => None,
	};
	Ok(CreateInput {
		name: fields.name,
		slug: None,
		server_id: fields
			.server_id
			.parse::<i64>()
			.map(ServerId)
			.map_err(|_| bad("Pick a server."))?,
		environment_id: environment.map(EnvironmentId),
		image: some(&fields.image),
		repo_url: some(&fields.repo_url),
		git_ref: None,
		dockerfile_path: None,
		container_port: port,
		settings: Settings {
			transport: Transport::parse(&fields.transport)
				.ok_or_else(|| bad("Pick a transport."))?,
			path: some(&fields.path),
			command: fields
				.command
				.lines()
				.map(str::trim)
				.filter(|l| !l.is_empty())
				.map(str::to_owned)
				.collect(),
		},
		deploy: fields.deploy.is_some(),
	})
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
	Form(fields): Form<Fields>,
) -> Response {
	let result = async {
		let input = input(fields, q.environment)?;
		service::create(&s, ctx.user.id, ctx.team.id, project, input).await
	}
	.await;
	match result {
		Ok(created) => {
			sse::redirect(&mcp_url(ctx.team.id, project, created.app.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}
