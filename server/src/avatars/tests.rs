//! Pictures: what an upload may be, what is stored, who may see it and who may change it.

use std::io::Cursor;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tower::ServiceExt;

use super::process::{SIZE, process};
use super::service;
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::tests::{PgPool, app, db_test, user_id, user_with_team};
use crate::ui::testing::{
	HOST, assert_contains, assert_not_contains, get_html, page_main, ui_session,
};

const BOUNDARY: &str = "----podkit-test-boundary";

/// A solid picture of `w` x `h` pixels in `format`.
fn picture(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
	let mut img = RgbaImage::from_pixel(w, h, Rgba([200, 30, 90, 255]));
	// A different corner, so a crop and a resize have something to move.
	img.put_pixel(0, 0, Rgba([0, 0, 255, 255]));
	let img = match format {
		// JPEG has no alpha channel.
		ImageFormat::Jpeg => DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(img).to_rgb8()),
		_ => DynamicImage::ImageRgba8(img),
	};
	let mut out = Vec::new();
	img.write_to(&mut Cursor::new(&mut out), format).unwrap();
	out
}

fn png(w: u32, h: u32) -> Vec<u8> {
	picture(w, h, ImageFormat::Png)
}

/// A `multipart/form-data` body with one `file` part.
fn multipart(content_type: &str, bytes: &[u8]) -> Vec<u8> {
	let mut body = format!(
		"--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"me\"\r\nContent-Type: {content_type}\r\n\r\n"
	)
	.into_bytes();
	body.extend_from_slice(bytes);
	body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
	body
}

enum Who<'a> {
	Token(&'a str),
	Cookie(&'a str),
}

async fn send(
	app: &Router,
	method: &str,
	uri: &str,
	who: Who<'_>,
	body: Option<Vec<u8>>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
	let mut req = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::HOST, HOST);
	req = match who {
		Who::Token(t) => req.header(header::AUTHORIZATION, format!("Bearer {t}")),
		Who::Cookie(c) => req
			.header(header::COOKIE, c)
			.header(header::ORIGIN, format!("http://{HOST}")),
	};
	let body = match body {
		Some(b) => {
			req = req.header(
				header::CONTENT_TYPE,
				format!("multipart/form-data; boundary={BOUNDARY}"),
			);
			Body::from(b)
		}
		None => Body::empty(),
	};
	let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
	let (parts, body) = resp.into_parts();
	let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
	(parts.status, parts.headers, bytes.to_vec())
}

async fn upload(app: &Router, uri: &str, token: &str, mime: &str, bytes: &[u8]) -> StatusCode {
	send(
		app,
		"PUT",
		uri,
		Who::Token(token),
		Some(multipart(mime, bytes)),
	)
	.await
	.0
}

/// Puts `name` into `team` as a plain Member, who can read but not update.
async fn add_member(db: &PgPool, team: i64, name: &str) {
	let user = user_id(db, name).await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(team)
		.bind(user)
		.execute(db)
		.await
		.unwrap();
}

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

	// A stranger and a read-only member are both refused, and nothing is stored.
	for token in [&bob_token, &dan_token] {
		assert_eq!(
			upload(&app, &uri, token, "image/png", &png(8, 8)).await,
			StatusCode::FORBIDDEN
		);
		let (status, _, _) = send(&app, "DELETE", &uri, Who::Token(token), None).await;
		assert_eq!(status, StatusCode::FORBIDDEN);
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
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
}

// --- the pages ------------------------------------------------------------------------------

async fn post_form(
	app: &Router,
	uri: &str,
	cookie: &str,
	mime: &str,
	bytes: &[u8],
) -> (StatusCode, HeaderMap) {
	let (status, headers, _) = send(
		app,
		"POST",
		uri,
		Who::Cookie(cookie),
		Some(multipart(mime, bytes)),
	)
	.await;
	(status, headers)
}

