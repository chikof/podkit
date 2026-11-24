//! Pictures: what an upload may be, what is stored, who may see it and who may change it.

use axum::http::{StatusCode, header};
use image::ImageFormat;

use super::process::{SIZE, process};
use super::service;
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::tests::avatars::{Who, add_member, multipart, picture, png, send, upload};
use crate::tests::browser::assert_contains;
use crate::tests::{PgPool, app, db_test, user_id, user_with_team};

// --- what an upload may be ------------------------------------------------------------------

#[test]
fn avatars_a_valid_png_becomes_a_256_pixel_square_webp() {
	// Wider than tall: the longer side is cropped, then the square is resized.
	let done = process(Some("image/png"), &png(300, 120)).unwrap();
	assert_eq!(done.mime, "image/webp");
	assert_eq!(image::guess_format(&done.bytes).unwrap(), ImageFormat::WebP);
	let decoded = image::load_from_memory(&done.bytes).unwrap();
	assert_eq!((decoded.width(), decoded.height()), (SIZE, SIZE));
	assert_eq!(done.hash.len(), 16);
}

#[test]
fn avatars_jpeg_webp_and_gif_are_accepted_too() {
	for (mime, format) in [
		("image/jpeg", ImageFormat::Jpeg),
		("image/gif", ImageFormat::Gif),
		("image/webp", ImageFormat::WebP),
	] {
		let done = process(Some(mime), &picture(64, 64, format))
			.unwrap_or_else(|e| panic!("{mime}: {e:?}"));
		let decoded = image::load_from_memory(&done.bytes).unwrap();
		assert_eq!((decoded.width(), decoded.height()), (SIZE, SIZE), "{mime}");
	}
}

#[test]
fn avatars_an_oversized_upload_is_refused() {
	let mut big = png(8, 8);
	big.resize(super::MAX_UPLOAD + 1, 0);
	let err = process(Some("image/png"), &big).unwrap_err();
	assert!(matches!(err, ServerError::PayloadTooLarge(_)), "{err:?}");
}

#[test]
fn avatars_an_svg_is_refused_whatever_it_is_called() {
	let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
	for declared in ["image/svg+xml", "image/png", "image/jpeg", "text/xml"] {
		let err = process(Some(declared), svg).unwrap_err();
		assert!(
			matches!(err, ServerError::Validation(_)),
			"{declared}: {err:?}"
		);
	}
}

#[test]
fn avatars_a_declared_type_that_the_bytes_contradict_is_refused() {
	let jpeg = picture(16, 16, ImageFormat::Jpeg);
	let err = process(Some("image/png"), &jpeg).unwrap_err();
	assert!(matches!(err, ServerError::Validation(_)), "{err:?}");
	let err = process(Some("image/jpeg"), &png(16, 16)).unwrap_err();
	assert!(matches!(err, ServerError::Validation(_)), "{err:?}");
	// Text and nothing at all are not pictures either.
	assert!(process(Some("image/png"), b"just some text").is_err());
	assert!(process(Some("image/png"), b"").is_err());
	// No declared type: refused rather than guessed.
	assert!(process(None, &png(8, 8)).is_err());
}

#[test]
fn avatars_metadata_does_not_survive() {
	let mut with_text = png(32, 32);
	// A tEXt chunk, spliced in before IEND (the last 12 bytes of a PNG).
	let chunk_start = with_text.len() - 12;
	let mut chunk = Vec::new();
	let data = b"Comment\0secret-gps-position";
	chunk.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
	chunk.extend_from_slice(b"tEXt");
	chunk.extend_from_slice(data);
	let mut crc_input = b"tEXt".to_vec();
	crc_input.extend_from_slice(data);
	chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());
	with_text.splice(chunk_start..chunk_start, chunk);
	assert!(
		with_text.windows(10).any(|w| w == b"secret-gps"),
		"the test file carries the text"
	);

	let done = process(Some("image/png"), &with_text).unwrap();
	assert!(!done.bytes.windows(10).any(|w| w == b"secret-gps"));
}

fn crc32(data: &[u8]) -> u32 {
	let mut crc = 0xFFFF_FFFFu32;
	for &byte in data {
		crc ^= u32::from(byte);
		for _ in 0..8 {
			crc = if crc & 1 == 1 {
				(crc >> 1) ^ 0xEDB8_8320
			} else {
				crc >> 1
			};
		}
	}
	!crc
}

