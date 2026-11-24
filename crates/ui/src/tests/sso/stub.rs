//! A stand-in `OpenID` Connect provider: a real axum server on a loopback port, so podkit's
//! client runs its real HTTP against it.
//!
//! It speaks what the specs ask of a provider and checks what they ask of a client:
//! discovery (<https://openid.net/specs/openid-connect-discovery-1_0.html> §4), the token
//! endpoint with `client_secret_basic` (RFC 6749 §2.3.1) and the PKCE verifier against the
//! S256 challenge of the authorization request (RFC 7636 §4.6), a JWKS and userinfo
//! (<https://openid.net/specs/openid-connect-core-1_0.html> §5.3). What it puts in the ID
//! token is set per test through [`Behaviour`], to produce the bad answers a client must
//! refuse.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The provider's published signing key.
const GOOD_KEY: &str = include_str!("../../../../core/src/ssh_keys/fixtures/rsa_pkcs1");
/// Another key the provider does not publish.
const OTHER_KEY: &str = include_str!("../../../../core/src/ssh_keys/fixtures/rsa_pkcs8");

const SHA256_PREFIX: [u8; 19] = [
	0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
	0x00, 0x04, 0x20,
];

pub const CLIENT_ID: &str = "podkit-client";
pub const CLIENT_SECRET: &str = "stub-client-secret-do-not-render";
pub const KID: &str = "stub-key-1";

/// Which key signs the ID token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Signer {
	/// The published key.
	Published,
	/// A key nobody published: a forged token.
	Unpublished,
	/// HS256 with the client secret as the key.
	Shared,
}

/// What the next ID token says and how it is signed.
#[derive(Clone)]
pub struct Behaviour {
	pub signer: Signer,
	pub subject: String,
	pub email: Option<String>,
	pub email_verified: Option<bool>,
	pub name: String,
	pub groups: Option<Value>,
	/// Instead of the issuer.
	pub iss: Option<String>,
	/// Instead of the client id.
	pub aud: Option<Value>,
	pub azp: Option<String>,
	/// `Some(None)` leaves the nonce out; `Some(Some(x))` says `x`.
	#[allow(clippy::option_option)]
	pub nonce: Option<Option<String>>,
	/// Seconds from now; negative is in the past.
	pub exp_in: i64,
	/// Leave the email out of the token and give it at userinfo.
	pub email_at_userinfo: bool,
}

impl Default for Behaviour {
	fn default() -> Self {
		Self {
			signer: Signer::Published,
			subject: "sub-1".to_owned(),
			email: Some("ada@corp.test".to_owned()),
			email_verified: Some(true),
			name: "Ada Lovelace".to_owned(),
			groups: None,
			iss: None,
			aud: None,
			azp: None,
			nonce: None,
			exp_in: 300,
			email_at_userinfo: false,
		}
	}
}

struct Grant {
	challenge: String,
	nonce: String,
	redirect_uri: String,
}

pub struct Stub {
	/// `http://127.0.0.1:<port>`: also the issuer.
	pub url: String,
	behaviour: Mutex<Behaviour>,
	grants: Mutex<HashMap<String, Grant>>,
	/// Tokens handed out, by access token, for userinfo.
	issued: Mutex<HashMap<String, Behaviour>>,
	pub token_requests: AtomicUsize,
	pub pkce_refused: AtomicUsize,
	pub client_refused: AtomicUsize,
	counter: AtomicUsize,
}

impl Stub {
	/// Starts the stub on a free loopback port.
	pub async fn start() -> Arc<Self> {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("http://{}", listener.local_addr().unwrap());
		let stub = Arc::new(Self {
			url,
			behaviour: Mutex::new(Behaviour::default()),
			grants: Mutex::new(HashMap::new()),
			issued: Mutex::new(HashMap::new()),
			token_requests: AtomicUsize::new(0),
			pkce_refused: AtomicUsize::new(0),
			client_refused: AtomicUsize::new(0),
			counter: AtomicUsize::new(0),
		});
		let app = Router::new()
			.route("/.well-known/openid-configuration", get(discovery))
			.route("/jwks", get(jwks))
			.route("/token", post(token))
			.route("/userinfo", get(userinfo))
			.with_state(stub.clone());
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		stub
	}