#[db_test]
async fn avatars_the_pages_show_the_monogram_until_there_is_a_picture(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let alice = user_id(&db, "alice").await;
	let settings = format!("/teams/{team}/settings");
	let account = format!("/teams/{team}/account");

	for uri in [&settings, &account] {
		let (status, _, page) = get_html(&app, uri, Some(&cookie)).await;
		assert_eq!(status, StatusCode::OK, "{uri}");
		assert_not_contains(&page, "/avatars/");
		assert_contains(&page, "ui-team-mark");
		assert_contains(&page, "ui-avatar");
		// The one-letter team mark and the two-letter name: the fallback text is there.
		assert_contains(&page, ">A</span>");
	}
	let (_, _, page) = get_html(&app, &account, Some(&cookie)).await;
	assert_contains(page_main(&page), "alice@example.com");
	assert_eq!(page.matches("<h1").count(), 1, "one h1");

	// Upload both through the forms.
	let (status, headers) = post_form(
		&app,
		&format!("{settings}/avatar"),
		&cookie,
		"image/png",
		&png(50, 50),
	)
	.await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	assert_eq!(headers[header::LOCATION], settings.as_str());
	let (status, _) = post_form(
		&app,
		&format!("{account}/avatar"),
		&cookie,
		"image/png",
		&png(50, 50),
	)
	.await;
	assert_eq!(status, StatusCode::SEE_OTHER);

	// Now the sidebar, the switcher and the pages draw the picture over the monogram.
	let (_, _, page) = get_html(&app, &settings, Some(&cookie)).await;
	assert_contains(&page, &format!("/avatars/team/{team}?v="));
	assert_contains(&page, &format!("/avatars/user/{alice}?v="));
	assert_contains(&page, "ui-avatar-pic");
	// The image is decoration: its name is the monogram's, and the picture has no alt text.
	assert_contains(&page, "alt=\"\"");

	// Removing brings the monogram back.
	let (status, _) = post_form(
		&app,
		&format!("{settings}/avatar/remove"),
		&cookie,
		"image/png",
		b"",
	)
	.await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	let (_, _, page) = get_html(&app, &settings, Some(&cookie)).await;
	assert_not_contains(&page, &format!("/avatars/team/{team}"));
	assert_contains(&page, &format!("/avatars/user/{alice}?v="));
}

#[db_test]
async fn avatars_a_forged_page_post_changes_nothing(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = ui_session(&app, "alice").await;
	let (dan_cookie, _) = ui_session(&app, "dan").await;
	add_member(&db, team, "dan").await;
	let uri = format!("/teams/{team}/settings/avatar");

	// A read-only member's post is turned back to the page and stores nothing.
	let (status, _) = post_form(&app, &uri, &dan_cookie, "image/png", &png(8, 8)).await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	assert_eq!(crate::tests::count(&db, "team_avatars").await, 0);

	// A stranger's post is not served: as for any action on a team they are not in, they get
	// the "no longer exists" answer, never a redirect back to the page.
	let (bob_cookie, _) = ui_session(&app, "bob").await;
	let (status, _) = post_form(&app, &uri, &bob_cookie, "image/png", &png(8, 8)).await;
	assert_ne!(status, StatusCode::SEE_OTHER);
	assert_eq!(crate::tests::count(&db, "team_avatars").await, 0);

	// A post from another site is refused before it is read.
	let req = Request::builder()
		.method("POST")
		.uri(&uri)
		.header(header::HOST, HOST)
		.header(header::ORIGIN, "http://evil.example")
		.header(header::COOKIE, &dan_cookie)
		.header(
			header::CONTENT_TYPE,
			format!("multipart/form-data; boundary={BOUNDARY}"),
		)
		.body(Body::from(multipart("image/png", &png(8, 8))))
		.unwrap();
	let resp = app.clone().oneshot(req).await.unwrap();
	assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[db_test]
async fn avatars_a_bad_file_on_the_page_is_a_toast_not_a_stored_picture(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let uri = format!("/teams/{team}/settings/avatar");
	let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;

	let (status, headers) = post_form(&app, &uri, &cookie, "image/svg+xml", svg).await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	let flash = headers
		.get_all(header::SET_COOKIE)
		.iter()
		.filter_map(|v| v.to_str().ok())
		.find(|c| c.starts_with("flash="))
		.expect("a flash cookie");
	assert_contains(flash, "t=danger");
	assert_eq!(crate::tests::count(&db, "team_avatars").await, 0);
}

#[db_test]
async fn avatars_the_members_list_uses_each_persons_picture(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (bob_token, _) = user_with_team(&app, "bob").await;
	add_member(&db, team, "bob").await;
	let bob = user_id(&db, "bob").await;
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

	let (status, _, page) = get_html(&app, &format!("/teams/{team}/members"), Some(&cookie)).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, &format!("/avatars/user/{bob}?v="));
	// Alice has none: her row keeps the monogram.
	assert_eq!(main.matches("/avatars/user/").count(), 1, "{main}");
}
