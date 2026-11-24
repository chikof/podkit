//! Notification channels: where a message can go and what each kind needs to be set up.
//!
//! What is safe to show (a chat id, a topic, an address) is stored as readable `config`.
//! What is not (a webhook URL, a bot token, a signing secret) is JSON sealed with the secret
//! box. Nothing sealed is ever read back into a page or a response; it is opened only to send.

use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use time::OffsetDateTime;

use crate::error::{AppResult, ServerError};
use crate::ids::{NotificationChannelId, TeamId};

/// Where a channel sends to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
	Discord,
	Slack,
	Telegram,
	Ntfy,
	Email,
	Webhook,
}

impl ChannelKind {
	pub const ALL: [Self; 6] = [
		Self::Discord,
		Self::Slack,
		Self::Telegram,
		Self::Ntfy,
		Self::Email,
		Self::Webhook,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Discord => "discord",
			Self::Slack => "slack",
			Self::Telegram => "telegram",
			Self::Ntfy => "ntfy",
			Self::Email => "email",
			Self::Webhook => "webhook",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|k| k.as_str() == s)
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Discord => "Discord",
			Self::Slack => "Slack",
			Self::Telegram => "Telegram",
			Self::Ntfy => "ntfy",
			Self::Email => "Email",
			Self::Webhook => "Webhook",
		}
	}
}

/// The part of a channel that may be shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
	/// Telegram: the chat or channel the bot writes to.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub chat_id: Option<String>,
	/// Telegram: the Bot API's address; only a self-hosted Bot API server changes it.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub api_base: Option<String>,
	/// ntfy: the server, `https://ntfy.sh` unless self-hosted.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub server: Option<String>,
	/// ntfy: the topic.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub topic: Option<String>,
	/// Email: the recipient.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub to: Option<String>,
}

/// The part that is sealed. Which fields are set depends on the kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelSecret {
	/// Discord, Slack and webhook: the URL to post to (it carries a token).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub url: Option<String>,
	/// Telegram: the bot token. ntfy: the access token, when the topic needs one.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub token: Option<String>,
	/// Webhook: the key the body is signed with.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub signing: Option<String>,
}

/// A row of `notification_channels`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Channel {
	pub id: NotificationChannelId,
	pub team_id: TeamId,
	pub name: String,
	pub kind: ChannelKind,
	pub config: Json<ChannelConfig>,
	pub hint: String,
	/// Sealed [`ChannelSecret`] JSON.
	pub secret: Option<Vec<u8>>,
	pub enabled: bool,
	pub last_test_at: Option<OffsetDateTime>,
	pub last_test_ok: Option<bool>,
	pub last_test_error: Option<String>,
}

pub const CHANNEL_COLUMNS: &str = "id, team_id, name, kind, config, hint, secret, enabled, \
	last_test_at, last_test_ok, last_test_error";

/// Everything a channel is made from, as the API and the page post it. Empty strings count as
/// not given.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChannelFields {
	pub url: String,
	pub token: String,
	pub signing_secret: String,
	pub chat_id: String,
	pub api_base: String,
	pub server: String,
	pub topic: String,
	pub to: String,
}

pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
pub const DEFAULT_TELEGRAM_API: &str = "https://api.telegram.org";
const MIN_SIGNING_LEN: usize = 8;

fn invalid(message: &str) -> ServerError {
	ServerError::Validation(message.to_owned())
}

fn present(s: &str) -> Option<String> {
	let t = s.trim();
	(!t.is_empty()).then(|| t.to_owned())
}

fn check_text(value: &str, what: &str, max: usize) -> AppResult<()> {
	if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
	{
		return Err(invalid(&format!("invalid {what}")));
	}
	Ok(())
}

/// An http or https address with a host. Returns it normalised.
pub fn parse_url(raw: &str, what: &str) -> AppResult<url::Url> {
	let url = url::Url::parse(raw.trim())
		.map_err(|_| invalid(&format!("{what} must be an http:// or https:// address")))?;
	if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
		return Err(invalid(&format!(
			"{what} must be an http:// or https:// address"
		)));
	}
	if !url.username().is_empty() || url.password().is_some() {
		return Err(invalid(&format!("{what} must not carry a login")));
	}
	Ok(url)
}

fn host_of(url: &url::Url) -> String {
	match url.port() {
		Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
		None => url.host_str().unwrap_or_default().to_owned(),
	}
}