#[db_test]
async fn avatars_the_api_refuses_a_big_file_an_svg_and_a_fake_type(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let team_uri = format!("/api/v1/teams/{team}/avatar");

	let mut big = png(8, 8);
	big.resize(3 * 1024 * 1024, 0);
	assert_eq!(
		upload(&app, &team_uri, &token, "image/png", &big).await,
		StatusCode::PAYLOAD_TOO_LARGE
	);
	let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
	assert_eq!(
		upload(&app, &team_uri, &token, "image/svg+xml", svg).await,
		StatusCode::BAD_REQUEST
	);
	assert_eq!(
		upload(&app, "/api/v1/me/avatar", &token, "image/png", svg).await,
		StatusCode::BAD_REQUEST
	);
	assert_eq!(
		upload(&app, "/api/v1/me/avatar", &token, "image/jpeg", &png(8, 8)).await,
		StatusCode::BAD_REQUEST
	);
	assert_eq!(crate::tests::count(&db, "team_avatars").await, 0);
	assert_eq!(crate::tests::count(&db, "user_avatars").await, 0);
}

// --- storing and serving --------------------------------------------------------------------

#[db_test]
async fn avatars_a_png_is_stored_as_a_256_pixel_webp_and_served_with_a_long_cache(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let alice = user_id(&db, "alice").await;

	let status = upload(&app, "/api/v1/me/avatar", &token, "image/png", &png(40, 20)).await;
	assert_eq!(status, StatusCode::OK);
	let status = upload(
		&app,
		&format!("/api/v1/teams/{team}/avatar"),
		&token,
		"image/png",
		&png(20, 40),
	)
	.await;
	assert_eq!(status, StatusCode::OK);

	for uri in [
		format!("/avatars/user/{alice}?v=x"),
		format!("/avatars/team/{team}?v=x"),
	] {
		let (status, headers, body) = send(&app, "GET", &uri, Who::Token(&token), None).await;
		assert_eq!(status, StatusCode::OK, "{uri}");
		assert_eq!(headers[header::CONTENT_TYPE], "image/webp");
		let cache = headers[header::CACHE_CONTROL].to_str().unwrap();
		assert!(
			cache.contains("max-age=31536000") && cache.contains("immutable"),
			"{cache}"
		);
		assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
		let decoded = image::load_from_memory(&body).unwrap();
		assert_eq!((decoded.width(), decoded.height()), (256, 256), "{uri}");
	}

	// The API says where the picture is; replacing it changes the URL.
	let uri = format!("/api/v1/teams/{team}/avatar");
	let (_, _, first) = send(
		&app,
		"PUT",
		&uri,
		Who::Token(&token),
		Some(multipart("image/png", &png(30, 30))),
	)
	.await;
	let (_, _, second) = send(
		&app,
		"PUT",
		&uri,
		Who::Token(&token),
		Some(multipart("image/jpeg", &picture(31, 31, ImageFormat::Jpeg))),
	)
	.await;
	let (first, second) = (
		String::from_utf8(first).unwrap(),
		String::from_utf8(second).unwrap(),
	);
	assert_ne!(first, second);
	assert_contains(&second, &format!("/avatars/team/{team}?v="));
}

#[db_test]
async fn avatars_removing_a_picture_makes_its_address_a_404(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let uri = format!("/api/v1/teams/{team}/avatar");
	assert_eq!(
		upload(&app, &uri, &token, "image/png", &png(8, 8)).await,
		StatusCode::OK
	);
	let get = format!("/avatars/team/{team}");
	assert_eq!(
		send(&app, "GET", &get, Who::Token(&token), None).await.0,
		StatusCode::OK
	);

	let (status, _, _) = send(&app, "DELETE", &uri, Who::Token(&token), None).await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(
		send(&app, "GET", &get, Who::Token(&token), None).await.0,
		StatusCode::NOT_FOUND
	);
}

// --- who sees them --------------------------------------------------------------------------

