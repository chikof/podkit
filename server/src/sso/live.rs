//! The real thing: an `OpenID` Connect sign-in round trip against a throwaway Dex container
//! (`pk-t_712cc5ef-dex`), the in-process app being the relying party.
//!
//! NOT RUN in CI. Needs a rootless Podman, the network (pulls the Dex image) and a free port:
//! `cargo test -p server --all-features -- --ignored oidc_live`
//!
//! Dex configuration (static client, static password, in-memory storage, no approval screen):
//! <https://dexidp.io/docs/configuration/> and the sample `examples/config-dev.yaml` in
//! <https://github.com/dexidp/dex>, which is also where the bcrypt hash (password
//! `password`) for `dex-user@example.com` comes from. The login form posts `login` and
//! `password` to the page's own address (`/dex/auth/local/login`).

use std::process::Command;

use axum::http::header;
use url::Url;

use super::providers::{self, Input, Kind, Scope};
use crate::ids::UserId;
use crate::state::{Config, Registration};
use crate::tests::{PgPool, db_test, state};
use crate::ui::testing::{Browser, ui_session};

const NAME: &str = "pk-t_712cc5ef-dex";
const PORT: u16 = 23471;
const IMAGE: &str = "docker.io/dexidp/dex:v2.41.1";
const PUBLIC: &str = "http://podkit.test";
const HASH: &str = "$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W";

fn podman(args: &[&str]) -> Result<String, String> {
	let path = format!(
		"{}:/run/current-system/sw/bin",
		std::env::var("PATH").unwrap_or_default()
	);
	let out = Command::new("podman")
		.args(args)
		.env("PATH", path)
		.output()
		.map_err(|e| format!("podman {args:?}: {e}"))?;
	if out.status.success() {
		Ok(String::from_utf8_lossy(&out.stdout).into_owned())
	} else {
		Err(format!(
			"podman {args:?}: {}",
			String::from_utf8_lossy(&out.stderr)
		))
	}
}

/// Removes what the test made, by name, whatever happened.
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
	fn drop(&mut self) {
		let _ = podman(&["rm", "-f", NAME]);
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

fn config(redirect: &str) -> String {
	format!(
		"issuer: http://127.0.0.1:{PORT}/dex\n\
		 storage:\n  type: memory\n\
		 web:\n  http: 0.0.0.0:5556\n\
		 oauth2:\n  skipApprovalScreen: true\n\
		 enablePasswordDB: true\n\
		 staticClients:\n\
		 \x20 - id: podkit\n    secret: podkit-live-secret\n    name: podkit\n    redirectURIs:\n      - {redirect}\n\
		 staticPasswords:\n\
		 \x20 - email: dex-user@example.com\n    hash: \"{HASH}\"\n    username: admin\n    userID: 08a8684b-db88-4b73-90a9-3cd1661f5466\n"
	)
}

/// Plays the browser at Dex: follows redirects, fills the login form, and stops at the
/// first address inside `PUBLIC`, which it returns as a path and query.
async fn through_dex(start: &str) -> String {
	let http = reqwest::Client::builder()
		.redirect(reqwest::redirect::Policy::none())
		.build()
		.unwrap();
	let mut url = Url::parse(start).unwrap();
	for _ in 0..12 {
		if url.as_str().starts_with(PUBLIC) {
			return format!("{}?{}", url.path(), url.query().unwrap_or_default());
		}
		let reply = http.get(url.clone()).send().await.unwrap();
		if reply.status().is_redirection() {
			let to = reply.headers()[header::LOCATION].to_str().unwrap();
			url = url.join(to).unwrap();
			continue;
		}
		let html = reply.text().await.unwrap();
		let action = html
			.split("action=\"")
			.nth(1)
			.and_then(|s| s.split('"').next())
			.unwrap_or_else(|| panic!("no login form in: {html}"))
			.replace("&amp;", "&");
		let post = http
			.post(url.join(&action).unwrap())
			.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
			.body("login=dex-user%40example.com&password=password")
			.send()
			.await
			.unwrap();
		assert!(post.status().is_redirection(), "login: {}", post.status());
		let to = post.headers()[header::LOCATION].to_str().unwrap();
		url = url.join(to).unwrap();
	}
	panic!("Dex never sent the browser back to podkit");
}

#[db_test]
#[ignore = "needs Podman and the network (a Dex container)"]
async fn oidc_live_dex_round_trip(db: PgPool) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let dir = std::env::temp_dir().join("pk-t_712cc5ef-dex");
	let _clean = Cleanup(dir.clone());
	let _ = podman(&["rm", "-f", NAME]);
	std::fs::create_dir_all(&dir).unwrap();

	let issuer = format!("http://127.0.0.1:{PORT}/dex");
	let mut st = state(db.clone());
	st.config = std::sync::Arc::new(Config {
		public_url: Some(PUBLIC.to_owned()),
		registration: Registration::Open,
		..(*st.config).clone()
	});
	let app = crate::routes::routes(st.clone()).await.unwrap();
	let _ = ui_session(&app, "admin").await;
	let admin = UserId(crate::tests::user_id(&db, "admin").await);
	let p = providers::create(
		&st,
		admin,
		Scope::Instance,
		&Input {
			kind: Kind::Oidc,
			name: "Dex".into(),
			slug: "dex".into(),
			issuer: issuer.clone(),
			client_id: "podkit".into(),
			client_secret: "podkit-live-secret".into(),
			scopes: "openid email profile".into(),
			jit: true,
			allow_unverified_email: false,
			group_claim: String::new(),
			group_roles: String::new(),
			enabled: true,
		},
	)
	.await
	.unwrap();

	// Dex needs the exact redirect address, so it starts once the provider's id is known.
	let redirect = format!("{PUBLIC}/sso/{}/callback", p.id);
	std::fs::write(dir.join("config.yaml"), config(&redirect)).unwrap();
	podman(&[
		"run",
		"-d",
		"--name",
		NAME,
		"-p",
		&format!("127.0.0.1:{PORT}:5556"),
		"-v",
		&format!(
			"{}:/etc/dex/config.yaml:ro,Z",
			dir.join("config.yaml").display()
		),
		IMAGE,
		"dex",
		"serve",
		"/etc/dex/config.yaml",
	])
	.unwrap();

	let http = reqwest::Client::new();
	let mut up = false;
	for _ in 0..60 {
		if let Ok(r) = http
			.get(format!("{issuer}/.well-known/openid-configuration"))
			.send()
			.await && r.status().is_success()
		{
			up = true;
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
	}
	assert!(up, "Dex did not come up: {:?}", podman(&["logs", NAME]));

	let mut b = Browser::default();
	let (status, headers, body) = b.get(&app, &format!("/sso/{}/start", p.id)).await;
	assert!(status.is_redirection(), "{status}\n{body}");
	let back = through_dex(headers[header::LOCATION].to_str().unwrap()).await;
	let (status, answer, body) = b.get(&app, &back).await;
	assert!(status.is_success(), "{status}\n{body}");
	assert!(
		b.has_session(),
		"signed in through Dex: {answer:?}\n{}",
		body.split("<main").nth(1).unwrap_or(&body)
	);
	let who: String = sqlx::query_scalar("SELECT email FROM users WHERE email = $1")
		.bind("dex-user@example.com")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(who, "dex-user@example.com");
}