	/// Changes what the next ID tokens say.
	pub fn set(&self, f: impl FnOnce(&mut Behaviour)) {
		f(&mut self.behaviour.lock().unwrap());
	}

	/// The person approves at the provider: takes podkit's authorization redirect and
	/// returns the path and query the provider sends the browser back to.
	pub fn approve(&self, location: &str) -> String {
		let url = url::Url::parse(location).expect("the authorization address");
		assert_eq!(
			url.path(),
			"/authorize",
			"podkit sends the browser to the authorization endpoint"
		);
		let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
		assert_eq!(q["response_type"], "code");
		assert_eq!(q["client_id"], CLIENT_ID);
		assert_eq!(q["code_challenge_method"], "S256", "PKCE with S256");
		assert!(q["scope"].split(' ').any(|s| s == "openid"));
		let code = format!("code-{}", self.counter.fetch_add(1, Ordering::SeqCst));
		self.grants.lock().unwrap().insert(
			code.clone(),
			Grant {
				challenge: q["code_challenge"].clone(),
				nonce: q.get("nonce").cloned().unwrap_or_default(),
				redirect_uri: q["redirect_uri"].clone(),
			},
		);
		let back = url::Url::parse(&q["redirect_uri"]).expect("the redirect address");
		format!(
			"{}?code={}&state={}",
			back.path(),
			code,
			url::form_urlencoded::byte_serialize(q["state"].as_bytes()).collect::<String>()
		)
	}
}

type Shared = State<Arc<Stub>>;

async fn discovery(State(s): Shared) -> Json<Value> {
	Json(json!({
		"issuer": s.url,
		"authorization_endpoint": format!("{}/authorize", s.url),
		"token_endpoint": format!("{}/token", s.url),
		"jwks_uri": format!("{}/jwks", s.url),
		"userinfo_endpoint": format!("{}/userinfo", s.url),
		"token_endpoint_auth_methods_supported": ["client_secret_basic"],
		"id_token_signing_alg_values_supported": ["RS256"],
	}))
}

async fn jwks() -> Json<Value> {
	let key = rsa::RsaPrivateKey::from_pkcs1_pem(GOOD_KEY).unwrap();
	let public = key.to_public_key();
	Json(json!({ "keys": [{
		"kty": "RSA",
		"use": "sig",
		"alg": "RS256",
		"kid": KID,
		"n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
		"e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
	}]}))
}

fn now() -> i64 {
	time::OffsetDateTime::now_utc().unix_timestamp()
}

fn claims(s: &Stub, b: &Behaviour, nonce: &str) -> Value {
	let mut map = Map::new();
	map.insert(
		"iss".into(),
		json!(b.iss.clone().unwrap_or_else(|| s.url.clone())),
	);
	map.insert("sub".into(), json!(b.subject));
	map.insert(
		"aud".into(),
		b.aud.clone().unwrap_or_else(|| json!(CLIENT_ID)),
	);
	if let Some(azp) = &b.azp {
		map.insert("azp".into(), json!(azp));
	}
	map.insert("iat".into(), json!(now()));
	map.insert("exp".into(), json!(now() + b.exp_in));
	match &b.nonce {
		None => {
			map.insert("nonce".into(), json!(nonce));
		}
		Some(Some(other)) => {
			map.insert("nonce".into(), json!(other));
		}
		Some(None) => {}
	}
	map.insert("name".into(), json!(b.name));
	if !b.email_at_userinfo {
		if let Some(email) = &b.email {
			map.insert("email".into(), json!(email));
		}
		if let Some(v) = b.email_verified {
			map.insert("email_verified".into(), json!(v));
		}
	}
	if let Some(groups) = &b.groups {
		map.insert("groups".into(), groups.clone());
	}
	Value::Object(map)
}