#[db_test]
async fn avatars_another_teams_pictures_are_a_404_and_a_fellow_members_are_not(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice_token, alice_team) = user_with_team(&app, "alice").await;
	let (bob_token, bob_team) = user_with_team(&app, "bob").await;
	let (carol_token, _) = user_with_team(&app, "carol").await;
	let alice = user_id(&db, "alice").await;
	let bob = user_id(&db, "bob").await;
	for (token, uri) in [
		(&alice_token, "/api/v1/me/avatar".to_owned()),
		(&alice_token, format!("/api/v1/teams/{alice_team}/avatar")),
		(&bob_token, "/api/v1/me/avatar".to_owned()),
	] {
		assert_eq!(
			upload(&app, &uri, token, "image/png", &png(8, 8)).await,
			StatusCode::OK
		);
	}
	let get = |token: String, uri: String| {
		let app = app.clone();
		async move { send(&app, "GET", &uri, Who::Token(&token), None).await.0 }
	};

	// Bob is in his own team only: Alice's team and Alice herself are not there for him.
	assert_eq!(
		get(bob_token.clone(), format!("/avatars/team/{alice_team}")).await,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		get(bob_token.clone(), format!("/avatars/user/{alice}")).await,
		StatusCode::NOT_FOUND
	);
	// A team that does not exist answers the same way as one that is not his.
	assert_eq!(
		get(bob_token.clone(), "/avatars/team/1".to_owned()).await,
		StatusCode::NOT_FOUND
	);
	// Someone with no picture and nobody signed in.
	assert_eq!(
		get(carol_token.clone(), format!("/avatars/user/{bob}")).await,
		StatusCode::NOT_FOUND
	);
	let (status, _, _) = send(
		&app,
		"GET",
		&format!("/avatars/team/{alice_team}"),
		Who::Token("nope"),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);

	// Everyone sees their own.
	assert_eq!(
		get(bob_token.clone(), format!("/avatars/user/{bob}")).await,
		StatusCode::OK
	);
	assert_eq!(
		get(bob_token.clone(), format!("/avatars/team/{bob_team}")).await,
		StatusCode::NOT_FOUND
	);

	// Once Bob joins Alice's team, her team's picture and hers are visible to him.
	add_member(&db, alice_team, "bob").await;
	assert_eq!(
		get(bob_token.clone(), format!("/avatars/team/{alice_team}")).await,
		StatusCode::OK
	);
	assert_eq!(
		get(bob_token, format!("/avatars/user/{alice}")).await,
		StatusCode::OK
	);
}

// --- who changes them -----------------------------------------------------------------------

#[db_test]
async fn avatars_only_the_owner_changes_a_persons_picture(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice_token, _) = user_with_team(&app, "alice").await;
	let (bob_token, _) = user_with_team(&app, "bob").await;
	let alice = UserId(user_id(&db, "alice").await);
	let bob = UserId(user_id(&db, "bob").await);

	// The service refuses anyone acting on another person.
	let err = service::set_user(&db, bob, alice, Some("image/png"), &png(8, 8))
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	let err = service::clear_user(&db, bob, alice).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	assert_eq!(crate::tests::count(&db, "user_avatars").await, 0);

	// And the route only ever acts on the caller: Bob's upload lands on Bob.
	assert_eq!(
		upload(
			&app,
			"/api/v1/me/avatar",
			&bob_token,
			"image/png",
			&png(8, 8)
		)
		.await,
		StatusCode::OK
	);
	let owners: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM user_avatars")
		.fetch_all(&db)
		.await
		.unwrap();
	assert_eq!(owners, vec![bob.0]);
	// Alice removing hers when she has none, or Bob's still being there afterwards.
	let (status, _, _) = send(
		&app,
		"DELETE",
		"/api/v1/me/avatar",
		Who::Token(&alice_token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(crate::tests::count(&db, "user_avatars").await, 1);
}

#[db_test]
async fn avatars_a_team_picture_needs_update_team(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let (bob_token, _) = user_with_team(&app, "bob").await;
	let (dan_token, _) = user_with_team(&app, "dan").await;
	add_member(&db, team, "dan").await;
	let uri = format!("/api/v1/teams/{team}/avatar");

	// A stranger is told the team is not there; a read-only member is refused. Nothing is stored.
	for (token, refused) in [
		(&bob_token, StatusCode::NOT_FOUND),
		(&dan_token, StatusCode::FORBIDDEN),
	] {
		assert_eq!(
			upload(&app, &uri, token, "image/png", &png(8, 8)).await,
			refused
		);
		let (status, _, _) = send(&app, "DELETE", &uri, Who::Token(token), None).await;
		assert_eq!(status, refused);
	}
	assert_eq!(crate::tests::count(&db, "team_avatars").await, 0);
	let err = service::set_team(
		&db,
		UserId(user_id(&db, "bob").await),
		TeamId(team),
		Some("image/png"),
		&png(8, 8),
	)
	.await
	.unwrap_err();
	assert!(matches!(err, ServerError::NotFound), "{err:?}");
}
