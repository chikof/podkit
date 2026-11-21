//! The OpenID Connect side of a sign-in: discovery, the code exchange and the validation
//! of the ID token.
//!
//! Specs followed: OIDC Core 1.0 (<https://openid.net/specs/openid-connect-core-1_0.html>)
//! §3.1.2 (authorization request), §3.1.3 (token request), §3.1.3.7 (ID token validation);
//! OIDC Discovery 1.0 (<https://openid.net/specs/openid-connect-discovery-1_0.html>) §4;
//! PKCE, RFC 7636 (<https://www.rfc-editor.org/rfc/rfc7636>). The token endpoint client
//! authentication is RFC 6749 §2.3.1 (<https://www.rfc-editor.org/rfc/rfc6749#section-2.3.1>).

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// How long to wait for an identity provider.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The signature algorithms an ID token may use. Never `none`, never a shared-secret
/// (`HS*`) one: the key must be the provider's public key, or anyone who knows the client
/// secret could mint tokens.
const ALLOWED: [Algorithm; 9] = [
	Algorithm::RS256,
	Algorithm::RS384,
	Algorithm::RS512,
	Algorithm::PS256,
	Algorithm::PS384,
	Algorithm::PS512,
	Algorithm::ES256,
	Algorithm::ES384,
	Algorithm::EdDSA,
];

/// Seconds of clock difference tolerated for `exp` and `iat`.
const LEEWAY: u64 = 60;

/// Why a step failed, for the log. The person is told something shorter.
pub type Failure = String;

/// The part of a provider's discovery document podkit uses.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
	pub issuer: String,
	pub authorization_endpoint: String,
	pub token_endpoint: String,
	pub jwks_uri: String,
	#[serde(default)]
	pub userinfo_endpoint: Option<String>,
	#[serde(default)]
	pub token_endpoint_auth_methods_supported: Vec<String>,
}

fn same_issuer(a: &str, b: &str) -> bool {
	a.trim_end_matches('/') == b.trim_end_matches('/')
}

async fn get_json(http: &reqwest::Client, url: &str) -> Result<Value, Failure> {
	let response = http
		.get(url)
		.header("accept", "application/json")
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| format!("cannot reach {url}: {e}"))?;
	if !response.status().is_success() {
		return Err(format!("{url} answered {}", response.status()));
	}
	read_json(response, &url).await
}

/// Reads a JSON body, up to a megabyte: an identity provider's answers are small.
pub async fn read_json<T: serde::de::DeserializeOwned>(
	response: reqwest::Response,
	what: &str,
) -> Result<T, Failure> {
	let bytes = response
		.bytes()
		.await
		.map_err(|e| format!("cannot read the answer of {what}: {e}"))?;
	if bytes.len() > 1 << 20 {
		return Err(format!("the answer of {what} is too large"));
	}
	serde_json::from_slice(&bytes)
		.map_err(|e| format!("{what} did not return the expected JSON: {e}"))
}

/// An `application/x-www-form-urlencoded` body.
pub fn form_body(pairs: &[(&str, &str)]) -> String {
	url::form_urlencoded::Serializer::new(String::new())
		.extend_pairs(pairs)
		.finish()
}

/// Reads `<issuer>/.well-known/openid-configuration`. The document must name the same issuer
/// it was fetched for (Discovery §4.3), or a provider could vouch for another one's tokens.
pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<Discovery, Failure> {
	let url = format!(
		"{}/.well-known/openid-configuration",
		issuer.trim_end_matches('/')
	);
	let doc: Discovery = serde_json::from_value(get_json(http, &url).await?)
		.map_err(|e| format!("the discovery document at {url} is incomplete: {e}"))?;
	if !same_issuer(&doc.issuer, issuer) {
		return Err(format!(
			"the discovery document names issuer {} but {issuer} was asked for",
			doc.issuer
		));
	}
	Ok(doc)
}

