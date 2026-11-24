//! The JSON API as the commands use it: one client, bearer auth, and errors that say what
//! to do next.

use reqwest::{Method, Response, StatusCode};
use serde_json::Value;

/// Exit status for a usage or configuration mistake.
pub const EXIT_USAGE: u8 = 2;
/// Exit status for a deploy that failed, was cancelled, or was rolled back.
pub const EXIT_DEPLOY_FAILED: u8 = 3;

/// Why a command stopped: the message for stderr and the exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
	/// The exit status.
	pub code: u8,
	/// What went wrong, and what to do about it when that is known.
	pub message: String,
}

impl Fail {
	/// A failure with exit status 1.
	pub fn new(message: impl Into<String>) -> Self {
		Self {
			code: 1,
			message: message.into(),
		}
	}

	/// A usage mistake, exit status 2.
	pub fn usage(message: impl Into<String>) -> Self {
		Self {
			code: EXIT_USAGE,
			message: message.into(),
		}
	}
}

/// What a command returns.
pub type Result<T> = std::result::Result<T, Fail>;

/// podkit's API at one address, as one token.
pub struct Api {
	client: reqwest::Client,
	base: String,
	token: String,
}

impl Api {
	/// `base` is podkit's address (`https://podkit.example.com`), without `/api/v1`.
	#[must_use]
	pub fn new(client: reqwest::Client, base: &str, token: &str) -> Self {
		Self {
			client,
			base: base.trim_end_matches('/').to_owned(),
			token: token.to_owned(),
		}
	}

	/// The address requests go to.
	#[must_use]
	pub fn base(&self) -> &str {
		&self.base
	}

	fn url(&self, path: &str) -> String {
		format!("{}/api/v1{path}", self.base)
	}

	async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Response> {
		let mut req = self
			.client
			.request(method, self.url(path))
			.bearer_auth(&self.token)
			.header("accept", "application/json");
		if let Some(body) = body {
			req = req
				.header("content-type", "application/json")
				.body(body.to_string());
		}
		let response = req
			.send()
			.await
			.map_err(|e| Fail::new(format!("could not reach podkit at {}: {e}", self.base)))?;
		if response.status().is_success() {
			return Ok(response);
		}
		let status = response.status();
		let text = response.text().await.unwrap_or_default();
		Err(refused(status, &text))
	}

	async fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
		let response = self.send(method, path, body).await?;
		let text = response
			.text()
			.await
			.map_err(|e| Fail::new(format!("podkit's answer was cut off: {e}")))?;
		if text.trim().is_empty() {
			return Ok(Value::Null);
		}
		serde_json::from_str(&text)
			.map_err(|e| Fail::new(format!("podkit answered something that is not JSON: {e}")))
	}

	/// `GET /api/v1{path}`.
	///
	/// # Errors
	/// When podkit cannot be reached or refuses.
	pub async fn get(&self, path: &str) -> Result<Value> {
		self.json(Method::GET, path, None).await
	}

	/// `POST /api/v1{path}` with an optional JSON body.
	///
	/// # Errors
	/// When podkit cannot be reached or refuses.
	pub async fn post(&self, path: &str, body: Option<&Value>) -> Result<Value> {
		self.json(Method::POST, path, body).await
	}

	/// `DELETE /api/v1{path}`.
	///
	/// # Errors
	/// When podkit cannot be reached or refuses.
	pub async fn delete(&self, path: &str) -> Result<Value> {
		self.json(Method::DELETE, path, None).await
	}

	/// `GET /api/v1{path}` as server-sent events; read it with [`crate::sse::Parser`].
	///
	/// # Errors
	/// When podkit cannot be reached or refuses.
	pub async fn events(&self, path: &str) -> Result<Response> {
		let response = self
			.client
			.get(self.url(path))
			.bearer_auth(&self.token)
			.header("accept", "text/event-stream")
			.send()
			.await
			.map_err(|e| Fail::new(format!("could not reach podkit at {}: {e}", self.base)))?;
		if response.status().is_success() {
			return Ok(response);
		}
		let status = response.status();
		let text = response.text().await.unwrap_or_default();
		Err(refused(status, &text))
	}
}

/// The message for a refused request, from its status and podkit's `{"error": …}`.
#[must_use]
pub fn refused(status: StatusCode, body: &str) -> Fail {
	let detail = serde_json::from_str::<Value>(body)
		.ok()
		.and_then(|v| v["error"].as_str().map(str::to_owned))
		.unwrap_or_default();
	let detail = if detail.is_empty() {
		String::new()
	} else {
		format!(": {detail}")
	};
	let code = status.as_u16();
	let message = match code {
		401 => format!(
			"podkit refused the token ({code}{detail}). It may be revoked or expired: make a new one in Settings -> API tokens and run podkit login"
		),
		403 => format!(
			"not allowed ({code}{detail}). The token lacks a scope, or your role lacks the permission"
		),
		404 => format!(
			"not found ({code}{detail}). Check the name, and that the token may see that team and project"
		),
		429 => format!("podkit asks to slow down ({code}{detail})"),
		_ => format!("podkit answered {code}{detail}"),
	};
	Fail::new(message)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn refusals_name_the_status_the_detail_and_what_to_do() {
		let f = refused(StatusCode::UNAUTHORIZED, r#"{"error":"invalid token"}"#);
		assert_eq!(f.code, 1);
		assert!(f.message.contains("401: invalid token"), "{}", f.message);
		assert!(f.message.contains("podkit login"));
		let f = refused(
			StatusCode::FORBIDDEN,
			r#"{"error":"the token lacks the write scope"}"#,
		);
		assert!(f.message.contains("403: the token lacks the write scope"));
		let f = refused(StatusCode::BAD_GATEWAY, "<html>");
		assert_eq!(f.message, "podkit answered 502");
	}
}
