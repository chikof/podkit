use crypto::ids::generate_id;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
	pub sub: i64,
	pub jti: i64,
	pub exp: i64,
	pub iat: i64,
}

#[derive(Clone)]
pub struct TokenService {
	encoding: EncodingKey,
	decoding: DecodingKey,
}

impl TokenService {
	pub fn new(secret: &[u8]) -> Self {
		Self {
			encoding: EncodingKey::from_secret(secret),
			decoding: DecodingKey::from_secret(secret),
		}
	}

	pub fn issue(&self, user_id: i64) -> anyhow::Result<String> {
		let now = OffsetDateTime::now_utc();
		let claims = Claims {
			sub: user_id,
			jti: generate_id(),
			iat: now.unix_timestamp(),
			exp: (now + time::Duration::hours(24)).unix_timestamp(),
		};
		Ok(encode(&Header::default(), &claims, &self.encoding)?)
	}

	pub fn verify(&self, token: &str) -> anyhow::Result<Claims> {
		let data = decode::<Claims>(token, &self.decoding, &Validation::default())?;
		Ok(data.claims)
	}
}