/// The checked and split form of [`ChannelFields`]: what to show, what to seal, and the hint.
pub struct Built {
	pub config: ChannelConfig,
	pub secret: Option<ChannelSecret>,
	pub hint: String,
}

/// Checks `fields` for `kind` and splits them into the readable and the sealed part.
pub fn build(kind: ChannelKind, fields: &ChannelFields) -> AppResult<Built> {
	match kind {
		ChannelKind::Discord | ChannelKind::Slack => {
			let url = parse_url(&fields.url, "webhook URL")?;
			Ok(Built {
				config: ChannelConfig::default(),
				hint: host_of(&url),
				secret: Some(ChannelSecret {
					url: Some(url.to_string()),
					..ChannelSecret::default()
				}),
			})
		}
		ChannelKind::Webhook => {
			let url = parse_url(&fields.url, "webhook URL")?;
			let signing = fields.signing_secret.trim();
			if signing.len() < MIN_SIGNING_LEN || signing.len() > 255 {
				return Err(invalid(
					"the signing secret must be at least 8 characters long",
				));
			}
			Ok(Built {
				config: ChannelConfig::default(),
				hint: host_of(&url),
				secret: Some(ChannelSecret {
					url: Some(url.to_string()),
					signing: Some(signing.to_owned()),
					..ChannelSecret::default()
				}),
			})
		}
		ChannelKind::Telegram => {
			let token = fields.token.trim();
			check_text(token, "bot token", 255)?;
			let chat = fields.chat_id.trim();
			check_text(chat, "chat id", 100)?;
			let api_base = match present(&fields.api_base) {
				Some(raw) => Some(parse_url(&raw, "Bot API address")?.to_string()),
				None => None,
			};
			Ok(Built {
				hint: format!("chat {chat}"),
				config: ChannelConfig {
					chat_id: Some(chat.to_owned()),
					api_base,
					..ChannelConfig::default()
				},
				secret: Some(ChannelSecret {
					token: Some(token.to_owned()),
					..ChannelSecret::default()
				}),
			})
		}
		ChannelKind::Ntfy => {
			let server = present(&fields.server).unwrap_or_else(|| DEFAULT_NTFY_SERVER.to_owned());
			let server = parse_url(&server, "ntfy server")?;
			let topic = fields.topic.trim();
			check_text(topic, "topic", 64)?;
			if !topic
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
			{
				return Err(invalid("a topic may only contain letters, digits, - and _"));
			}
			Ok(Built {
				hint: format!("{}/{topic}", host_of(&server)),
				config: ChannelConfig {
					server: Some(server.as_str().trim_end_matches('/').to_owned()),
					topic: Some(topic.to_owned()),
					..ChannelConfig::default()
				},
				secret: present(&fields.token).map(|token| ChannelSecret {
					token: Some(token),
					..ChannelSecret::default()
				}),
			})
		}
		ChannelKind::Email => {
			let to = fields.to.trim();
			check_text(to, "email address", 254)?;
			if to.parse::<lettre::message::Mailbox>().is_err() {
				return Err(invalid("that email address isn't valid"));
			}
			Ok(Built {
				hint: to.to_owned(),
				config: ChannelConfig {
					to: Some(to.to_owned()),
					..ChannelConfig::default()
				},
				secret: None,
			})
		}
	}
}

/// Opens a channel's sealed part.
pub fn open_secret(
	secrets: &crypto::age::SecretBox,
	channel: &Channel,
) -> AppResult<ChannelSecret> {
	let Some(sealed) = &channel.secret else {
		return Ok(ChannelSecret::default());
	};
	let json = secrets.decrypt(sealed).map_err(|e| {
		tracing::error!(
			"cannot open the secret of notification channel {}: {e}",
			channel.id
		);
		ServerError::Internal
	})?;
	serde_json::from_str(&json).map_err(|e| {
		tracing::error!(
			"notification channel {} has an unreadable secret: {e}",
			channel.id
		);
		ServerError::Internal
	})
}

/// Seals a channel's secret part.
pub fn seal_secret(secrets: &crypto::age::SecretBox, secret: &ChannelSecret) -> AppResult<Vec<u8>> {
	let json = serde_json::to_string(secret).map_err(|_| ServerError::Internal)?;
	secrets.encrypt(&json).map_err(|e| {
		tracing::error!("failed to encrypt a notification secret: {e}");
		ServerError::Internal
	})
}
