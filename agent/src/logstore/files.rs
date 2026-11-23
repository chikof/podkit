//! The files: one directory per resource under `<data dir>/logs/`, holding segments named by
//! the time of their first line (`1795248000123.ndjson`), each a line-delimited JSON of
//! [`proto::LogRecord`] without its `svc`. A segment is closed at a size limit and the next
//! one started, so retention deletes whole files and a search skips the ones outside its time
//! range by name alone. No index, no database.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use proto::LogRecord;

/// A segment is closed once it would grow past this.
pub const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
/// A line longer than this is cut when stored.
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// A writer nobody wrote to for this long closes its file.
const IDLE_WRITER: Duration = Duration::from_secs(60);
const EXTENSION: &str = "ndjson";

/// One segment on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
	/// Where the file is.
	pub path: PathBuf,
	/// The time in the name: when the segment's first line was written (unix milliseconds).
	pub first_ts: i64,
	/// Its size in bytes.
	pub size: u64,
	/// Last write (unix milliseconds); no line in the segment is newer.
	pub modified: i64,
}

struct Writer {
	file: File,
	path: PathBuf,
	size: u64,
	used: Instant,
}

/// What a retention pass removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
	/// Files deleted.
	pub files: u64,
	/// Bytes freed.
	pub bytes: u64,
}

/// The log directory and its open writers.
pub struct Files {
	dir: PathBuf,
	segment_bytes: u64,
	writers: Mutex<HashMap<String, Writer>>,
}

