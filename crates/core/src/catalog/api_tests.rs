//! API tests: the catalog and the Hugging Face search against a stub hub.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;

use crate::catalog::fit::fixtures::{chikos_box, cpu_512gb};
use crate::tests::{call, state, user_with_team};

fn lfs(path: &str, size: u64) -> Value {
	json!({ "type": "file", "path": path, "size": 133, "lfs": { "oid": "x", "size": size } })
}

async fn search(Query(q): Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
	assert_eq!(q.get("filter").map(String::as_str), Some("gguf"));
	assert_eq!(q.get("sort").map(String::as_str), Some("downloads"));
	match q.get("search").map(String::as_str) {
		Some("qwen") => axum::Json(json!([
			{ "id": "acme/small-GGUF", "downloads": 900, "likes": 5 },
			{ "id": "acme/gated-GGUF", "downloads": 100, "likes": 1 },
			{ "id": "not a repo", "downloads": 1 },
		]))
		.into_response(),
		Some("down") => StatusCode::SERVICE_UNAVAILABLE.into_response(),
		_ => axum::Json(json!([])).into_response(),
	}
}

async fn tree(Path((owner, name, _rev)): Path<(String, String, String)>) -> impl IntoResponse {
	match format!("{owner}/{name}").as_str() {
		"acme/small-GGUF" => axum::Json(json!([
			lfs("small-Q4_K_M.gguf", 2_500_000_000),
			lfs("small-Q8_0.gguf", 4_400_000_000),
			lfs("mmproj-F16.gguf", 900_000_000),
			{ "type": "file", "path": "README.md", "size": 10 },
		]))
		.into_response(),
		_ => StatusCode::UNAUTHORIZED.into_response(),
	}
}

async fn hub() -> String {
	let app = Router::new()
		.route("/api/models", get(search))
		.route("/api/models/{owner}/{name}/tree/{rev}", get(tree));
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr: SocketAddr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	format!("http://{addr}")
}

struct Setup {
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	server: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let mut state = state(db.clone());
		std::sync::Arc::make_mut(&mut state.config).hf_endpoint = hub().await;
		let app = crate::routes::routes(state).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let server = servers[0]["id"].as_i64().unwrap();
		Self {
			app,
			db,
			token,
			team,
			server,
		}
	}

	async fn set_hardware(&self, hardware: &proto::Hardware) {
		sqlx::query("UPDATE servers SET hardware = $2 WHERE id = $1")
			.bind(self.server)
			.bind(Json(hardware))
			.execute(&self.db)
			.await
			.unwrap();
	}

	async fn get(&self, uri: &str) -> (StatusCode, Value) {
		call(&self.app, "GET", uri, Some(&self.token), None).await
	}
}

fn entry<'a>(list: &'a Value, id: &str) -> &'a Value {
	list.as_array()
		.unwrap()
		.iter()
		.find(|m| m["id"] == id)
		.unwrap_or_else(|| panic!("{id} is not listed"))
}

#[sqlx::test]
async fn the_catalog_lists_models_with_tier_size_and_recipes(db: PgPool) {
	let t = Setup::new(db).await;
	let (s, list) = t.get("/api/v1/catalog").await;
	assert_eq!(s, StatusCode::OK);
	assert!(list.as_array().unwrap().len() >= 15);
	let m = entry(&list, "qwen3-5-4b");
	assert_eq!(m["tier"], "A");
	assert_eq!(m["size_class"], "Tiny");
	assert_eq!(m["name"], "Qwen3.5 4B");
	assert!(m["tags"].as_array().unwrap().contains(&json!("vision")));
	assert!(!m["recipes"].as_array().unwrap().is_empty());
	assert!(m.get("best").is_none(), "no server asked, so no fit");
	assert!(entry(&list, "qwen3-8-flash-next").get("tier").is_none());
	assert_eq!(entry(&list, "qwen3-4b")["recipes"][0]["verified"], true);

	let (_, only_s) = t.get("/api/v1/catalog?tier=S").await;
	assert_eq!(only_s.as_array().unwrap().len(), 5);
	let (_, tiny_coding) = t.get("/api/v1/catalog?size_class=Tiny&tag=reasoning").await;
	assert!(
		tiny_coding
			.as_array()
			.unwrap()
			.iter()
			.all(|m| m["size_class"] == "Tiny")
	);
	let (s, _) = t.get("/api/v1/catalog?tier=Z").await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn the_catalog_needs_a_login(db: PgPool) {
	let t = Setup::new(db).await;
	for uri in [
		"/api/v1/catalog",
		"/api/v1/catalog/qwen3-5-4b",
		"/api/v1/huggingface/search?q=x",
	] {
		let (s, _) = call(&t.app, "GET", uri, None, None).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}: no token");
		let (s, _) = call(&t.app, "GET", uri, Some("bogus"), None).await;
		assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}: bad token");
	}
}

