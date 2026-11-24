//! Reading server-sent events (build and runtime logs) from a byte stream that arrives in
//! pieces of any size: <https://html.spec.whatwg.org/multipage/server-sent-events.html>.

/// One event: its `event:` name (`message` when it had none) and its `data:` lines joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
	/// The `event:` field.
	pub name: String,
	/// The `data:` lines, joined by line breaks.
	pub data: String,
}

/// Collects bytes and hands out the events completed so far.
#[derive(Debug, Default)]
pub struct Parser {
	/// Bytes after the last line break: a line, or a UTF-8 character, still arriving.
	pending: Vec<u8>,
	name: String,
	data: Vec<String>,
}

impl Parser {
	/// Adds a chunk and returns every event it completed.
	pub fn feed(&mut self, chunk: &[u8]) -> Vec<Event> {
		self.pending.extend_from_slice(chunk);
		let mut events = Vec::new();
		while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
			let raw: Vec<u8> = self.pending.drain(..=end).collect();
			let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
			let line = line.strip_suffix('\r').unwrap_or(&line);
			if let Some(event) = self.line(line) {
				events.push(event);
			}
		}
		events
	}

	fn line(&mut self, line: &str) -> Option<Event> {
		if line.is_empty() {
			if self.data.is_empty() && self.name.is_empty() {
				return None;
			}
			let name = std::mem::take(&mut self.name);
			return Some(Event {
				name: if name.is_empty() {
					"message".to_owned()
				} else {
					name
				},
				data: std::mem::take(&mut self.data).join("\n"),
			});
		}
		// A comment: keep-alives are sent as these.
		if line.starts_with(':') {
			return None;
		}
		let (field, value) = line.split_once(':').unwrap_or((line, ""));
		let value = value.strip_prefix(' ').unwrap_or(value);
		match field {
			"event" => value.clone_into(&mut self.name),
			"data" => self.data.push(value.to_owned()),
			_ => {}
		}
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ev(name: &str, data: &str) -> Event {
		Event {
			name: name.to_owned(),
			data: data.to_owned(),
		}
	}

	#[test]
	fn events_split_anywhere_come_out_whole() {
		let stream =
			"event: log\ndata: {\"line\":\"héllo\"}\n\n:keep-alive\n\nevent: end\ndata: \n\n";
		let want = vec![ev("log", "{\"line\":\"héllo\"}"), ev("end", "")];
		// Every split point, including inside the two-byte `é`.
		for at in 0..stream.len() {
			let (a, b) = stream.as_bytes().split_at(at);
			let mut p = Parser::default();
			let mut got = p.feed(a);
			got.extend(p.feed(b));
			assert_eq!(got, want, "split at {at}");
		}
	}

	#[test]
	fn crlf_multi_line_data_and_unnamed_events() {
		let mut p = Parser::default();
		let got = p.feed(b"data: one\r\ndata: two\r\n\r\nevent: lagged\ndata:3\n\n");
		assert_eq!(got, [ev("message", "one\ntwo"), ev("lagged", "3")]);
	}

	#[test]
	fn an_event_without_its_blank_line_is_not_yet_out() {
		let mut p = Parser::default();
		assert!(p.feed(b"event: end\ndata: x\n").is_empty());
		assert_eq!(p.feed(b"\n"), [ev("end", "x")]);
	}
}