/// A resource directory name is a `podkit.service` value: letters, digits, `-`, `_`, `.`.
#[must_use]
pub fn valid_service(svc: &str) -> bool {
	!svc.is_empty()
		&& svc.len() <= 128
		&& svc != "."
		&& svc != ".."
		&& svc
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether the directory of `service` holds the lines a search for `wanted` asks for: the
/// same service, or (for `stack-3`) one of the stack's services (`stack-3-web`).
#[must_use]
pub fn service_matches(wanted: &str, service: &str) -> bool {
	service == wanted
		|| service
			.strip_prefix(wanted)
			.is_some_and(|rest| rest.starts_with('-'))
}

/// Now, in unix milliseconds.
#[must_use]
pub fn now() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn modified_ms(meta: &std::fs::Metadata) -> i64 {
	meta.modified()
		.ok()
		.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
		.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Files {
	#[must_use]
	/// Files under `dir` with the default segment size.
	pub fn new(dir: PathBuf) -> Self {
		Self::with_segment_bytes(dir, SEGMENT_BYTES)
	}

	#[must_use]
	/// [`Self::new`] with a chosen segment size.
	pub fn with_segment_bytes(dir: PathBuf, segment_bytes: u64) -> Self {
		Self {
			dir,
			segment_bytes,
			writers: Mutex::default(),
		}
	}

	#[must_use]
	/// The directory the resource directories are in.
	pub fn dir(&self) -> &Path {
		&self.dir
	}

	fn lock(&self) -> MutexGuard<'_, HashMap<String, Writer>> {
		self.writers.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Appends `records` to the resource's newest segment, starting a new one when it is
	/// full. Lines are cut at [`MAX_LINE_BYTES`].
	///
	/// # Errors
	/// The service name is not a plain directory name, or the disk refuses the write.
	pub fn append(&self, svc: &str, records: &[LogRecord]) -> std::io::Result<()> {
		if !valid_service(svc) {
			return Err(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				"not a service name",
			));
		}
		let dir = self.dir.join(svc);
		let mut writers = self.lock();
		for record in records {
			let stored = LogRecord {
				svc: String::new(),
				line: clip(&record.line).to_owned(),
				..record.clone()
			};
			let mut bytes = serde_json::to_vec(&stored).map_err(std::io::Error::other)?;
			bytes.push(b'\n');
			let full = writers
				.get(svc)
				.is_some_and(|w| w.size > 0 && w.size + bytes.len() as u64 > self.segment_bytes);
			if full {
				writers.remove(svc);
			}
			if !writers.contains_key(svc) {
				writers.insert(
					svc.to_owned(),
					open_writer(&dir, record.ts, self.segment_bytes, full)?,
				);
			}
			let Some(writer) = writers.get_mut(svc) else {
				continue;
			};
			writer.file.write_all(&bytes)?;
			writer.size += bytes.len() as u64;
			writer.used = Instant::now();
		}
		Ok(())
	}

	/// The segments of one resource, oldest first.
	#[must_use]
	pub fn segments(&self, svc: &str) -> Vec<Segment> {
		segments_in(&self.dir.join(svc))
	}

	/// Every resource directory name.
	#[must_use]
	pub fn services(&self) -> Vec<String> {
		let Ok(entries) = std::fs::read_dir(&self.dir) else {
			return Vec::new();
		};
		let mut names: Vec<String> = entries
			.flatten()
			.filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
			.filter_map(|e| e.file_name().into_string().ok())
			.filter(|n| valid_service(n))
			.collect();
		names.sort();
		names
	}

	/// Total size and count of the segments, and the oldest first-line time.
	#[must_use]
	pub fn usage(&self) -> (u64, u64, Option<i64>) {
		let mut bytes = 0;
		let mut files = 0;
		let mut oldest: Option<i64> = None;
		for svc in self.services() {
			for s in self.segments(&svc) {
				bytes += s.size;
				files += 1;
				oldest = Some(oldest.map_or(s.first_ts, |o| o.min(s.first_ts)));
			}
		}
		(bytes, files, oldest)
	}

	/// Closes the writers nobody has written to for a while, so a quiet resource holds no
	/// file descriptor.
	pub fn close_idle(&self) {
		self.lock().retain(|_, w| w.used.elapsed() < IDLE_WRITER);
	}

	/// Deletes segments last written more than `retention_days` before `now` (unix
	/// milliseconds), then the oldest ones while the rest take more than `max_bytes`; empty
	/// resource directories go too.
	pub fn enforce(&self, retention_days: u32, max_bytes: u64, now: i64) -> Removed {
		let cutoff = now.saturating_sub(i64::from(retention_days) * 86_400_000);
		let mut removed = Removed::default();
		let mut keep: Vec<(String, Segment)> = Vec::new();
		for svc in self.services() {
			for s in self.segments(&svc) {
				if s.modified < cutoff {
					self.remove(&svc, &s, &mut removed);
				} else {
					keep.push((svc.clone(), s));
				}
			}
		}
		let mut total: u64 = keep.iter().map(|(_, s)| s.size).sum();
		keep.sort_by_key(|(_, s)| (s.first_ts, s.modified));
		for (svc, s) in &keep {
			if total <= max_bytes {
				break;
			}
			total = total.saturating_sub(s.size);
			self.remove(svc, s, &mut removed);
		}
		for svc in self.services() {
			let dir = self.dir.join(&svc);
			if segments_in(&dir).is_empty() && !self.lock().contains_key(&svc) {
				let _ = std::fs::remove_dir(&dir);
			}
		}
		removed
	}

	fn remove(&self, svc: &str, segment: &Segment, removed: &mut Removed) {
		{
			let mut writers = self.lock();
			if writers.get(svc).is_some_and(|w| w.path == segment.path) {
				writers.remove(svc);
			}
		}
		match std::fs::remove_file(&segment.path) {
			Ok(()) => {
				removed.files += 1;
				removed.bytes += segment.size;
			}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => {
				tracing::warn!(path = %segment.path.display(), error = %e, "removing an old log file failed");
			}
		}
	}
}

fn clip(line: &str) -> &str {
	if line.len() <= MAX_LINE_BYTES {
		return line;
	}
	let mut end = MAX_LINE_BYTES;
	while !line.is_char_boundary(end) {
		end -= 1;
	}
	&line[..end]
}

/// Appends to the newest segment while it has room, otherwise (or when `fresh`, after a
/// rotation) starts one named after `ts`.
fn open_writer(dir: &Path, ts: i64, segment_bytes: u64, fresh: bool) -> std::io::Result<Writer> {
	std::fs::create_dir_all(dir)?;
	if !fresh
		&& let Some(last) = segments_in(dir).pop()
		&& last.size < segment_bytes
	{
		let file = OpenOptions::new().append(true).open(&last.path)?;
		return Ok(Writer {
			file,
			path: last.path,
			size: last.size,
			used: Instant::now(),
		});
	}
	let path = dir.join(format!("{:013}.{EXTENSION}", ts.max(0)));
	let file = OpenOptions::new().create(true).append(true).open(&path)?;
	let size = file.metadata().map_or(0, |m| m.len());
	Ok(Writer {
		file,
		path,
		size,
		used: Instant::now(),
	})
}

fn segments_in(dir: &Path) -> Vec<Segment> {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return Vec::new();
	};
	let mut segments: Vec<Segment> = entries
		.flatten()
		.filter_map(|e| {
			let path = e.path();
			if path.extension().and_then(|x| x.to_str()) != Some(EXTENSION) {
				return None;
			}
			let first_ts: i64 = path.file_stem()?.to_str()?.parse().ok()?;
			let meta = e.metadata().ok()?;
			Some(Segment {
				first_ts,
				size: meta.len(),
				modified: modified_ms(&meta),
				path,
			})
		})
		.collect();
	segments.sort_by_key(|s| s.first_ts);
	segments
}

