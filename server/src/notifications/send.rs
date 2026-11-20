//! Sends one rendered message to one channel. Errors are plain strings that go into the
//! delivery log, so none of them may carry a URL or a token.

use std::time::Duration;

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use super::channel::{
	Channel, ChannelKind, DEFAULT_NTFY_SERVER, DEFAULT_TELEGRAM_API, open_secret,
};
use super::event::clip;
use super::format::{self, Message, absolute};
use super::settings::{Security, Smtp, load_smtp};
use crate::state::AppState;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const SMTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Sends `msg` through `channel`.
pub async fn send(state: &AppState, channel: &Channel, msg: &Message) -> Result<(), String> {
	let secret =
		open_secret(&state.secrets, channel).map_err(|_| "cannot open the secret".to_owned())?;
	let link = absolute(state.config.public_url.as_deref(), msg.link.as_deref());
	let link = link.as_deref();
	let config = &channel.config.0;
	match channel.kind {
		ChannelKind::Discord => {
			let url = secret.url.ok_or("no webhook URL")?;
			post_json(state, &url, &format::discord(msg, link), &[]).await
		}
		ChannelKind::Slack => {
			let url = secret.url.ok_or("no webhook URL")?;
			post_json(state, &url, &format::slack(msg, link), &[]).await
		}
		ChannelKind::Telegram => {
			let token = secret.token.ok_or("no bot token")?;
			let chat = config.chat_id.as_deref().ok_or("no chat id")?;
			let base = config.api_base.as_deref().unwrap_or(DEFAULT_TELEGRAM_API);
			let url = format!("{}/bot{token}/sendMessage", base.trim_end_matches('/'));
			post_json(state, &url, &format::telegram(msg, chat, link), &[]).await
		}
		ChannelKind::Ntfy => {
			let server = config.server.as_deref().unwrap_or(DEFAULT_NTFY_SERVER);
			let topic = config.topic.as_deref().ok_or("no topic")?;
			let auth = secret
				.token
				.map(|t| ("Authorization", format!("Bearer {t}")));
			let headers: Vec<(&str, String)> = auth.into_iter().collect();
			post_json(state, server, &format::ntfy(msg, topic, link), &headers).await
		}
		ChannelKind::Webhook => {
			let url = secret.url.ok_or("no webhook URL")?;
			let signing = secret.signing.ok_or("no signing secret")?;
			let body =
				serde_json::to_vec(&format::webhook(msg, link)).map_err(|e| e.to_string())?;
			let headers = [
				("X-Podkit-Signature", format::sign(&signing, &body)),
				("X-Podkit-Event", msg.kind.clone()),
			];
			post_bytes(state, &url, body, &headers).await
		}
		ChannelKind::Email => {
			let to = config.to.as_deref().ok_or("no recipient")?;
			let smtp = load_smtp(&state.db, channel.team_id)
				.await
				.map_err(|e| {
					tracing::warn!("cannot load the SMTP settings: {e}");
					"cannot load the SMTP settings".to_owned()
				})?
				.ok_or("the team has no SMTP server set up")?;
			let (subject, body) = format::email(msg, link);
			send_mail(state, &smtp, to, &subject, body).await
		}
	}
}

async fn post_json(
	state: &AppState,
	url: &str,
	body: &serde_json::Value,
	headers: &[(&str, String)],
) -> Result<(), String> {
	let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
	post_bytes(state, url, bytes, headers).await
}

async fn post_bytes(
	state: &AppState,
	url: &str,
	body: Vec<u8>,
	headers: &[(&str, String)],
) -> Result<(), String> {
	let mut request = state
		.http
		.post(url)
		.timeout(HTTP_TIMEOUT)
		.header("Content-Type", "application/json")
		.header("User-Agent", "podkit");
	for (name, value) in headers {
		request = request.header(*name, value);
	}
	let response = request
		.body(body)
		.send()
		.await
		// The error would name the URL, and the URL is the secret.
		.map_err(|e| e.without_url().to_string())?;
	let status = response.status();
	if status.is_success() {
		return Ok(());
	}
	let detail = response.text().await.unwrap_or_default();
	let detail = clip(detail.trim(), 200);
	if detail.is_empty() {
		Err(format!("HTTP {}", status.as_u16()))
	} else {
		Err(format!("HTTP {}: {detail}", status.as_u16()))
	}
}

async fn send_mail(
	state: &AppState,
	smtp: &Smtp,
	to: &str,
	subject: &str,
	body: String,
) -> Result<(), String> {
	let port = u16::try_from(smtp.port).map_err(|_| "invalid SMTP port".to_owned())?;
	let builder = match Security::parse(&smtp.security) {
		Some(Security::StartTls) => {
			AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
		}
		Some(Security::Tls) => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host),
		_ => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
			&smtp.host,
		)),
	}
	.map_err(|e| e.to_string())?;
	let mut builder = builder.port(port).timeout(Some(SMTP_TIMEOUT));
	if let (Some(user), Some(sealed)) = (&smtp.username, &smtp.password) {
		let password = state
			.secrets
			.decrypt(sealed)
			.map_err(|_| "cannot open the SMTP password".to_owned())?;
		builder = builder.credentials(Credentials::new(user.clone(), password));
	}
	let from: Mailbox = smtp
		.from_addr
		.parse()
		.map_err(|_| "the From address isn't valid".to_owned())?;
	let to: Mailbox = to
		.parse()
		.map_err(|_| "the recipient address isn't valid".to_owned())?;
	let mail = lettre::Message::builder()
		.from(from)
		.to(to)
		.subject(subject)
		.header(ContentType::TEXT_PLAIN)
		.body(body)
		.map_err(|e| e.to_string())?;
	builder
		.build()
		.send(mail)
		.await
		.map(|_| ())
		.map_err(|e| clip(&e.to_string(), 300))
}
