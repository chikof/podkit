//! HTTP handlers of the catalog slice. They only translate: the rules live in
//! [`super::service`], the verdicts in [`super::fit`].

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::search::{self, DEFAULT_LIMIT, Hit, MAX_LIMIT};
use super::{Fit, Model, Recipe, SizeClass, Tag, Tier, service, suggest};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::ServerId;
use crate::state::AppState;

/// `/api/v1/catalog`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list))
		.route("/{model_id}", get(show))
}

/// `/api/v1/huggingface`.
pub fn hf_router() -> Router<AppState> {
	Router::new().route("/search", get(hf_search))
}

#[derive(Deserialize)]
struct ServerQuery {
	/// Rate the recipes against this server's hardware.
	server_id: Option<ServerId>,
	tier: Option<Tier>,
	size_class: Option<SizeClass>,
	tag: Option<Tag>,
}

#[derive(Serialize)]
struct Best {
	recipe_id: String,
	fit: Fit,
}

#[derive(Serialize)]
struct ListEntry<'a> {
	#[serde(flatten)]
	model: &'a Model,
	/// The best recipe for the asked server; absent without `server_id`.
	#[serde(skip_serializing_if = "Option::is_none")]
	best: Option<Best>,
}

async fn list(
	State(s): State<AppState>,
	auth: AuthUser,
	Query(q): Query<ServerQuery>,
) -> AppResult<Json<Vec<ListEntry<'static>>>> {
	let hardware = service::hardware(&s, auth.id, q.server_id).await?;
	let models = super::models()
		.iter()
		.filter(|m| q.tier.is_none_or(|t| m.tier == Some(t)))
		.filter(|m| q.size_class.is_none_or(|c| m.size_class == c))
		.filter(|m| q.tag.is_none_or(|t| m.tags.contains(&t)))
		.map(|model| ListEntry {
			model,
			best: hardware.as_ref().and_then(|hw| {
				suggest(hw, model).into_iter().next().map(|(r, fit)| Best {
					recipe_id: r.id,
					fit,
				})
			}),
		})
		.collect();
	Ok(Json(models))
}

#[derive(Serialize)]
struct Suggestion {
	#[serde(flatten)]
	recipe: Recipe,
	fit: Fit,
}

#[derive(Serialize)]
struct Detail<'a> {
	#[serde(flatten)]
	model: &'a Model,
	/// The recipes rated for the asked server, best first; absent without `server_id`.
	#[serde(skip_serializing_if = "Option::is_none")]
	suggestions: Option<Vec<Suggestion>>,
}

async fn show(
	State(s): State<AppState>,
	auth: AuthUser,
	Path(model_id): Path<String>,
	Query(q): Query<ServerQuery>,
) -> AppResult<Json<Detail<'static>>> {
	let model = super::model(&model_id).ok_or(ServerError::ModelNotFound)?;
	let hardware = service::hardware(&s, auth.id, q.server_id).await?;
	let suggestions = hardware.map(|hw| {
		suggest(&hw, model)
			.into_iter()
			.map(|(recipe, fit)| Suggestion { recipe, fit })
			.collect()
	});
	Ok(Json(Detail { model, suggestions }))
}

#[derive(Deserialize)]
struct SearchQuery {
	q: String,
	limit: Option<usize>,
	server_id: Option<ServerId>,
}

#[derive(Serialize)]
struct SearchResponse {
	hits: Vec<Hit>,
}

async fn hf_search(
	State(s): State<AppState>,
	auth: AuthUser,
	Query(q): Query<SearchQuery>,
) -> AppResult<Json<SearchResponse>> {
	let query = q.q.trim();
	if query.is_empty() || query.chars().count() > 100 || query.chars().any(char::is_control) {
		return Err(ServerError::Validation(
			"q must be 1 to 100 characters without control characters".to_owned(),
		));
	}
	let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
	let hardware = service::hardware(&s, auth.id, q.server_id).await?;
	let hits = search::search(&s, query, limit, hardware.as_ref()).await?;
	Ok(Json(SearchResponse { hits }))
}