#[cfg(test)]
mod tests {
	use proto::Stream;

	use super::*;

	fn record(ts: i64, line: &str) -> LogRecord {
		LogRecord {
			ts,
			stream: Stream::Stdout,
			dep: Some(1),
			level: None,
			line: line.to_owned(),
			svc: String::new(),
		}
	}

	fn files(segment_bytes: u64) -> (tempfile::TempDir, Files) {
		let dir = tempfile::tempdir().unwrap();
		let files = Files::with_segment_bytes(dir.path().join("logs"), segment_bytes);
		(dir, files)
	}

	fn set_modified(path: &Path, ms: i64) {
		let file = OpenOptions::new().write(true).open(path).unwrap();
		let at = std::time::UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap());
		file.set_modified(at).unwrap();
	}

	#[test]
	fn logstore_lines_are_written_as_json_without_the_service() {
		let (_dir, files) = files(SEGMENT_BYTES);
		files.append("app-1", &[record(1000, "hello")]).unwrap();
		let segments = files.segments("app-1");
		assert_eq!(segments.len(), 1);
		assert_eq!(segments[0].first_ts, 1000);
		let text = std::fs::read_to_string(&segments[0].path).unwrap();
		assert_eq!(
			text,
			"{\"ts\":1000,\"stream\":\"stdout\",\"dep\":1,\"line\":\"hello\"}\n"
		);
	}

	#[test]
	fn logstore_a_full_segment_is_closed_and_the_next_is_named_by_its_first_line() {
		let (_dir, files) = files(300);
		let records: Vec<LogRecord> = (0..20)
			.map(|i| {
				record(
					1000 + i * 10,
					&format!("line number {i:02} with some padding"),
				)
			})
			.collect();
		files.append("app-1", &records).unwrap();
		let segments = files.segments("app-1");
		assert!(segments.len() >= 4, "{segments:?}");
		assert!(segments.iter().all(|s| s.size <= 300), "{segments:?}");
		let firsts: Vec<i64> = segments.iter().map(|s| s.first_ts).collect();
		let mut sorted = firsts.clone();
		sorted.sort_unstable();
		sorted.dedup();
		assert_eq!(firsts, sorted);
		assert_eq!(firsts[0], 1000);
		let total: usize = segments
			.iter()
			.map(|s| std::fs::read_to_string(&s.path).unwrap().lines().count())
			.sum();
		assert_eq!(total, 20, "no line is lost to a rotation");
	}

	#[test]
	fn logstore_a_restart_appends_to_the_newest_segment_with_room() {
		let (dir, files) = files(10_000);
		files.append("app-1", &[record(1, "one")]).unwrap();
		drop(files);
		let again = Files::with_segment_bytes(dir.path().join("logs"), 10_000);
		again.append("app-1", &[record(2, "two")]).unwrap();
		assert_eq!(again.segments("app-1").len(), 1);
	}

	#[test]
	fn logstore_a_long_line_is_cut_and_a_bad_service_name_refused() {
		let (_dir, files) = files(SEGMENT_BYTES);
		files
			.append("app-1", &[record(1, &"é".repeat(MAX_LINE_BYTES))])
			.unwrap();
		let text = std::fs::read_to_string(&files.segments("app-1")[0].path).unwrap();
		assert!(text.len() < MAX_LINE_BYTES + 100, "{}", text.len());
		for bad in ["", "..", "../x", "a/b", "a b"] {
			assert!(files.append(bad, &[record(1, "x")]).is_err(), "{bad}");
		}
	}

	#[test]
	fn logstore_a_stack_stands_for_its_services() {
		assert!(service_matches("stack-3", "stack-3-web"));
		assert!(service_matches("app-7", "app-7"));
		assert!(!service_matches("app-7", "app-70"));
		assert!(!service_matches("stack-3", "stack-30-web"));
	}

	#[test]
	fn logstore_retention_by_age_deletes_old_segments_and_keeps_new_ones() {
		let (_dir, files) = files(100);
		let day = 86_400_000;
		let now = 100 * day;
		files
			.append("app-1", &[record(now - 10 * day, "old line one")])
			.unwrap();
		files
			.append(
				"app-1",
				&[record(
					now - 9 * day,
					"old line two with padding to rotate it out",
				)],
			)
			.unwrap();
		files
			.append("app-1", &[record(now - day, "recent line, kept here")])
			.unwrap();
		let segments = files.segments("app-1");
		assert!(segments.len() >= 2, "{segments:?}");
		// File times stand in for when each segment was last written.
		set_modified(&segments[0].path, now - 10 * day);
		for s in &segments[1..segments.len() - 1] {
			set_modified(&s.path, now - 9 * day);
		}
		set_modified(&segments[segments.len() - 1].path, now - day);

		let removed = files.enforce(7, u64::MAX, now);
		assert!(removed.files >= 1, "{removed:?}");
		let left = files.segments("app-1");
		assert!(!left.is_empty());
		assert!(left.iter().all(|s| s.modified >= now - 7 * day), "{left:?}");
	}

	#[test]
	fn logstore_retention_by_size_deletes_the_oldest_first_across_resources() {
		let (_dir, files) = files(120);
		for (i, svc) in ["app-1", "app-2"].into_iter().enumerate() {
			for n in 0..6 {
				let ts = 1_000 + i64::from(n) * 100 + i64::try_from(i).unwrap();
				files
					.append(
						svc,
						&[record(ts, "a line that fills most of a segment....")],
					)
					.unwrap();
			}
		}
		let (before, count, _) = files.usage();
		assert!(count >= 6, "{count}");
		let cap = before / 2;
		let removed = files.enforce(36_500, cap, now());
		assert!(removed.files > 0);
		let (after, _, oldest) = files.usage();
		assert!(after <= cap, "{after} > {cap}");
		assert!(after > 0);
		// The oldest went: what is left starts later than the first line written.
		assert!(oldest.unwrap() > 1_000);
	}

	#[test]
	fn logstore_a_resource_with_nothing_left_loses_its_directory() {
		let (_dir, files) = files(SEGMENT_BYTES);
		files.append("app-1", &[record(1, "x")]).unwrap();
		// Idle writers stay for a minute; drop them by hand to see the directory go.
		files.lock().clear();
		let removed = files.enforce(1, 0, now());
		assert_eq!(removed.files, 1);
		assert!(files.services().is_empty());
	}
}
