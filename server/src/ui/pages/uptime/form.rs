//! The check form: the fields of a new check and of an edit, the signals they bind and the
//! conversion to the service's input. Both pages use the same signal names; a page has one form.

use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::ui::components::{CHECKBOX, INPUT, LABEL, SELECT};
use crate::uptime::Check;
use crate::uptime::service::CheckInput;

/// A number a field may send as a number or as the text typed into it.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::Number(n) => n.as_i64().and_then(|n| i32::try_from(n).ok()).unwrap_or(0),
		Value::String(s) => s.trim().parse().unwrap_or(0),
		_ => 0,
	})
}

/// What the form posts.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Signals {
	name: String,
	kind: String,
	url: String,
	method: String,
	#[serde(deserialize_with = "lenient")]
	expected_status: i32,
	keyword: String,
	host: String,
	#[serde(deserialize_with = "lenient")]
	port: i32,
	#[serde(deserialize_with = "lenient")]
	timeout_secs: i32,
	#[serde(deserialize_with = "lenient")]
	interval_secs: i32,
	#[serde(deserialize_with = "lenient")]
	tls_warn_days: i32,
	tls_skip_verify: bool,
	enabled: bool,
	status_visible: bool,
	status_name: String,
}

impl Signals {
	pub fn input(self) -> CheckInput {
		CheckInput {
			name: self.name,
			kind: self.kind,
			url: self.url,
			method: self.method,
			expected_status: self.expected_status,
			keyword: self.keyword,
			host: self.host,
			port: self.port,
			timeout_secs: self.timeout_secs,
			interval_secs: self.interval_secs,
			tls_warn_days: Some(self.tls_warn_days).filter(|d| *d > 0),
			tls_skip_verify: self.tls_skip_verify,
			enabled: self.enabled,
			status_visible: self.status_visible,
			status_name: self.status_name,
		}
	}
}

/// The starting signals: a blank form, or the check being edited.
pub fn initial(check: Option<&Check>) -> Value {
	match check {
		None => json!({
			"name": "", "kind": "http", "url": "https://", "method": "GET", "expected_status": 200,
			"keyword": "", "host": "", "port": 443, "timeout_secs": 10, "interval_secs": 60,
			"tls_warn_days": 14, "tls_skip_verify": false, "enabled": true, "status_visible": false,
			"status_name": "",
		}),
		Some(c) => json!({
			"name": c.name, "kind": c.kind, "url": c.url.clone().unwrap_or_default(),
			"method": c.method, "expected_status": c.expected_status,
			"keyword": c.keyword.clone().unwrap_or_default(),
			"host": c.host.clone().unwrap_or_default(), "port": c.port.unwrap_or(443),
			"timeout_secs": c.timeout_secs, "interval_secs": c.interval_secs,
			"tls_warn_days": c.tls_warn_days.unwrap_or(0), "tls_skip_verify": c.tls_skip_verify,
			"enabled": c.enabled,
			"status_visible": c.status_visible, "status_name": c.status_name.clone().unwrap_or_default(),
		}),
	}
}

/// The fields. `http` is whether the form starts on the HTTP kind (the other kind's fields start
/// hidden, so a page without script still shows the right ones).
pub fn fields(http: bool) -> Markup {
	let hide = |shown: bool| if shown { "" } else { "display: none" };
	html! {
		div class="ui-pair" {
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" required maxlength="60" autocomplete="off" "data-bind:name";
			}
			label class=(LABEL) {
				"Type"
				select class=(SELECT) "data-bind:kind" {
					option value="http" selected[http] { "HTTP(S) request" }
					option value="tcp" selected[!http] { "TCP connection" }
				}
			}
		}
		div class="flex flex-col gap-4" "data-show"="$kind == 'http'" style=(hide(http)) {
			label class=(LABEL) {
				"URL"
				input class=(INPUT) type="url" autocomplete="off" spellcheck="false" autocapitalize="off" inputmode="url"
					placeholder="https://example.com/health" "data-bind:url";
				span class="ui-hint" { "Do not put a password or token in the address: it is shown to everyone on the team." }
			}
			div class="ui-pair" {
				label class=(LABEL) {
					"Method"
					select class=(SELECT) "data-bind:method" {
						option value="GET" { "GET" }
						option value="HEAD" { "HEAD" }
					}
				}
				label class=(LABEL) {
					"Expected status"
					input class=(INPUT) type="number" min="0" max="599" "data-bind:expected_status";
					span class="ui-hint" { "0 accepts anything below 400." }
				}
			}
			div class="ui-pair" {
				label class=(LABEL) {
					"Keyword (optional)"
					input class=(INPUT) type="text" maxlength="200" autocomplete="off" "data-bind:keyword";
					span class="ui-hint" { "The answer must contain it. Needs GET." }
				}
				label class=(LABEL) {
					"Warn before the certificate expires (days)"
					input class=(INPUT) type="number" min="0" max="90" "data-bind:tls_warn_days";
					span class="ui-hint" { "HTTPS only. 0 turns the warning off." }
				}
			}
			div class="flex flex-col gap-1" {
				div class="ui-check" {
					input id="chk-skip-tls" type="checkbox" class=(CHECKBOX) aria-describedby="skip-tls-hint" "data-bind:tls_skip_verify";
					label for="chk-skip-tls" class="cursor-pointer" { "Skip TLS verification" }
				}
				span id="skip-tls-hint" class="ui-hint" {
					"For a service with a self-signed certificate on your own network. The check then trusts any certificate, so it cannot tell you the site was swapped. HTTPS only."
				}
			}
		}
		div class="ui-pair" "data-show"="$kind == 'tcp'" style=(hide(!http)) {
			label class=(LABEL) {
				"Host"
				input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off" "data-bind:host";
			}
			label class=(LABEL) {
				"Port"
				input class=(INPUT) type="number" min="1" max="65535" "data-bind:port";
			}
		}
		div class="ui-pair" {
			label class=(LABEL) {
				"Check every (seconds)"
				input class=(INPUT) type="number" min="30" max="86400" required "data-bind:interval_secs";
				span class="ui-hint" { "30 at the least." }
			}
			label class=(LABEL) {
				"Give up after (seconds)"
				input class=(INPUT) type="number" min="1" max="60" required "data-bind:timeout_secs";
			}
		}
		div class="ui-check" {
			input id="chk-enabled" type="checkbox" class=(CHECKBOX) "data-bind:enabled";
			label for="chk-enabled" class="cursor-pointer" { "Run this check" }
		}
		fieldset class="flex flex-col gap-3" {
			legend class="text-sm font-medium" { "Public status page" }
			div class="ui-check" {
				input id="chk-visible" type="checkbox" class=(CHECKBOX) "data-bind:status_visible";
				label for="chk-visible" class="cursor-pointer" { "Show this check on the status page" }
			}
			label class=(LABEL) {
				"Name on the status page"
				input class=(INPUT) type="text" maxlength="60" autocomplete="off" placeholder="Same as the name" "data-bind:status_name";
				span class="ui-hint" { "The page shows this name, the state and the uptime, never the address." }
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn numbers_arrive_as_numbers_or_as_text() {
		let s: Signals = serde_json::from_value(json!({
			"name": "a", "kind": "tcp", "port": "8080", "interval_secs": 45, "tls_warn_days": "0",
		}))
		.unwrap();
		let input = s.input();
		assert_eq!((input.port, input.interval_secs), (8080, 45));
		assert_eq!(input.tls_warn_days, None, "0 means no warning");
	}
}