/// A PKCE pair (RFC 7636 §4): the secret `verifier` stays here, the S256 `challenge` goes to
/// the provider in the authorization request.
#[derive(Debug, Clone)]
pub struct Pkce {
	pub verifier: String,
	pub challenge: String,
}

pub fn pkce() -> Pkce {
	// 32 random bytes make a 43-character verifier, the shortest RFC 7636 allows.
	let mut bytes = [0u8; 32];
	getrandom::fill(&mut bytes).expect("OS RNG unavailable");
	let verifier = URL_SAFE_NO_PAD.encode(bytes);
	let challenge = challenge_of(&verifier);
	Pkce {
		verifier,
		challenge,
	}
}

/// `BASE64URL(SHA256(verifier))`.
pub fn challenge_of(verifier: &str) -> String {
	URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The address to send the browser to.
pub struct AuthRequest<'a> {
	pub endpoint: &'a str,
	pub client_id: &'a str,
	pub redirect_uri: &'a str,
	pub scope: &'a str,
	pub state: &'a str,
	pub nonce: Option<&'a str>,
	pub challenge: &'a str,
}

pub fn authorization_url(req: &AuthRequest<'_>) -> Result<String, Failure> {
	let mut url =
		url::Url::parse(req.endpoint).map_err(|e| format!("bad authorization endpoint: {e}"))?;
	{
		let mut q = url.query_pairs_mut();
		q.append_pair("response_type", "code")
			.append_pair("client_id", req.client_id)
			.append_pair("redirect_uri", req.redirect_uri)
			.append_pair("scope", req.scope)
			.append_pair("state", req.state)
			.append_pair("code_challenge", req.challenge)
			.append_pair("code_challenge_method", "S256");
		if let Some(nonce) = req.nonce {
			q.append_pair("nonce", nonce);
		}
	}
	Ok(url.into())
}

/// What the token endpoint returned.
#[derive(Debug, Deserialize)]
pub struct Tokens {
	#[serde(default)]
	pub id_token: Option<String>,
	#[serde(default)]
	pub access_token: Option<String>,
}

/// Trades the authorization `code` for tokens, proving the PKCE `verifier`.
pub async fn exchange(
	http: &reqwest::Client,
	disc: &Discovery,
	client_id: &str,
	client_secret: &str,
	redirect_uri: &str,
	code: &str,
	verifier: &str,
) -> Result<Tokens, Failure> {
	let mut form = vec![
		("grant_type", "authorization_code"),
		("code", code),
		("redirect_uri", redirect_uri),
		("code_verifier", verifier),
	];
	// `client_secret_basic` is the default when the provider says nothing (Discovery §3);
	// use the body form only when it is the only one offered.
	let methods = &disc.token_endpoint_auth_methods_supported;
	let post_only = !methods.is_empty()
		&& !methods.iter().any(|m| m == "client_secret_basic")
		&& methods.iter().any(|m| m == "client_secret_post");
	let mut request = http.post(&disc.token_endpoint).timeout(TIMEOUT);
	if post_only {
		form.push(("client_id", client_id));
		form.push(("client_secret", client_secret));
	} else {
		request = request.basic_auth(form_encode(client_id), Some(form_encode(client_secret)));
	}
	let response = request
		.header("accept", "application/json")
		.header("content-type", "application/x-www-form-urlencoded")
		.body(form_body(&form))
		.send()
		.await
		.map_err(|e| format!("cannot reach the token endpoint: {e}"))?;
	let status = response.status();
	if !status.is_success() {
		// The body can say why (`invalid_grant`), and holds no secret of ours.
		let body = response.text().await.unwrap_or_default();
		return Err(format!(
			"the token endpoint answered {status}: {}",
			body.chars().take(200).collect::<String>()
		));
	}
	read_json(response, "the token endpoint").await
}

