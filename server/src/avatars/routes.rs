use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Serialize;

use super::process::MAX_UPLOAD;
use super::{Stored, service, team_url, user_url};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::state::AppState;

/// The request body may be the picture plus the multipart framing around it.
const BODY_LIMIT: usize = MAX_UPLOAD + 64 * 1024;

/// `PUT`/`DELETE` of a team's or the caller's picture, nested under `/api/v1`.
pub fn api_router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/avatar",
			put(put_team)
				.delete(delete_team)
				.layer(DefaultBodyLimit::max(BODY_LIMIT)),
		)
		.route(
			"/me/avatar",
			put(put_me)
				.delete(delete_me)
				.layer(DefaultBodyLimit::max(BODY_LIMIT)),
		)
}

/// The pictures themselves, at `/avatars/...`.
pub fn public_router() -> Router<AppState> {
	Router::new()
		.route("/avatars/team/{team_id}", get(serve_team))
		.route("/avatars/user/{user_id}", get(serve_user))
}

/// The `file` part of a multipart upload: its declared type and its bytes.
///
/// # Errors
/// `PayloadTooLarge` when the body is over the limit, `Validation` when there is no file.
pub async fn read_upload(mut form: Multipart) -> AppResult<(Option<String>, Vec<u8>)> {
	loop {
		let field = match form.next_field().await {
			Ok(Some(field)) => field,
			Ok(None) => {
				return Err(ServerError::Validation(
					"Choose a picture to upload.".to_owned(),
				));
			}
			Err(e) => return Err(upload_error(&e)),
		};
		if field.name() != Some("file") {
			continue;
		}
		let declared = field.content_type().map(str::to_owned);
		let bytes = field.bytes().await.map_err(|e| upload_error(&e))?;
		return Ok((declared, bytes.to_vec()));
	}
}

fn upload_error(e: &axum::extract::multipart::MultipartError) -> ServerError {
	if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
		ServerError::PayloadTooLarge("The picture is over 2 MB. Choose a smaller one.".to_owned())
	} else {
		ServerError::Validation("The upload could not be read.".to_owned())
	}
}

#[derive(Serialize)]
struct AvatarResponse {
	/// Where the picture is served, with its cache-busting hash.
	url: String,
}

async fn put_team(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	form: Multipart,
) -> AppResult<Json<AvatarResponse>> {
	let (declared, bytes) = read_upload(form).await?;
	let hash = service::set_team(&s.db, user.id, team, declared.as_deref(), &bytes).await?;
	Ok(Json(AvatarResponse {
		url: team_url(team, &hash),
	}))
}

async fn delete_team(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<StatusCode> {
	service::clear_team(&s.db, user.id, team).await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn put_me(
	State(s): State<AppState>,
	user: AuthUser,
	form: Multipart,
) -> AppResult<Json<AvatarResponse>> {
	let (declared, bytes) = read_upload(form).await?;
	let hash = service::set_user(&s.db, user.id, user.id, declared.as_deref(), &bytes).await?;
	Ok(Json(AvatarResponse {
		url: user_url(user.id, &hash),
	}))
}

async fn delete_me(State(s): State<AppState>, user: AuthUser) -> AppResult<StatusCode> {
	service::clear_user(&s.db, user.id, user.id).await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn serve_team(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> Response {
	match service::team_image(&s.db, user.id, team).await {
		Ok(image) => image_response(image),
		Err(e) => not_found_or(e),
	}
}

async fn serve_user(
	State(s): State<AppState>,
	user: AuthUser,
	Path(target): Path<UserId>,
) -> Response {
	match service::user_image(&s.db, user.id, target).await {
		Ok(image) => image_response(image),
		Err(e) => not_found_or(e),
	}
}

/// An empty 404 for a missing or hidden picture, the error's own answer otherwise.
fn not_found_or(e: ServerError) -> Response {
	if matches!(e, ServerError::NotFound) {
		StatusCode::NOT_FOUND.into_response()
	} else {
		e.into_response()
	}
}

/// The picture with a year of caching. The URL carries the hash, so a new picture is a new
/// URL. `private` because the answer depends on who asks.
fn image_response(image: Stored) -> Response {
	let mut resp = Response::new(Body::from(image.bytes));
	let headers = resp.headers_mut();
	if let Ok(v) = HeaderValue::from_str(&image.mime) {
		headers.insert(header::CONTENT_TYPE, v);
	}
	headers.insert(
		header::CACHE_CONTROL,
		HeaderValue::from_static("private, max-age=31536000, immutable"),
	);
	if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", image.hash)) {
		headers.insert(header::ETAG, v);
	}
	headers.insert(
		header::X_CONTENT_TYPE_OPTIONS,
		HeaderValue::from_static("nosniff"),
	);
	headers.insert(
		header::CONTENT_SECURITY_POLICY,
		HeaderValue::from_static("default-src 'none'; sandbox"),
	);
	headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
	resp
}
