//! What a message looks like on each channel. Everything here is a pure function of the
//! [`Message`], so the tests can look at the bytes that would go out.
//!
//! The shapes follow each service's own documentation:
//! - Discord: <https://discord.com/developers/docs/resources/webhook#execute-webhook> with an
//!   embed (<https://discord.com/developers/docs/resources/message#embed-object>); mentions are
//!   switched off with `allowed_mentions` so event text cannot ping anyone.
//! - Slack: <https://api.slack.com/messaging/webhooks> with Block Kit sections; `&`, `<` and
//!   `>` are escaped as <https://api.slack.com/reference/surfaces/formatting#escaping> asks.
//! - Telegram: <https://core.telegram.org/bots/api#sendmessage> with the HTML parse mode, which
//!   needs `&`, `<` and `>` escaped (<https://core.telegram.org/bots/api#html-style>).
//! - ntfy: <https://docs.ntfy.sh/publish/#publish-as-json>.
//! - Webhook: our own JSON, signed with HMAC-SHA256 in `X-Podkit-Signature: sha256=<hex>`.

use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;

use super::event::{Severity, clip};

/// One line of a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestItem {
	pub title: String,
	pub severity: Severity,
	pub at: String,
	pub count: i32,
}

/// A notification as it is stored in the queue and rendered per channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
	pub kind: String,
	pub severity: Severity,
	pub title: String,
	#[serde(default)]
	pub body: String,
	/// A path inside podkit; [`absolute`] turns it into a link.
	#[serde(default)]
	pub link: Option<String>,
	#[serde(default = "one")]
	pub count: i32,
	/// RFC 3339.
	pub at: String,
	pub team_id: i64,
	#[serde(default)]
	pub project_id: Option<i64>,
	#[serde(default)]
	pub environment_id: Option<i64>,
	#[serde(default)]
	pub event_id: Option<i64>,
	/// Set on a digest: what piled up during quiet hours.
	#[serde(default)]
	pub digest: Vec<DigestItem>,
	/// A message from a channel's "Send test" button.
	#[serde(default)]
	pub test: bool,
}

fn one() -> i32 {
	1
}

impl Message {
	/// The title with how often it happened, when more than once.
	pub fn heading(&self) -> String {
		if self.count > 1 {
			format!("{} (×{})", self.title, self.count)
		} else {
			self.title.clone()
		}
	}

	/// The body, or for a digest the list of what it holds.
	pub fn text(&self) -> String {
		if self.digest.is_empty() {
			return self.body.clone();
		}
		let mut out = String::new();
		for item in self.digest.iter().take(DIGEST_LINES) {
			if !out.is_empty() {
				out.push('\n');
			}
			out.push_str("• ");
			out.push_str(&clip(&item.title, 200));
			if item.count > 1 {
				let _ = write!(out, " (×{})", item.count);
			}
		}
		if self.digest.len() > DIGEST_LINES {
			let _ = write!(out, "\n… and {} more", self.digest.len() - DIGEST_LINES);
		}
		out
	}
}

/// Most lines a digest lists before it says "and N more".
pub const DIGEST_LINES: usize = 20;

/// `path` on podkit's public address, or nothing when podkit does not know its address.
pub fn absolute(base: Option<&str>, path: Option<&str>) -> Option<String> {
	let base = base?.trim_end_matches('/');
	let path = path?;
	if path.starts_with('/') {
		Some(format!("{base}{path}"))
	} else {
		None
	}
}

fn colour(severity: Severity) -> u32 {
	match severity {
		Severity::Info => 0x0058_9B5E,
		Severity::Warning => 0x00D9_9B1E,
		Severity::Critical => 0x00D9_3F3F,
	}
}

pub fn discord(msg: &Message, link: Option<&str>) -> Value {
	let mut embed = json!({
		"title": clip(&msg.heading(), 256),
		"description": clip(&msg.text(), 4000),
		"color": colour(msg.severity),
		"timestamp": msg.at,
		"footer": { "text": format!("podkit · {}", msg.severity.as_str()) },
	});
	if let Some(link) = link {
		embed["url"] = json!(link);
	}
	json!({
		"username": "podkit",
		"allowed_mentions": { "parse": [] },
		"embeds": [embed],
	})
}

