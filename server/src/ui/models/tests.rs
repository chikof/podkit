//! Tests of the model pages against a stub Hugging Face hub and a fake agent
//! on the local server's socket. Nothing here touches Podman or the network.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{post, put};
use proto::{GatewayChatRequest, GatewayConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use super::{fit_label, human_bytes};
use crate::catalog::fit::fixtures::{chikos_box, cpu_512gb, gpu_24gb};
use crate::catalog::{self, suggest};
use crate::state::AppState;
use crate::tests::{
	PgPool, call, count, db_test, project_id, seed_app_and_database, seed_model, server_id,
	set_file_status, set_hardware, state, user_with_team,
};
use crate::ui::testing::{
	HOST, assert_contains, assert_not_contains, get_html, post_signals, ui_login,
};

const TOKEN_TEXT: &str = "hf_supersecrettoken123";

/// What the fake agent was asked to do.
#[derive(Default)]
struct FakeAgent {
	configs: Mutex<Vec<GatewayConfig>>,
	chats: Mutex<Vec<GatewayChatRequest>>,
	/// The `content` pieces the next chat streams.
	pieces: Mutex<Vec<String>>,
}

fn ndjson(lines: &[Value]) -> axum::response::Response {
	let body = lines.iter().fold(String::new(), |mut out, l| {
		out.push_str(&l.to_string());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

fn log_line(inner: &Value) -> Value {
	json!({ "type": "log", "stream": "stdout", "line": inner.to_string() })
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let (config, chat) = (agent.clone(), agent.clone());
	let router = Router::new()
		.route(
			"/v1/gateway/config",
			put(move |body: Bytes| async move {
				config
					.configs
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				axum::Json(json!({}))
			}),
		)
		.route(
			"/v1/gateway/chat",
			post(move |body: Bytes| async move {
				chat.chats
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				let pieces = chat.pieces.lock().unwrap().clone();
				let mut lines: Vec<Value> = pieces
					.iter()
					.map(|p| {
						let chunk = json!({ "choices": [{ "delta": { "content": p } }] });
						log_line(&json!({ "data": chunk.to_string() }))
					})
					.collect();
				let usage = json!({
					"choices": [],
					"usage": { "prompt_tokens": 5, "completion_tokens": 11 }
				});
				lines.push(log_line(&json!({ "data": usage.to_string() })));
				lines.push(log_line(&json!({ "data": "[DONE]" })));
				lines.push(log_line(&json!({ "end": true })));
				lines.push(json!({ "type": "done" }));
				ndjson(&lines)
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
	agent
}

/// A hub that has every repo, with one 2.5 GB GGUF file in it.
async fn hub() -> String {
	use axum::extract::Path;
	use axum::routing::get;
	async fn tree(Path(_): Path<(String, String, String)>) -> impl IntoResponse {
		axum::Json(json!([{
			"type": "file", "path": "Qwen3-4B-Q4_K_M.gguf", "size": 133,
			"lfs": { "oid": "x", "size": 2_497_280_256_u64 },
		}]))
	}
	let router = Router::new().route("/api/models/{owner}/{name}/tree/{rev}", get(tree));
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
	format!("http://{addr}")
}

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	agent: Arc<FakeAgent>,
	token: String,
	cookie: String,
	team: i64,
	project: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let mut state = state(db.clone());
		std::sync::Arc::make_mut(&mut state.config).hf_endpoint = hub().await;
		let agent = fake_agent(&state);
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let cookie = ui_login(&app, "alice").await;
		let (status, _) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "ai", "slug": "ai" })),
		)
		.await;
		assert!(status.is_success());
		let project = project_id(&db, "ai").await;
		set_hardware(&db, team, &chikos_box()).await;
		Self {
			app,
			state,
			db,
			agent,
			token,
			cookie,
			team,
			project,
		}
	}

	fn model_url(&self, model: i64) -> String {
		format!(
			"/teams/{}/projects/{}/models/{model}",
			self.team, self.project
		)
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	async fn post(&self, uri: &str, signals: &Value) -> String {
		post_signals(&self.app, uri, Some(&self.cookie), signals)
			.await
			.2
	}

	/// A running model with a container, ready to chat with.
	async fn running_model(&self) -> (i64, i64) {
		seed_model(
			&self.db,
			self.team,
			self.project,
			"Qwen chat",
			"running",
			Some("c1"),
		)
		.await
	}

	/// Adds `name` as a member with the read-only Member role; returns their cookie.
	async fn member(&self, name: &str) -> String {
		user_with_team(&self.app, name).await;
		let (s, _) = call(
			&self.app,
			"POST",
			&format!("/api/v1/teams/{}/members", self.team),
			Some(&self.token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
		ui_login(&self.app, name).await
	}
}

/// A form post of the Hugging Face token, as the settings page sends it.
async fn post_token(app: &Router, cookie: &str, team: i64, uri_tail: &str, form: &str) -> String {
	let req = axum::http::Request::post(format!("/teams/{team}/models/settings/token{uri_tail}"))
		.header(header::HOST, HOST)
		.header(header::ORIGIN, format!("http://{HOST}"))
		.header(header::COOKIE, cookie)
		.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
		.header("datastar-request", "true")
		.body(axum::body::Body::from(form.to_owned()))
		.unwrap();
	let resp = app.clone().oneshot(req).await.unwrap();
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	String::from_utf8(bytes.to_vec()).unwrap()
}

/// What maud writes for `s` in text and attributes.
fn esc(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}

fn card_of<'a>(body: &'a str, id: &str, server: i64) -> &'a str {
	let marker = format!("/catalog/{id}?server={server}\"");
	let start = body
		.find(&marker)
		.unwrap_or_else(|| panic!("no card for {id} in the page"));
	let rest = &body[start..];
	&rest[..rest.find("Set up").unwrap()]
}

#[db_test]
async fn the_catalog_groups_cards_by_tier_and_filters_by_size_and_tag(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let server = server_id(&db, s.team).await;
	let list = format!("/teams/{}/models/catalog", s.team);

	let (status, body) = s.get(&list).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "S tier");
	assert_contains(&body, "Rate models against");
	for model in catalog::models() {
		assert_contains(&body, &format!("/catalog/{}?server={server}\"", model.id));
	}

	let (_, tiny) = s.get(&format!("{list}?size=tiny")).await;
	let (_, coding) = s.get(&format!("{list}?tag=coding&size=large")).await;
	for model in catalog::models() {
		let href = format!("/catalog/{}?server={server}\"", model.id);
		assert_eq!(
			tiny.contains(&href),
			model.size_class == catalog::SizeClass::Tiny,
			"{} under size=tiny",
			model.id
		);
		assert_eq!(
			coding.contains(&href),
			model.size_class == catalog::SizeClass::Large
				&& model.tags.contains(&catalog::Tag::Coding),
			"{} under tag=coding&size=large",
			model.id
		);
	}
	assert_contains(&tiny, "aria-current=\"true\"");
}

#[db_test]
async fn fit_badges_match_catalog_fit_for_the_selected_server(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let server = server_id(&db, s.team).await;
	let list = format!("/teams/{}/models/catalog?server={server}", s.team);

	for (name, hardware) in [
		("chiko's box", chikos_box()),
		("a 24 GB card", gpu_24gb()),
		("a big CPU box", cpu_512gb()),
	] {
		set_hardware(&db, s.team, &hardware).await;
		let (status, body) = s.get(&list).await;
		assert_eq!(status, StatusCode::OK);
		for model in catalog::models() {
			let (recipe, fit) = suggest(&hardware, model).remove(0);
			let card = card_of(&body, &model.id, server);
			let (label, _) = fit_label(fit.kind);
			assert!(
				card.contains(label),
				"{} on {name}: {label}\n{card}",
				model.id
			);
			assert!(
				card.contains(&format!("title=\"{}\"", esc(&fit.explanation))),
				"{} on {name} lacks its explanation",
				model.id
			);
			assert!(card.contains(&recipe.quant), "{} on {name}", model.id);
			assert!(card.contains(&human_bytes(recipe.total_bytes)));
		}
	}

	// Chiko's 12 GB card cannot hold everything: the catalog has both verdicts.
	set_hardware(&db, s.team, &chikos_box()).await;
	let (_, body) = s.get(&list).await;
	assert_contains(&body, "Fits on GPU");
	assert!(
		body.contains("Won&#39;t fit") || body.contains("Won't fit"),
		"some model should not fit 12 GB"
	);
}

#[db_test]
async fn without_hardware_the_page_says_so_and_rates_nothing(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	crate::tests::clear_hardware(&db).await;
	let (status, body) = s.get(&format!("/teams/{}/models/catalog", s.team)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "hasn't reported its hardware yet");
	assert_not_contains(&body, "Fits on GPU");
	assert_contains(&body, "No server picked");
}

#[db_test]
async fn the_setup_page_lists_recipes_with_fit_and_download_size(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let server = server_id(&db, s.team).await;
	let (status, body) = s
		.get(&format!(
			"/teams/{}/models/catalog/qwen3-4b?server={server}",
			s.team
		))
		.await;
	assert_eq!(status, StatusCode::OK);
	let model = catalog::model("qwen3-4b").unwrap();
	for (recipe, fit) in suggest(&chikos_box(), model) {
		assert_contains(&body, &recipe.quant);
		assert_contains(
			&body,
			&format!("Download {}", human_bytes(recipe.total_bytes)),
		);
		assert_contains(&body, &esc(&fit.explanation));
	}
	assert_contains(&body, "Overrides");
	assert_contains(&body, "Context size");

	let (status, _) = s
		.get(&format!("/teams/{}/models/catalog/no-such-model", s.team))
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

fn file_id_in(body: &str) -> i64 {
	let rest = &body[body.find("/downloads/").expect("a download link") + "/downloads/".len()..];
	rest[..rest.find(['?', '\'']).unwrap()].parse().unwrap()
}

#[db_test]
async fn download_then_start_runs_the_whole_create_flow(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let server = server_id(&db, s.team).await;
	let team = s.team;

	let body = s
		.post(
			&format!("/teams/{team}/models/downloads/recipe"),
			&json!({
				"catalog": "qwen3-4b", "recipe": "q4-k-m", "server": server.to_string(),
				"project": s.project.to_string(), "name": "My Qwen", "served": "", "ctx": "4096",
			}),
		)
		.await;
	assert_contains(&body, "window.location.assign('/teams/");
	assert_contains(&body, "engine=llama_cuda");
	assert_contains(&body, "path=Qwen3-4B-Q4_K_M.gguf");
	assert_contains(&body, "name=My%20Qwen");
	assert_eq!(count(&db, "model_files").await, 1);
	let file = file_id_in(&body);
	let query = body[body.find('?').unwrap()..body.rfind('\'').unwrap()].to_owned();

	// A second click opens the same download.
	let again = s
		.post(
			&format!("/teams/{team}/models/downloads/recipe"),
			&json!({
				"catalog": "qwen3-4b", "recipe": "q4-k-m", "server": server.to_string(),
				"project": s.project.to_string(), "name": "My Qwen",
			}),
		)
		.await;
	assert_eq!(file_id_in(&again), file);
	assert_eq!(count(&db, "model_files").await, 1);

	// The download page polls while it runs and has no start form yet.
	let page_url = format!("/teams/{team}/models/downloads/{file}{query}");
	let (status, page) = s.get(&page_url).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&page, "Queued");
	assert_contains(&page, "role=\"progressbar\"");
	assert_contains(&page, "data-on-interval__duration.3s");
	assert_contains(&page, "/log");
	assert_not_contains(&page, "Start the model");

	set_file_status(&db, file, "downloading", 1_248_640_128).await;
	let progress_url = format!("/teams/{team}/models/downloads/{file}/progress{query}");
	let (_, progress) = s.get(&progress_url).await;
	assert_contains(&progress, "Downloading");
	assert_contains(&progress, "50%");
	assert_contains(&progress, "1.2 GB of 2.5 GB");

	// When the files are ready polling stops and the start form shows what was chosen.
	set_file_status(&db, file, "ready", 2_497_280_256).await;
	let (_, ready) = s.get(&progress_url).await;
	assert_contains(&ready, "Ready");
	assert_contains(&ready, "Start the model");
	assert_not_contains(&ready, "data-on-interval");
	assert_contains(&ready, "My\\u0020Qwen");
	assert_contains(&ready, "llama.cpp (CUDA)");

	let started = s
		.post(
			&format!("/teams/{team}/models/downloads/{file}/start"),
			&json!({
				"project": s.project.to_string(), "name": "My Qwen",
				"engine": "llama_cuda", "path": "Qwen3-4B-Q4_K_M.gguf", "ctx": "4096", "served": "",
			}),
		)
		.await;
	assert_contains(
		&started,
		&format!("/teams/{team}/projects/{}/models/", s.project),
	);
	assert_eq!(count(&db, "models").await, 1);

	let bad = s
		.post(
			&format!("/teams/{team}/models/downloads/{file}/start"),
			&json!({
				"project": s.project.to_string(), "name": "x", "engine": "nope", "ctx": "",
			}),
		)
		.await;
	assert_contains(&bad, "Pick an engine.");
	assert_eq!(count(&db, "models").await, 1);
}

#[db_test]
async fn a_failed_download_shows_why_and_the_files_in_use_cannot_be_deleted(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (file, _) = seed_model(&db, s.team, s.project, "Seed", "stopped", None).await;
	crate::tests::fail_file(&db, file, "disk full <b>now</b>").await;
	let (_, page) = s
		.get(&format!("/teams/{}/models/downloads/{file}", s.team))
		.await;
	assert_contains(&page, "Failed");
	assert_contains(&page, "disk full &lt;b&gt;now&lt;/b&gt;");
	assert_contains(&page, "Retry");
	assert_not_contains(&page, "Start the model");

	// A model still serves these files, so they can't go.
	let body = s
		.post(
			&format!("/teams/{}/models/downloads/{file}/delete", s.team),
			&json!({}),
		)
		.await;
	assert_contains(&body, "serves these files");
	assert_eq!(count(&db, "model_files").await, 1);

	let body = s
		.post(
			&format!("/teams/{}/models/downloads/{file}/cancel", s.team),
			&json!({}),
		)
		.await;
	assert_contains(&body, "nothing is downloading");
}

#[db_test]
async fn the_model_page_shows_how_it_runs_and_its_actions(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	let (status, page) = s.get(&s.model_url(model)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&page, "Qwen chat");
	assert_contains(&page, "Running");
	assert_contains(&page, "llama.cpp (CPU)");
	assert_contains(&page, &format!("served-{model}"));
	assert_contains(&page, &format!("http://model-slug-{model}:8080/v1"));
	assert_contains(&page, "4096 tokens");
	assert_contains(&page, "Stop");
	assert_contains(&page, "Restart");
	assert_contains(&page, "Remove");
	assert_contains(&page, "Playground");
	assert_contains(&page, "/logs");
	assert_not_contains(&page, "data-on-interval");

	// Stopped: no chat. Starting: polls, with a hint.
	crate::tests::set_model_status(&db, model, "stopped", None).await;
	let (_, page) = s.get(&s.model_url(model)).await;
	assert_contains(&page, "Stopped");
	assert_contains(&page, "Start the model to chat with it.");
	crate::tests::set_model_status(&db, model, "starting", None).await;
	let status_url = format!("{}/status?logs=1", s.model_url(model));
	let (_, fragment) = s.get(&status_url).await;
	assert_contains(&fragment, "Starting");
	assert_contains(&fragment, "Loading the weights");
	assert_contains(&fragment, "data-on-interval__duration.3s");
	assert_contains(&fragment, "id=\"model-status\"");

	// A failure shows the engine's last words, escaped, and stops polling.
	crate::tests::set_model_status(&db, model, "failed", Some("boom <script>alert(1)</script>"))
		.await;
	let (_, fragment) = s.get(&status_url).await;
	assert_contains(&fragment, "Failed");
	assert_contains(&fragment, "boom &lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(&fragment, "<script>alert(1)");
	assert_not_contains(&fragment, "data-on-interval");

	// Once the engine has a container the poll also swaps in its own log.
	let (_, fragment) = s
		.get(&format!("{}/status?logs=0", s.model_url(model)))
		.await;
	assert_contains(&fragment, "model-log-card");
}

#[db_test]
async fn actions_change_the_model_and_refuse_what_cannot_work(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	let url = s.model_url(model);

	let body = s.post(&format!("{url}/start"), &json!({})).await;
	assert_contains(&body, "only a stopped or failed model can be started");

	let body = s.post(&format!("{url}/restart"), &json!({})).await;
	assert_contains(&body, "Starting");
	assert_contains(&body, "data-on-interval__duration.3s");

	let body = s.post(&format!("{url}/remove"), &json!({})).await;
	assert_contains(&body, "Removing");
	let body = s.post(&format!("{url}/remove"), &json!({})).await;
	assert_contains(&body, "already being removed");
}

fn urlencode(s: &str) -> String {
	s.bytes()
		.map(|b| {
			if b.is_ascii_alphanumeric() {
				char::from(b).to_string()
			} else {
				format!("%{b:02X}")
			}
		})
		.collect()
}

async fn gateway_key(s: &Setup, app: i64) -> String {
	let env = crate::links::env(&s.state, crate::ids::ApplicationId(app))
		.await
		.unwrap();
	env.into_iter()
		.find(|(k, _)| k == "OPENAI_API_KEY")
		.map(|(_, v)| v)
		.unwrap()
}

#[db_test]
async fn linking_previews_five_variables_with_the_key_masked(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	seed_app_and_database(&db, s.team, s.project).await;
	let url = s.model_url(model);
	let preview_url = format!(
		"{url}/link-preview?datastar={}",
		urlencode(&json!({ "linkApp": "5001" }).to_string())
	);

	let (_, page) = s.get(&url).await;
	assert_contains(&page, "Link an application");
	assert_contains(&page, "Storefront");
	assert_contains(&page, "No application uses this model yet.");

	let (_, preview) = s.get(&preview_url).await;
	for var in [
		"OPENAI_BASE_URL",
		"OPENAI_API_KEY",
		"ANTHROPIC_BASE_URL",
		"ANTHROPIC_API_KEY",
		"PODKIT_MODEL",
	] {
		assert_contains(&preview, var);
	}
	assert_contains(&preview, "http://llm.podkit.internal:8088/v1");
	assert_contains(&preview, &format!("served-{model}"));
	assert_contains(&preview, crate::links::service::MASKED_KEY);
	assert_eq!(
		count(&db, "gateway_keys").await,
		0,
		"a preview issues nothing"
	);

	let linked = s
		.post(
			&format!("{url}/link"),
			&json!({ "linkApp": "5001", "linkDefault": false }),
		)
		.await;
	assert_contains(&linked, "Linked.");
	assert_contains(&linked, "id=\"model-links\"");
	assert_contains(&linked, "Unlink");
	assert_eq!(count(&db, "application_models").await, 1);
	assert_eq!(count(&db, "gateway_keys").await, 1);

	// The real key is nowhere in what the page or the actions sent.
	let key = gateway_key(&s, 5001).await;
	let (_, page) = s.get(&url).await;
	let (_, preview_again) = s.get(&preview_url).await;
	for text in [&page, &linked, &preview, &preview_again] {
		assert_not_contains(text, &key);
	}
	assert_contains(&page, "Storefront");
	assert_contains(&page, "default");

	let unlinked = s.post(&format!("{url}/unlink?app=5001"), &json!({})).await;
	assert_contains(&unlinked, "Unlinked.");
	assert_eq!(count(&db, "application_models").await, 0);
	let again = s.post(&format!("{url}/unlink?app=5001"), &json!({})).await;
	assert_contains(&again, "That no longer exists.");
}

#[db_test]
async fn the_playground_escapes_model_output_and_shows_speed_and_usage(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	let url = s.model_url(model);
	*s.agent.pieces.lock().unwrap() = vec![
		"<script>alert(1)</script>".to_owned(),
		" and <img src=x onerror=alert(2)>".to_owned(),
	];

	let (_, page) = s.get(&url).await;
	assert_contains(&page, "Playground");
	assert_contains(&page, "pg-thread");

	let body = s
		.post(
			&format!("{url}/chat"),
			&json!({ "prompt": "say <b>hi</b>", "chat": [] }),
		)
		.await;
	assert_not_contains(&body, "<script>alert(1)");
	assert_not_contains(&body, "<img src=x");
	assert_not_contains(&body, "<b>hi</b>");
	assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_contains(&body, "&lt;img src=x onerror=alert(2)&gt;");
	assert_contains(&body, "say &lt;b&gt;hi&lt;/b&gt;");
	assert_contains(&body, "pg-ans-0");
	assert_contains(&body, "11 tokens");
	assert_contains(&body, "5 in, 11 out");
	// The exchange comes back as the conversation to send next time.
	assert_contains(&body, "\"chat\"");
	assert_contains(&body, "assistant");

	// The gateway got the model's served name and a stream request.
	let chats = s.agent.chats.lock().unwrap().clone();
	assert_eq!(chats.len(), 1);
	let sent: Value = serde_json::from_str(&chats[0].body).unwrap();
	assert_eq!(sent["model"], format!("served-{model}"));
	assert_eq!(sent["stream"], true);
	assert_eq!(sent["messages"][0]["content"], "say <b>hi</b>");
	assert_not_contains(&body, &chats[0].key);
	assert_not_contains(&page, &chats[0].key);

	// The key is the model's own, and the routing table the agent got knows it.
	assert_eq!(count(&db, "playground_keys").await, 1);
	let configs = s.agent.configs.lock().unwrap().clone();
	let hash = crate::links::hash_key(&chats[0].key);
	let key = configs
		.iter()
		.flat_map(|c| &c.keys)
		.find(|k| k.key_hash == hash)
		.expect("the playground key reached the agent");
	assert_eq!(key.models.len(), 1);
	assert_eq!(key.models[0].name, format!("served-{model}"));

	// A second message reuses the key and carries the history.
	let history = json!([
		{ "role": "user", "content": "say <b>hi</b>" },
		{ "role": "assistant", "content": "ok" },
	]);
	let body = s
		.post(
			&format!("{url}/chat"),
			&json!({ "prompt": "again", "chat": history }),
		)
		.await;
	assert_contains(&body, "pg-ans-1");
	assert_eq!(count(&db, "playground_keys").await, 1);
	let chats = s.agent.chats.lock().unwrap().clone();
	let sent: Value = serde_json::from_str(&chats[1].body).unwrap();
	assert_eq!(sent["messages"].as_array().unwrap().len(), 3);
	assert_eq!(chats[0].key, chats[1].key);
}

#[db_test]
async fn the_playground_refuses_a_stopped_model_and_a_bad_conversation(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	let url = format!("{}/chat", s.model_url(model));

	let body = s.post(&url, &json!({ "prompt": "   ", "chat": [] })).await;
	assert_contains(&body, "1 to 8000 characters");
	let body = s
		.post(
			&url,
			&json!({ "prompt": "hi", "chat": [{ "role": "system", "content": "x" }] }),
		)
		.await;
	assert_contains(&body, "from the user or the assistant");

	crate::tests::set_model_status(&db, model, "stopped", None).await;
	let body = s.post(&url, &json!({ "prompt": "hi", "chat": [] })).await;
	assert_contains(&body, "not running");
	assert_eq!(s.agent.chats.lock().unwrap().len(), 0);
	assert_eq!(count(&db, "playground_keys").await, 0);
}

#[tokio::test]
async fn the_stream_renderer_escapes_whatever_the_model_says() {
	use super::playground::respond;
	use crate::links::playground::ChatEvent;
	use futures_util::StreamExt;

	let events = futures_util::stream::iter([
		ChatEvent::Thinking("<i>hm</i>".to_owned()),
		ChatEvent::Token("<script>alert('x')</script>".to_owned()),
		ChatEvent::Error("<b>broken</b>".to_owned()),
	])
	.boxed();
	let resp = respond(
		CancellationToken::new(),
		"<u>hi</u>".to_owned(),
		Vec::new(),
		events,
	);
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	let text = String::from_utf8(bytes.to_vec()).unwrap();
	for raw in ["<script>", "<i>hm", "<b>broken", "<u>hi"] {
		assert_not_contains(&text, raw);
	}
	assert_contains(&text, "&lt;script&gt;alert(");
	assert_contains(&text, "&lt;b&gt;broken&lt;/b&gt;");
	// A failed answer is not added to the conversation.
	assert_not_contains(&text, "\"chat\"");
}

#[db_test]
async fn the_hugging_face_token_is_write_only(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let settings = format!("/teams/{}/models/settings", s.team);

	let (status, page) = s.get(&settings).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&page, "Not set");
	assert_not_contains(&page, "Remove");

	// A form post, not Datastar signals: the token is in no signal.
	let saved = post_token(
		&s.app,
		&s.cookie,
		s.team,
		"",
		&format!("token={TOKEN_TEXT}"),
	)
	.await;
	assert_not_contains(&saved, TOKEN_TEXT);
	assert_contains(&saved, "window.location.assign");
	assert_eq!(count(&db, "huggingface_tokens").await, 1);

	let (_, page) = s.get(&settings).await;
	assert_contains(&page, "Configured");
	assert_contains(&page, "Replace");
	assert_not_contains(&page, TOKEN_TEXT);
	for uri in [
		format!("/teams/{}/models", s.team),
		format!("/teams/{}/models/catalog", s.team),
	] {
		let (_, body) = s.get(&uri).await;
		assert_not_contains(&body, TOKEN_TEXT);
	}

	let removed = s
		.post(&format!("{settings}/token/remove"), &json!({}))
		.await;
	assert_contains(&removed, "Not set");
	assert_not_contains(&removed, TOKEN_TEXT);
	assert_eq!(count(&db, "huggingface_tokens").await, 0);

	let bad = post_token(&s.app, &s.cookie, s.team, "", "token=has%20space").await;
	assert_contains(&bad, "invalid token");
}

#[db_test]
async fn the_models_page_lists_models_and_downloads_and_the_sidebar_links_to_it(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (file, model) = s.running_model().await;
	set_file_status(&db, file, "downloading", 1_000_000_000).await;

	let (status, page) = s.get(&format!("/teams/{}/models", s.team)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&page, "Qwen chat");
	assert_contains(&page, &s.model_url(model));
	assert_contains(&page, "acme/open");
	assert_contains(&page, "50%");
	assert_contains(&page, "Browse the catalog");

	// Every page of the shell has the entry.
	let (_, projects) = s.get(&format!("/teams/{}/projects", s.team)).await;
	assert_contains(&projects, &format!("href=\"/teams/{}/models\"", s.team));
}

#[db_test]
async fn another_teams_model_pages_are_the_404_page(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (file, model) = s.running_model().await;
	user_with_team(&s.app, "bob").await;
	let bob = ui_login(&s.app, "bob").await;
	let bob_team = crate::tests::team_id(&db, "bob").await;

	for uri in [
		format!("/teams/{}/models", s.team),
		format!("/teams/{}/models/catalog", s.team),
		format!("/teams/{}/models/settings", s.team),
		format!("/teams/{}/models/downloads/{file}", s.team),
		s.model_url(model),
		// Bob's own team, Alice's ids.
		format!("/teams/{bob_team}/models/downloads/{file}"),
		format!("/teams/{bob_team}/projects/{}/models/{model}", s.project),
	] {
		let (st, _, body) = get_html(&s.app, &uri, Some(&bob)).await;
		assert_eq!(st, StatusCode::NOT_FOUND, "{uri}");
		assert_not_contains(&body, "Qwen chat");
		assert_not_contains(&body, "acme/open");
	}
	// Polls are actions, not pages: they answer in-page, and a model that is gone
	// (or was never yours) sends the browser to the list.
	let (_, _, body) = get_html(
		&s.app,
		&format!(
			"/teams/{bob_team}/projects/{}/models/{model}/status",
			s.project
		),
		Some(&bob),
	)
	.await;
	assert_contains(
		&body,
		&format!("window.location.assign('/teams/{bob_team}/models')"),
	);
	assert_not_contains(&body, "Qwen chat");
	let (_, _, body) = get_html(
		&s.app,
		&format!("/teams/{bob_team}/models/downloads/{file}/progress"),
		Some(&bob),
	)
	.await;
	assert_contains(&body, "That no longer exists.");
	assert_not_contains(&body, "acme/open");
	for (uri, signals) in [
		(format!("{}/stop", s.model_url(model)), json!({})),
		(
			format!(
				"/teams/{bob_team}/projects/{}/models/{model}/stop",
				s.project
			),
			json!({}),
		),
		(
			format!(
				"/teams/{bob_team}/projects/{}/models/{model}/chat",
				s.project
			),
			json!({ "prompt": "hi", "chat": [] }),
		),
		(
			format!("/teams/{bob_team}/models/downloads/{file}/delete"),
			json!({}),
		),
		(
			format!("/teams/{bob_team}/models/downloads/{file}/start"),
			json!({
				"project": s.project.to_string(), "name": "x", "engine": "llama_cpu",
			}),
		),
	] {
		let (_, _, body) = post_signals(&s.app, &uri, Some(&bob), &signals).await;
		assert_contains(&body, "That no longer exists.");
	}
	assert_eq!(count(&db, "models").await, 1);
	assert_eq!(count(&db, "model_files").await, 1);
	assert_eq!(count(&db, "playground_keys").await, 0);
}

#[db_test]
async fn a_read_only_member_sees_no_actions_and_forged_posts_fail(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (file, model) = s.running_model().await;
	seed_app_and_database(&db, s.team, s.project).await;
	let carol = s.member("carol").await;
	let url = s.model_url(model);

	let (st, _, page) = get_html(&s.app, &url, Some(&carol)).await;
	assert_eq!(st, StatusCode::OK);
	assert_contains(&page, "Qwen chat");
	assert_contains(&page, "Running");
	for hidden in [
		"/stop",
		"/restart",
		"/remove",
		"/chat",
		"Link an application",
		"Send",
	] {
		assert_not_contains(&page, hidden);
	}

	let (_, _, body) = get_html(
		&s.app,
		&format!("/teams/{}/models/catalog/qwen3-4b", s.team),
		Some(&carol),
	)
	.await;
	assert_contains(&body, "Qwen3 4B");
	assert_not_contains(&body, ">Download</button>");
	let (_, _, settings) = get_html(
		&s.app,
		&format!("/teams/{}/models/settings", s.team),
		Some(&carol),
	)
	.await;
	assert_not_contains(&settings, "type=\"password\"");

	let server = server_id(&db, s.team).await;
	let denied = "You don&#39;t have permission to do that.";
	let denied_raw = "You don't have permission to do that.";
	for (uri, signals) in [
		(format!("{url}/stop"), json!({})),
		(format!("{url}/restart"), json!({})),
		(format!("{url}/remove"), json!({})),
		(format!("{url}/chat"), json!({ "prompt": "hi", "chat": [] })),
		(
			format!("{url}/link"),
			json!({ "linkApp": "5001", "linkDefault": false }),
		),
		(
			format!("/teams/{}/models/downloads/{file}/delete", s.team),
			json!({}),
		),
		(
			format!("/teams/{}/models/downloads/recipe", s.team),
			json!({
				"catalog": "qwen3-4b", "recipe": "q4-k-m", "server": server.to_string(),
				"project": s.project.to_string(), "name": "x",
			}),
		),
		(
			format!("/teams/{}/models/downloads/{file}/start", s.team),
			json!({
				"project": s.project.to_string(), "name": "x", "engine": "llama_cpu",
			}),
		),
	] {
		let (_, _, body) = post_signals(&s.app, &uri, Some(&carol), &signals).await;
		assert!(
			body.contains(denied) || body.contains(denied_raw),
			"{uri}: {body}"
		);
	}
	assert_eq!(count(&db, "model_files").await, 1);
	assert_eq!(count(&db, "application_models").await, 0);
	assert_eq!(count(&db, "playground_keys").await, 0);

	// The token form is refused too.
	let refused = post_token(&s.app, &carol, s.team, "", &format!("token={TOKEN_TEXT}")).await;
	assert!(
		refused.contains(denied) || refused.contains(denied_raw),
		"{refused}"
	);
	assert_eq!(count(&db, "huggingface_tokens").await, 0);
}

#[db_test]
async fn usage_shows_each_key_over_24_hours_and_7_days(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, model) = s.running_model().await;
	seed_app_and_database(&db, s.team, s.project).await;
	let url = s.model_url(model);
	let (_, page) = s.get(&url).await;
	assert_contains(&page, "Nothing has used this model in the last 7 days.");

	s.post(
		&format!("{url}/link"),
		&json!({ "linkApp": "5001", "linkDefault": false }),
	)
	.await;
	let app_key = crate::links::hash_key(&gateway_key(&s, 5001).await);
	crate::tests::seed_usage(&db, &app_key, model, 2, 3, 100, 40, Some(5001)).await;
	crate::tests::seed_usage(&db, &app_key, model, 90, 4, 10, 5, Some(5001)).await;
	// The playground's own key, once there is one.
	*s.agent.pieces.lock().unwrap() = vec!["ok".to_owned()];
	s.post(
		&format!("{url}/chat"),
		&json!({ "prompt": "hi", "chat": [] }),
	)
	.await;
	let play_key = crate::tests::playground_hash(&db, model).await;
	crate::tests::seed_usage(&db, &play_key, model, 1, 1, 7, 9, None).await;
	// A removed key's traffic, and another model's.
	crate::tests::seed_usage(&db, "deadbeef", model, 1, 6, 1, 1, None).await;
	crate::tests::seed_usage(
		&db,
		&app_key,
		model + 1,
		1,
		50,
		123_456_789,
		123_456_789,
		Some(5001),
	)
	.await;

	let (_, page) = s.get(&url).await;
	assert_contains(&page, "Storefront");
	assert_contains(&page, "Playground");
	assert_contains(&page, "Removed key");
	// Storefront: 3 requests and 100 / 40 tokens in 24 h, 7 requests and 110 / 45 in 7 d.
	assert_contains(&page, "<td class=\"py-2 pr-4\">3</td>");
	assert_contains(&page, "100 / 40");
	assert_contains(&page, "110 / 45");
	assert_not_contains(&page, "123456789");
}

#[test]
fn model_and_download_statuses_have_badges() {
	use crate::model_files::ModelFileStatus;
	use crate::models::ModelStatus;
	use crate::ui::status::{Status, status_badge};

	for status in [
		ModelStatus::Starting,
		ModelStatus::Running,
		ModelStatus::Stopped,
		ModelStatus::Failed,
		ModelStatus::Removing,
	] {
		assert!(status_badge(status).0.contains(status.label()));
	}
	assert!(ModelStatus::Starting.in_flight() && !ModelStatus::Running.in_flight());
	assert!(ModelStatus::Removing.in_flight() && !ModelStatus::Failed.in_flight());
	for status in [
		ModelFileStatus::Queued,
		ModelFileStatus::Downloading,
		ModelFileStatus::Ready,
		ModelFileStatus::Failed,
	] {
		assert!(status_badge(status).0.contains(status.label()));
	}
	assert!(ModelFileStatus::Downloading.in_flight() && !ModelFileStatus::Ready.in_flight());
}