fn sign(b: &Behaviour, claims: &Value) -> String {
	if b.signer == Signer::Shared {
		let mut header = Header::new(Algorithm::HS256);
		header.kid = Some(KID.to_owned());
		let key = EncodingKey::from_secret(CLIENT_SECRET.as_bytes());
		return jsonwebtoken::encode(&header, claims, &key).unwrap();
	}
	// RS256 by hand: the signature of `header.claims` under the key (RFC 7518 §3.3).
	let pem = if b.signer == Signer::Published {
		GOOD_KEY
	} else {
		OTHER_KEY
	};
	let key = rsa::RsaPrivateKey::from_pkcs1_pem(pem)
		.or_else(|_| rsa::RsaPrivateKey::from_pkcs8_pem(pem))
		.unwrap();
	let header =
		URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "typ": "JWT", "kid": KID }).to_string());
	let body = URL_SAFE_NO_PAD.encode(claims.to_string());
	let input = format!("{header}.{body}");
	// `rsa` and `sha2` use different `digest` crates here, so the `DigestInfo` header of
	// SHA-256 (RFC 8017 §9.2) is put on by hand and the unprefixed scheme signs it.
	let mut info = SHA256_PREFIX.to_vec();
	info.extend_from_slice(&Sha256::digest(input.as_bytes()));
	let signature = key
		.sign(rsa::Pkcs1v15Sign::new_unprefixed(), &info)
		.unwrap();
	format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn oauth_error(error: &str) -> Response {
	(StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

async fn token(State(s): Shared, headers: HeaderMap, body: String) -> Response {
	s.token_requests.fetch_add(1, Ordering::SeqCst);
	// client_secret_basic: `base64(id:secret)`.
	let expected = format!(
		"Basic {}",
		STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
	);
	let given = headers
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.unwrap_or_default();
	if given != expected {
		s.client_refused.fetch_add(1, Ordering::SeqCst);
		return (
			StatusCode::UNAUTHORIZED,
			Json(json!({ "error": "invalid_client" })),
		)
			.into_response();
	}
	let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
		.into_owned()
		.collect();
	if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
		return oauth_error("unsupported_grant_type");
	}
	// A code works once.
	let Some(grant) = form
		.get("code")
		.and_then(|c| s.grants.lock().unwrap().remove(c))
	else {
		return oauth_error("invalid_grant");
	};
	if form.get("redirect_uri") != Some(&grant.redirect_uri) {
		return oauth_error("invalid_grant");
	}
	let verifier = form.get("code_verifier").cloned().unwrap_or_default();
	let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
	if verifier.len() < 43 || challenge != grant.challenge {
		s.pkce_refused.fetch_add(1, Ordering::SeqCst);
		return oauth_error("invalid_grant");
	}
	let behaviour = s.behaviour.lock().unwrap().clone();
	let id_token = sign(&behaviour, &claims(&s, &behaviour, &grant.nonce));
	let access = format!("access-{}", s.counter.fetch_add(1, Ordering::SeqCst));
	s.issued.lock().unwrap().insert(access.clone(), behaviour);
	Json(json!({
		"access_token": access,
		"token_type": "Bearer",
		"expires_in": 300,
		"id_token": id_token,
	}))
	.into_response()
}

async fn userinfo(State(s): Shared, headers: HeaderMap) -> Response {
	let access = headers
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "))
		.unwrap_or_default();
	let Some(b) = s.issued.lock().unwrap().get(access).cloned() else {
		return StatusCode::UNAUTHORIZED.into_response();
	};
	Json(json!({
		"sub": b.subject,
		"email": b.email,
		"email_verified": b.email_verified,
		"name": b.name,
	}))
	.into_response()
}