/// RFC 6749 §2.3.1: the id and secret are form-urlencoded before they are put in the
/// Basic header.
fn form_encode(text: &str) -> String {
	url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// The claims podkit reads from an ID token or the userinfo endpoint.
#[derive(Debug, Clone, Default)]
pub struct Claims {
	pub subject: String,
	pub email: Option<String>,
	pub email_verified: Option<bool>,
	pub name: Option<String>,
	/// Every claim, for the groups claim an administrator names.
	pub all: Map<String, Value>,
}

fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
	map.get(key)
		.and_then(Value::as_str)
		.map(str::to_owned)
		.filter(|s| !s.is_empty())
}

/// Some providers send `"email_verified": "true"`.
fn flag(map: &Map<String, Value>, key: &str) -> Option<bool> {
	match map.get(key)? {
		Value::Bool(b) => Some(*b),
		Value::String(s) => Some(s.eq_ignore_ascii_case("true")),
		_ => None,
	}
}

impl Claims {
	fn from_map(all: Map<String, Value>) -> Option<Self> {
		Some(Self {
			subject: text(&all, "sub")?,
			email: text(&all, "email"),
			email_verified: flag(&all, "email_verified"),
			name: text(&all, "name").or_else(|| text(&all, "preferred_username")),
			all,
		})
	}

	/// The values of the claim `name`: a JSON array of strings, or one string (split on
	/// commas, as some providers join them).
	pub fn list(&self, name: &str) -> Vec<String> {
		match self.all.get(name) {
			Some(Value::Array(items)) => items
				.iter()
				.filter_map(Value::as_str)
				.map(str::to_owned)
				.collect(),
			Some(Value::String(s)) => s
				.split(',')
				.map(str::trim)
				.filter(|g| !g.is_empty())
				.map(str::to_owned)
				.collect(),
			_ => Vec::new(),
		}
	}
}

/// The provider's signing keys. A key of a kind podkit does not know is skipped rather
/// than failing the whole set.
async fn keys(http: &reqwest::Client, jwks_uri: &str) -> Result<Vec<Jwk>, Failure> {
	let doc = get_json(http, jwks_uri).await?;
	let list = doc
		.get("keys")
		.and_then(Value::as_array)
		.ok_or_else(|| format!("{jwks_uri} holds no keys"))?;
	Ok(list
		.iter()
		.filter_map(|k| serde_json::from_value::<Jwk>(k.clone()).ok())
		.collect())
}

/// Validates an ID token as OIDC Core §3.1.3.7 asks, in the parts that apply to the
/// authorization code flow:
/// the signature (against the provider's published keys, with an algorithm from [`ALLOWED`]),
/// `iss` equal to the issuer, `aud` containing our client id (and `azp` equal to it when
/// there are several audiences), `exp` in the future, and `nonce` equal to the one this
/// browser's request carried.
pub async fn verify_id_token(
	http: &reqwest::Client,
	disc: &Discovery,
	client_id: &str,
	nonce: &str,
	id_token: &str,
) -> Result<Claims, Failure> {
	let header = decode_header(id_token).map_err(|e| format!("unreadable ID token: {e}"))?;
	if !ALLOWED.contains(&header.alg) {
		return Err(format!(
			"ID token signed with {:?}, which is not accepted",
			header.alg
		));
	}
	let all = keys(http, &disc.jwks_uri).await?;
	let usable: Vec<&Jwk> = match &header.kid {
		Some(kid) => all
			.iter()
			.filter(|k| k.common.key_id.as_deref() == Some(kid.as_str()))
			.collect(),
		// No `kid`: only unambiguous when the provider has one key.
		None if all.len() == 1 => all.iter().collect(),
		None => Vec::new(),
	};
	if usable.is_empty() {
		return Err("no key of the provider matches the ID token".to_owned());
	}

	let mut validation = Validation::new(header.alg);
	validation.leeway = LEEWAY;
	validation.set_audience(&[client_id]);
	validation.set_issuer(&[disc.issuer.as_str()]);
	validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

	let mut last = "no usable key".to_owned();
	for jwk in usable {
		let key = match DecodingKey::from_jwk(jwk) {
			Ok(key) => key,
			Err(e) => {
				last = format!("unusable key: {e}");
				continue;
			}
		};
		match decode::<Map<String, Value>>(id_token, &key, &validation) {
			Ok(data) => return check_claims(data.claims, client_id, nonce),
			Err(e) => last = format!("ID token refused: {e}"),
		}
	}
	Err(last)
}

