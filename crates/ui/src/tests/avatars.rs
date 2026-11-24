//! Pictures: what an upload may be, what is stored, who may see it and who may change it.
//!
//! The tests of `podkit_core::avatars` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::avatars::tests`.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use tower::ServiceExt;

use crate::testing::{HOST, assert_contains, assert_not_contains, get_html, page_main, ui_session};
use crate::tests::{PgPool, app, db_test, user_id, user_with_team};
use podkit_core::tests::avatars::{BOUNDARY, Who, add_member, multipart, png, send, upload};

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

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
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

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
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

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
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

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
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