#[sqlx::test]
async fn a_model_comes_with_its_recipes_ranked_for_a_server(db: PgPool) {
	let t = Setup::new(db).await;
	t.set_hardware(&chikos_box()).await;
	let (s, m) = t
		.get(&format!(
			"/api/v1/catalog/gpt-oss-20b?server_id={}",
			t.server
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	let first = &m["suggestions"][0];
	assert_eq!(first["id"], "mxfp4");
	assert_eq!(first["fit"]["kind"], "gpu_plus_ram");
	assert_eq!(first["fit"]["engine"], "llama_cuda");
	assert!(
		first["fit"]["explanation"]
			.as_str()
			.unwrap()
			.contains("RAM")
	);
	assert_eq!(first["total_bytes"], 12_109_566_624_u64);

	let (_, m) = t
		.get(&format!(
			"/api/v1/catalog/deepseek-v4-1-flash?server_id={}",
			t.server
		))
		.await;
	for suggestion in m["suggestions"].as_array().unwrap() {
		assert_eq!(suggestion["fit"]["kind"], "does_not_fit");
		assert!(
			suggestion["fit"]["explanation"]
				.as_str()
				.unwrap()
				.contains("disk")
		);
	}

	let (_, list) = t
		.get(&format!("/api/v1/catalog?server_id={}", t.server))
		.await;
	assert_eq!(
		entry(&list, "qwen3-5-4b")["best"]["fit"]["kind"],
		"gpu_full"
	);
	assert_eq!(entry(&list, "qwen3-5-4b")["best"]["recipe_id"], "q8-0");

	t.set_hardware(&cpu_512gb()).await;
	let (_, m) = t
		.get(&format!(
			"/api/v1/catalog/qwen3-5-397b?server_id={}",
			t.server
		))
		.await;
	assert_eq!(m["suggestions"][0]["fit"]["kind"], "cpu_only");
	assert_eq!(m["suggestions"][0]["fit"]["engine"], "llama_cpu");

	let (s, m) = t.get("/api/v1/catalog/qwen3-5-4b").await;
	assert_eq!(s, StatusCode::OK);
	assert!(m.get("suggestions").is_none());
}

#[sqlx::test]
async fn unknown_models_and_servers_are_not_found(db: PgPool) {
	let t = Setup::new(db).await;
	t.set_hardware(&chikos_box()).await;
	let (s, _) = t.get("/api/v1/catalog/nope").await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _) = t.get("/api/v1/catalog/qwen3-5-4b?server_id=12345").await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn another_teams_server_is_not_found(db: PgPool) {
	let t = Setup::new(db).await;
	t.set_hardware(&chikos_box()).await;
	let (bob, _) = user_with_team(&t.app, "bob").await;
	for uri in [
		format!("/api/v1/catalog?server_id={}", t.server),
		format!("/api/v1/catalog/qwen3-5-4b?server_id={}", t.server),
		format!("/api/v1/huggingface/search?q=qwen&server_id={}", t.server),
	] {
		let (s, _) = call(&t.app, "GET", &uri, Some(&bob), None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
	}
	let _ = t.team;
}

#[sqlx::test]
async fn a_server_without_a_hardware_snapshot_is_a_conflict(db: PgPool) {
	let t = Setup::new(db).await;
	sqlx::query("UPDATE servers SET hardware = NULL WHERE id = $1")
		.bind(t.server)
		.execute(&t.db)
		.await
		.unwrap();
	let (s, body) = t
		.get(&format!("/api/v1/catalog?server_id={}", t.server))
		.await;
	assert_eq!(s, StatusCode::CONFLICT);
	assert!(body["error"].as_str().unwrap().contains("hardware"));
}

#[sqlx::test]
async fn hugging_face_search_lists_variants_and_rates_them(db: PgPool) {
	let t = Setup::new(db).await;
	t.set_hardware(&chikos_box()).await;
	let (s, body) = t
		.get(&format!(
			"/api/v1/huggingface/search?q=qwen&server_id={}",
			t.server
		))
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let hits = body["hits"].as_array().unwrap();
	assert_eq!(hits.len(), 2, "the invalid repo id is dropped");
	assert_eq!(hits[0]["repo"], "acme/small-GGUF");
	assert_eq!(hits[0]["downloads"], 900);
	let variants = hits[0]["variants"].as_array().unwrap();
	assert_eq!(variants.len(), 2, "mmproj and README are not variants");
	assert_eq!(variants[0]["quant"], "Q4_K_M");
	assert_eq!(variants[0]["total_bytes"], 2_500_000_000_u64);
	assert_eq!(variants[0]["selection"]["files"][0], "small-Q4_K_M.gguf");
	assert_eq!(variants[0]["fit"]["kind"], "gpu_full");
	assert_eq!(variants[1]["quant"], "Q8_0");
	// A repo whose files cannot be listed is reported, not fatal.
	assert_eq!(hits[1]["repo"], "acme/gated-GGUF");
	assert!(hits[1]["variants"].as_array().unwrap().is_empty());
	assert!(hits[1]["error"].as_str().unwrap().contains("token"));

	let (_, plain) = t.get("/api/v1/huggingface/search?q=qwen").await;
	assert!(plain["hits"][0]["variants"][0].get("fit").is_none());
}

#[sqlx::test]
async fn hugging_face_search_validates_and_maps_upstream_failures(db: PgPool) {
	let t = Setup::new(db).await;
	for uri in [
		"/api/v1/huggingface/search?q=",
		"/api/v1/huggingface/search?q=%20%20",
		"/api/v1/huggingface/search",
	] {
		let (s, _) = t.get(uri).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}");
	}
	let long = "a".repeat(101);
	let (s, _) = t.get(&format!("/api/v1/huggingface/search?q={long}")).await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
	let (s, _) = t.get("/api/v1/huggingface/search?q=down").await;
	assert_eq!(s, StatusCode::BAD_GATEWAY);
	let (s, body) = t.get("/api/v1/huggingface/search?q=nothing").await;
	assert_eq!(s, StatusCode::OK);
	assert!(body["hits"].as_array().unwrap().is_empty());
}
