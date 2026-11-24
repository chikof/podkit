//! The fixtures the pictures tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `avatars::tests`.

use std::io::Cursor;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tower::ServiceExt;

use crate::tests::browser::HOST;
use crate::tests::{PgPool, user_id};

pub const BOUNDARY: &str = "----podkit-test-boundary";

/// A solid picture of `w` x `h` pixels in `format`.
pub fn picture(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
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

pub fn png(w: u32, h: u32) -> Vec<u8> {
	picture(w, h, ImageFormat::Png)
}

/// A `multipart/form-data` body with one `file` part.
pub fn multipart(content_type: &str, bytes: &[u8]) -> Vec<u8> {
	let mut body = format!(
		"--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"me\"\r\nContent-Type: {content_type}\r\n\r\n"
	)
	.into_bytes();
	body.extend_from_slice(bytes);
	body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
	body
}

pub enum Who<'a> {
	Token(&'a str),
	Cookie(&'a str),
}

pub async fn send(
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

pub async fn upload(app: &Router, uri: &str, token: &str, mime: &str, bytes: &[u8]) -> StatusCode {
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
pub async fn add_member(db: &PgPool, team: i64, name: &str) {
	let user = user_id(db, name).await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(team)
		.bind(user)
		.execute(db)
		.await
		.unwrap();
}