fn check_claims(map: Map<String, Value>, client_id: &str, nonce: &str) -> Result<Claims, Failure> {
	// With several audiences the party the token was issued to must be us (§3.1.3.7 rule 4).
	if let Some(Value::Array(aud)) = map.get("aud")
		&& aud.len() > 1
		&& text(&map, "azp").as_deref() != Some(client_id)
	{
		return Err("the ID token has several audiences and azp is not this client".to_owned());
	}
	// The nonce is compared in constant time, though it is not a long-lived secret.
	match text(&map, "nonce") {
		Some(got) if crypto::constant_time_eq(&got, nonce) => {}
		Some(_) => return Err("the ID token's nonce does not match this sign-in".to_owned()),
		None => return Err("the ID token has no nonce".to_owned()),
	}
	Claims::from_map(map).ok_or_else(|| "the ID token has no subject".to_owned())
}

/// The userinfo endpoint (Core §5.3), for a provider whose ID token omits the email. The
/// subject must be the ID token's (§5.3.2), or the answer is about someone else.
pub async fn userinfo(
	http: &reqwest::Client,
	endpoint: &str,
	access_token: &str,
	subject: &str,
) -> Result<Claims, Failure> {
	let response = http
		.get(endpoint)
		.bearer_auth(access_token)
		.header("accept", "application/json")
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| format!("cannot reach the userinfo endpoint: {e}"))?;
	if !response.status().is_success() {
		return Err(format!(
			"the userinfo endpoint answered {}",
			response.status()
		));
	}
	let map: Map<String, Value> = read_json(response, "the userinfo endpoint").await?;
	let claims = Claims::from_map(map).ok_or("the userinfo answer has no subject")?;
	if claims.subject != subject {
		return Err("the userinfo answer is about a different subject".to_owned());
	}
	Ok(claims)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn oidc_pkce_challenge_is_the_s256_of_the_verifier() {
		// RFC 7636 appendix B.
		assert_eq!(
			challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
			"E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
		);
		let pair = pkce();
		assert_eq!(pair.verifier.len(), 43);
		assert_eq!(challenge_of(&pair.verifier), pair.challenge);
		assert_ne!(pkce().verifier, pair.verifier);
	}

	#[test]
	fn oidc_authorization_url_carries_pkce_state_and_nonce() {
		let url = authorization_url(&AuthRequest {
			endpoint: "https://idp.example/authorize",
			client_id: "podkit",
			redirect_uri: "https://pk.example/sso/1/callback",
			scope: "openid email",
			state: "st",
			nonce: Some("no"),
			challenge: "ch",
		})
		.unwrap();
		let parsed = url::Url::parse(&url).unwrap();
		let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
		assert_eq!(q["response_type"], "code");
		assert_eq!(q["code_challenge_method"], "S256");
		assert_eq!(q["code_challenge"], "ch");
		assert_eq!(q["state"], "st");
		assert_eq!(q["nonce"], "no");
		assert_eq!(q["scope"], "openid email");
		assert_eq!(q["redirect_uri"], "https://pk.example/sso/1/callback");
	}

	#[test]
	fn oidc_groups_come_from_an_array_or_a_comma_list() {
		let claims = |v: Value| Claims::from_map(v.as_object().unwrap().clone()).unwrap();
		let a = claims(serde_json::json!({"sub": "1", "groups": ["ops", "dev"]}));
		assert_eq!(a.list("groups"), ["ops", "dev"]);
		let b = claims(serde_json::json!({"sub": "1", "groups": "ops, dev,"}));
		assert_eq!(b.list("groups"), ["ops", "dev"]);
		assert!(b.list("missing").is_empty());
		let c = claims(serde_json::json!({"sub": "1", "email_verified": "true"}));
		assert_eq!(c.email_verified, Some(true));
	}
}