fn slack_escape(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

pub fn slack(msg: &Message, link: Option<&str>) -> Value {
	let heading = slack_escape(&msg.heading());
	let mut text = format!("*{heading}*");
	let body = msg.text();
	if !body.is_empty() {
		text.push('\n');
		text.push_str(&slack_escape(&clip(&body, 2800)));
	}
	if let Some(link) = link {
		let _ = write!(text, "\n<{link}|Open in podkit>");
	}
	let marker = match msg.severity {
		Severity::Info => ":white_check_mark:",
		Severity::Warning => ":warning:",
		Severity::Critical => ":rotating_light:",
	};
	json!({
		"text": format!("{marker} {}", msg.heading()),
		"blocks": [
			{ "type": "section", "text": { "type": "mrkdwn", "text": format!("{marker} {text}") } },
			{ "type": "context", "elements": [
				{ "type": "mrkdwn", "text": format!("podkit · {}", msg.severity.as_str()) }
			] },
		],
	})
}

fn html_escape(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

pub fn telegram(msg: &Message, chat_id: &str, link: Option<&str>) -> Value {
	let marker = match msg.severity {
		Severity::Info => "✅",
		Severity::Warning => "⚠️",
		Severity::Critical => "🚨",
	};
	let mut text = format!("{marker} <b>{}</b>", html_escape(&msg.heading()));
	let body = msg.text();
	if !body.is_empty() {
		text.push('\n');
		text.push_str(&html_escape(&clip(&body, 3000)));
	}
	if let Some(link) = link {
		// The link goes in an attribute: quotes would end it.
		let safe = html_escape(link).replace('"', "%22");
		let _ = write!(text, "\n<a href=\"{safe}\">Open in podkit</a>");
	}
	json!({
		"chat_id": chat_id,
		"text": text,
		"parse_mode": "HTML",
		"link_preview_options": { "is_disabled": true },
	})
}

pub fn ntfy(msg: &Message, topic: &str, link: Option<&str>) -> Value {
	let (priority, tag) = match msg.severity {
		Severity::Info => (2, "white_check_mark"),
		Severity::Warning => (3, "warning"),
		Severity::Critical => (5, "rotating_light"),
	};
	let mut out = json!({
		"topic": topic,
		"title": clip(&msg.heading(), 250),
		"message": clip(&msg.text(), 3500),
		"priority": priority,
		"tags": [tag],
	});
	if let Some(link) = link {
		out["click"] = json!(link);
	}
	out
}

/// The body of a generic webhook call.
pub fn webhook(msg: &Message, link: Option<&str>) -> Value {
	json!({
		"event": msg.kind,
		"severity": msg.severity.as_str(),
		"title": msg.title,
		"body": msg.text(),
		"link": link,
		"count": msg.count,
		"at": msg.at,
		"team_id": msg.team_id,
		"project_id": msg.project_id,
		"environment_id": msg.environment_id,
		"test": msg.test,
	})
}

/// `sha256=<hex>` over `body`, keyed with the channel's signing secret.
pub fn sign(secret: &str, body: &[u8]) -> String {
	let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
	mac.update(body);
	let tag = mac.finalize().into_bytes();
	let mut out = String::from("sha256=");
	for byte in tag {
		let _ = write!(out, "{byte:02x}");
	}
	out
}

/// Subject and plain-text body of an email.
pub fn email(msg: &Message, link: Option<&str>) -> (String, String) {
	let subject = clip(&format!("[podkit] {}", msg.heading()), 200).replace(['\r', '\n'], " ");
	let mut body = msg.heading();
	let text = msg.text();
	if !text.is_empty() {
		body.push_str("\n\n");
		body.push_str(&text);
	}
	if let Some(link) = link {
		let _ = write!(body, "\n\nOpen in podkit: {link}");
	}
	let _ = write!(body, "\n\n— podkit · {}\n", msg.severity.as_str());
	(subject, body)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn msg() -> Message {
		Message {
			kind: "deploy_failed".to_owned(),
			severity: Severity::Warning,
			title: "Deploy of <web> & co failed".to_owned(),
			body: "Build exited with 1".to_owned(),
			link: Some("/teams/1/projects/2".to_owned()),
			count: 3,
			at: "2026-01-01T00:00:00Z".to_owned(),
			team_id: 1,
			project_id: Some(2),
			environment_id: None,
			event_id: Some(9),
			digest: Vec::new(),
			test: false,
		}
	}

	#[test]
	fn links_need_a_public_address() {
		assert_eq!(
			absolute(Some("https://pk.example/"), Some("/a/b")).as_deref(),
			Some("https://pk.example/a/b")
		);
		assert_eq!(absolute(None, Some("/a")), None);
		assert_eq!(absolute(Some("https://pk.example"), Some("evil")), None);
	}

	#[test]
	fn heading_shows_the_count() {
		assert_eq!(msg().heading(), "Deploy of <web> & co failed (×3)");
	}

	#[test]
	fn slack_and_telegram_escape_markup() {
		let slack = slack(&msg(), Some("https://pk.example/x")).to_string();
		assert!(slack.contains("&lt;web&gt; &amp; co"), "{slack}");
		assert!(slack.contains("<https://pk.example/x|Open in podkit>"));
		let tg = telegram(&msg(), "42", None).to_string();
		assert!(tg.contains("&lt;web&gt; &amp; co"), "{tg}");
		assert!(!tg.contains("<web>"));
	}

	#[test]
	fn discord_never_pings() {
		let d = discord(&msg(), None);
		assert_eq!(d["allowed_mentions"]["parse"], json!([]));
		assert_eq!(d["embeds"][0]["color"], json!(colour(Severity::Warning)));
	}

	#[test]
	fn the_signature_is_hmac_sha256_hex() {
		// RFC 4231 test case 2.
		assert_eq!(
			sign("Jefe", b"what do ya want for nothing?"),
			"sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
		);
	}

	#[test]
	fn a_digest_lists_and_truncates() {
		let mut m = msg();
		m.digest = (0..25)
			.map(|i| DigestItem {
				title: format!("event {i}"),
				severity: Severity::Info,
				at: m.at.clone(),
				count: 1,
			})
			.collect();
		let text = m.text();
		assert_eq!(text.lines().count(), DIGEST_LINES + 1);
		assert!(text.ends_with("… and 5 more"), "{text}");
	}
}
