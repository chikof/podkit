//! Token counting for one proxied response.
//!
//! Counts come from the `usage` object of the non-streaming JSON body, or
//! from the SSE events of a stream (`OpenAI`'s final chunk; Anthropic's
//! `message_start` and `message_delta`). Both spellings are understood on
//! both kinds of response, and the largest figure seen wins, because the
//! engines repeat or refine counts across events (`docs/spikes/engines.md`).

use std::sync::Arc;

use serde_json::Value;

use super::Gateway;

/// A JSON body bigger than this is forwarded but not read for its usage.
const MAX_JSON_BYTES: usize = 16 << 20;
/// A single SSE line bigger than this is skipped.
const MAX_LINE_BYTES: usize = 1 << 20;

/// What was counted so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
	pub input: u64,
	pub output: u64,
}

impl Tokens {
	/// Takes the larger figure of each count.
	pub fn absorb(&mut self, value: &Value) {
		for usage in [value.get("usage"), value.pointer("/message/usage")]
			.into_iter()
			.flatten()
		{
			let number = |key: &str| usage.get(key).and_then(Value::as_u64);
			// `OpenAI`'s prompt count includes cached tokens. Anthropic's input count
			// does not: its cached part comes separately.
			let input = number("prompt_tokens").or_else(|| {
				number("input_tokens").map(|n| n + number("cache_read_input_tokens").unwrap_or(0))
			});
			let output = number("completion_tokens").or_else(|| number("output_tokens"));
			self.input = self.input.max(input.unwrap_or(0));
			self.output = self.output.max(output.unwrap_or(0));
		}
	}
}

/// Watches the bytes of one response and records the request when dropped,
/// which is when the body has been sent, or the client went away.
pub struct Meter {
	gateway: Arc<Gateway>,
	key_hash: String,
	model_id: i64,
	sse: bool,
	/// The JSON body, or the unfinished SSE line.
	buf: Vec<u8>,
	overflow: bool,
	tokens: Tokens,
}

impl Meter {
	pub fn new(gateway: Arc<Gateway>, key_hash: String, model_id: i64, sse: bool) -> Self {
		Self {
			gateway,
			key_hash,
			model_id,
			sse,
			buf: Vec::new(),
			overflow: false,
			tokens: Tokens::default(),
		}
	}

	/// Looks at a chunk on its way to the client.
	pub fn feed(&mut self, chunk: &[u8]) {
		if !self.sse {
			if self.buf.len() + chunk.len() > MAX_JSON_BYTES {
				self.overflow = true;
				self.buf = Vec::new();
			} else if !self.overflow {
				self.buf.extend_from_slice(chunk);
			}
			return;
		}
		self.buf.extend_from_slice(chunk);
		while let Some(end) = self.buf.iter().position(|b| *b == b'\n') {
			let line: Vec<u8> = self.buf.drain(..=end).collect();
			self.line(&line);
		}
		if self.buf.len() > MAX_LINE_BYTES {
			self.buf = Vec::new();
		}
	}

	fn line(&mut self, line: &[u8]) {
		let Ok(text) = std::str::from_utf8(line) else {
			return;
		};
		let Some(data) = text.trim().strip_prefix("data:") else {
			return;
		};
		// Chunks without a usage object are the vast majority; skip parsing them.
		if !data.contains("\"usage\"") {
			return;
		}
		if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
			self.tokens.absorb(&value);
		}
	}
}

impl Drop for Meter {
	fn drop(&mut self) {
		if self.sse {
			let rest = std::mem::take(&mut self.buf);
			self.line(&rest);
		} else if !self.overflow
			&& let Ok(value) = serde_json::from_slice::<Value>(&self.buf)
		{
			self.tokens.absorb(&value);
		}
		self.gateway
			.record(&self.key_hash, self.model_id, self.tokens);
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn tokens(value: &Value) -> Tokens {
		let mut t = Tokens::default();
		t.absorb(value);
		t
	}

	#[test]
	fn openai_usage() {
		let t = tokens(&json!({"usage": {"prompt_tokens": 27, "completion_tokens": 8}}));
		assert_eq!(
			t,
			Tokens {
				input: 27,
				output: 8
			}
		);
	}

	#[test]
	fn null_usage_is_nothing() {
		assert_eq!(tokens(&json!({"usage": null})), Tokens::default());
	}

	#[test]
	fn anthropic_input_includes_the_cached_part() {
		let t = tokens(&json!({"usage": {
			"input_tokens": 3, "cache_read_input_tokens": 24, "output_tokens": 9
		}}));
		assert_eq!(
			t,
			Tokens {
				input: 27,
				output: 9
			}
		);
	}

	#[test]
	fn the_largest_figure_wins_across_events() {
		let mut t = Tokens::default();
		t.absorb(&json!({"type": "message_start",
			"message": {"usage": {"input_tokens": 12, "output_tokens": 0}}}));
		t.absorb(&json!({"type": "message_delta", "usage": {"output_tokens": 30}}));
		assert_eq!(
			t,
			Tokens {
				input: 12,
				output: 30
			}
		);
	}
}
