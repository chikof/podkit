//! Log retention and search on the server's own disk.
//!
//! The agent follows the logs of every podkit container (Podman's logs API with timestamps)
//! into rotating per-resource files under `<data dir>/logs/` (see [`files`]), keeps them for
//! the team's chosen days and bytes, answers searches over them ([`search`]) and, when the
//! team asked, forwards every line to Loki or an OTLP collector ([`export`]).
//!
//! It is built to stay small: one task per running container, each holding a line buffer
//! and a Podman stream; one open file per active resource, closed when idle; searches that
//! read a file at a time and keep only `limit` lines; an export queue of a fixed length that
//! drops lines when full. Nothing runs for a server with no containers.

pub mod collector;
pub mod detect;
pub mod export;
pub mod files;
pub mod search;

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use proto::{LogConfig, LogExport, LogRecord, LogSearchRequest, LogSearchResult, LogStatus};

use self::export::{Counters, Exporter, Outgoing};
use self::files::Files;

/// What a server keeps until a team says otherwise.
pub const DEFAULT_RETENTION_DAYS: u32 = 7;
/// ... and this many bytes.
pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;
/// The smallest cap the agent accepts: a segment is 4 MiB, and less would delete the lines
/// being written.
pub const MIN_MAX_BYTES: u64 = 16 << 20;
/// The longest retention a team may ask for.
pub const MAX_RETENTION_DAYS: u32 = 3650;

const CONFIG_FILE: &str = "config.json";
const CURSORS_FILE: &str = "cursors.json";

/// A [`LogConfig`] refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadConfig(pub String);

/// The log files of one agent and what goes with them.
pub struct LogStore {
	files: Files,
	config: RwLock<LogConfig>,
	exporter: Mutex<Option<Exporter>>,
	counters: Arc<Counters>,
	/// Last line time stored per container name, so a follower that starts again does not
	/// store a line twice.
	cursors: Mutex<HashMap<String, i64>>,
	cursors_dirty: Mutex<bool>,
	following: AtomicU64,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<LogStore>>> {
	static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<LogStore>>>> = OnceLock::new();
	REGISTRY.get_or_init(Mutex::default)
}

/// The store of an agent data directory: the same one for the collector and the routes.
#[must_use]
pub fn for_dir(data_dir: &Path) -> Arc<LogStore> {
	let mut all = registry().lock().unwrap_or_else(PoisonError::into_inner);
	Arc::clone(
		all.entry(data_dir.to_path_buf())
			.or_insert_with(|| Arc::new(LogStore::open(&data_dir.join("logs")))),
	)
}

fn default_config() -> LogConfig {
	LogConfig {
		retention_days: DEFAULT_RETENTION_DAYS,
		max_bytes: DEFAULT_MAX_BYTES,
		export: None,
	}
}

/// Writes `bytes` to `path` readable by the agent's user only (the config holds a secret).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let tmp = path.with_extension("tmp");
	let mut file = std::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(true)
		.mode(0o600)
		.open(&tmp)?;
	file.write_all(bytes)?;
	file.sync_all()?;
	std::fs::rename(tmp, path)
}

impl LogStore {
	#[must_use]
	/// Opens (or creates) the log directory and reads its saved settings.
	pub fn open(dir: &Path) -> Self {
		Self::with_files(Files::new(dir.to_path_buf()))
	}

	#[must_use]
	/// [`Self::open`] over a chosen [`Files`].
	pub fn with_files(files: Files) -> Self {
		let dir = files.dir().to_path_buf();
		let _ = std::fs::create_dir_all(&dir);
		let config = std::fs::read(dir.join(CONFIG_FILE))
			.ok()
			.and_then(|b| serde_json::from_slice::<LogConfig>(&b).ok())
			.unwrap_or_else(default_config);
		let cursors = std::fs::read(dir.join(CURSORS_FILE))
			.ok()
			.and_then(|b| serde_json::from_slice::<HashMap<String, i64>>(&b).ok())
			.unwrap_or_default();
		let counters = Arc::new(Counters::default());
		let store = Self {
			files,
			config: RwLock::new(config),
			exporter: Mutex::new(None),
			counters,
			cursors: Mutex::new(cursors),
			cursors_dirty: Mutex::new(false),
			following: AtomicU64::new(0),
		};
		store.restart_exporter();
		store
	}

	#[must_use]
	/// The log files.
	pub fn files(&self) -> &Files {
		&self.files
	}

	fn dir(&self) -> &Path {
		self.files.dir()
	}

	#[must_use]
	/// The current retention and forwarding settings.
	pub fn config(&self) -> LogConfig {
		self.config
			.read()
			.unwrap_or_else(PoisonError::into_inner)
			.clone()
	}

	fn exporter(&self) -> MutexGuard<'_, Option<Exporter>> {
		self.exporter.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// (Re)starts forwarding as the config says. Needs a Tokio runtime when forwarding.
	fn restart_exporter(&self) {
		let wanted: Option<LogExport> = self.config().export;
		let mut slot = self.exporter();
		*slot = None;
		if let Some(export) = wanted
			&& tokio::runtime::Handle::try_current().is_ok()
		{
			*slot = Some(Exporter::start(export, Arc::clone(&self.counters)));
		}
	}

	/// Replaces the retention and forwarding settings, keeps them across restarts and applies
	/// the retention at once.
	///
	/// # Errors
	/// The numbers are out of range, or the settings cannot be written.
	pub fn set_config(&self, config: LogConfig) -> Result<(), BadConfig> {
		if !(1..=MAX_RETENTION_DAYS).contains(&config.retention_days) {
			return Err(BadConfig(format!(
				"retention must be between 1 and {MAX_RETENTION_DAYS} days"
			)));
		}
		if config.max_bytes < MIN_MAX_BYTES {
			return Err(BadConfig("the size cap must be at least 16 MiB".to_owned()));
		}
		if let Some(export) = &config.export
			&& !(export.url.starts_with("http://") || export.url.starts_with("https://"))
		{
			return Err(BadConfig(
				"the export address must start with http:// or https://".to_owned(),
			));
		}
		let json = serde_json::to_vec(&config).map_err(|e| BadConfig(e.to_string()))?;
		write_private(&self.dir().join(CONFIG_FILE), &json)
			.map_err(|e| BadConfig(format!("could not save the log settings: {e}")))?;
		let changed_export = self.config().export != config.export;
		*self.config.write().unwrap_or_else(PoisonError::into_inner) = config;
		if changed_export {
			self.restart_exporter();
		}
		self.maintain();
		Ok(())
	}

	/// Stores lines of one resource and offers them to the export.
	///
	/// # Errors
	/// The disk refuses the write.
	pub fn append(&self, svc: &str, records: &[LogRecord]) -> std::io::Result<()> {
		self.files.append(svc, records)?;
		if let Some(exporter) = self.exporter().as_ref() {
			for record in records {
				exporter.offer(Outgoing {
					svc: svc.to_owned(),
					record: record.clone(),
				});
			}
		}
		Ok(())
	}

	/// Runs a search on a blocking thread: it reads files.
	///
	/// # Errors
	/// The query is not valid (a bad regular expression).
	pub async fn search(
		self: &Arc<Self>,
		request: LogSearchRequest,
	) -> Result<LogSearchResult, search::BadQuery> {
		let store = Arc::clone(self);
		tokio::task::spawn_blocking(move || search::run(&store.files, &request))
			.await
			.unwrap_or_else(|_| Err(search::BadQuery("the search stopped unexpectedly".into())))
	}

	#[must_use]
	/// What the logs take and how forwarding is doing.
	pub fn status(&self) -> LogStatus {
		let (bytes, files, oldest) = self.files.usage();
		LogStatus {
			bytes,
			files,
			following: self.following.load(Ordering::Relaxed),
			export_dropped: self.counters.dropped.load(Ordering::Relaxed),
			oldest,
		}
	}

	pub(crate) fn set_following(&self, n: usize) {
		self.following.store(n as u64, Ordering::Relaxed);
	}

	/// Retention, idle writers and the cursor file; once a minute.
	pub fn maintain(&self) {
		let config = self.config();
		let removed = self
			.files
			.enforce(config.retention_days, config.max_bytes, files::now());
		if removed.files > 0 {
			tracing::info!(
				files = removed.files,
				bytes = removed.bytes,
				"old log files removed"
			);
		}
		self.files.close_idle();
		self.flush_cursors();
	}

	#[must_use]
	/// The last line time stored for a container, if any.
	pub fn cursor(&self, container: &str) -> Option<i64> {
		self.cursors
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.get(container)
			.copied()
	}

	/// Remembers that `container` was stored up to `ts`; never moves back.
	pub fn set_cursor(&self, container: &str, ts: i64) {
		let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
		let slot = cursors.entry(container.to_owned()).or_insert(i64::MIN);
		if ts > *slot {
			*slot = ts;
			*self
				.cursors_dirty
				.lock()
				.unwrap_or_else(PoisonError::into_inner) = true;
		}
	}

	/// Forgets the cursors of containers that no longer exist.
	pub fn keep_cursors(&self, existing: &[String]) {
		let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
		let before = cursors.len();
		cursors.retain(|name, _| existing.contains(name));
		if cursors.len() != before {
			*self
				.cursors_dirty
				.lock()
				.unwrap_or_else(PoisonError::into_inner) = true;
		}
	}

	fn flush_cursors(&self) {
		let dirty = std::mem::take(
			&mut *self
				.cursors_dirty
				.lock()
				.unwrap_or_else(PoisonError::into_inner),
		);
		if !dirty {
			return;
		}
		let json = {
			let cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
			serde_json::to_vec(&*cursors).unwrap_or_default()
		};
		if let Err(e) = write_private(&self.dir().join(CURSORS_FILE), &json) {
			tracing::warn!(error = %e, "saving the log cursors failed");
		}
	}
}

#[cfg(test)]
mod tests {
	use proto::Stream;

	use super::*;

	fn record(ts: i64, line: &str) -> LogRecord {
		LogRecord {
			ts,
			stream: Stream::Stdout,
			dep: None,
			level: None,
			line: line.to_owned(),
			svc: String::new(),
		}
	}

	fn config(days: u32, max: u64) -> LogConfig {
		LogConfig {
			retention_days: days,
			max_bytes: max,
			export: None,
		}
	}

	#[test]
	fn logstore_defaults_are_seven_days_and_one_gigabyte() {
		let dir = tempfile::tempdir().unwrap();
		let store = LogStore::open(&dir.path().join("logs"));
		assert_eq!(store.config(), config(7, 1 << 30));
	}

	#[test]
	fn logstore_settings_survive_a_restart_and_are_private() {
		use std::os::unix::fs::PermissionsExt;

		let dir = tempfile::tempdir().unwrap();
		let logs = dir.path().join("logs");
		let store = LogStore::open(&logs);
		store.set_config(config(3, 64 << 20)).unwrap();
		drop(store);
		assert_eq!(LogStore::open(&logs).config(), config(3, 64 << 20));
		let mode = std::fs::metadata(logs.join("config.json"))
			.unwrap()
			.permissions()
			.mode();
		assert_eq!(mode & 0o077, 0, "{mode:o}");
	}

	#[test]
	fn logstore_nonsense_settings_are_refused() {
		let dir = tempfile::tempdir().unwrap();
		let store = LogStore::open(&dir.path().join("logs"));
		assert!(store.set_config(config(0, 1 << 30)).is_err());
		assert!(store.set_config(config(10_000, 1 << 30)).is_err());
		assert!(store.set_config(config(7, 1024)).is_err());
		let mut bad = config(7, 1 << 30);
		bad.export = Some(LogExport {
			kind: proto::LogExportKind::Loki,
			url: "ftp://x".into(),
			authorization: None,
			tenant: None,
			labels: Vec::new(),
		});
		assert!(store.set_config(bad).is_err());
		assert_eq!(
			store.config(),
			config(7, 1 << 30),
			"a refusal changes nothing"
		);
	}

	#[test]
	fn logstore_cursors_only_move_forward_and_are_saved_by_the_sweep() {
		let dir = tempfile::tempdir().unwrap();
		let logs = dir.path().join("logs");
		let store = LogStore::open(&logs);
		store.set_cursor("podkit-app-1-2", 500);
		store.set_cursor("podkit-app-1-2", 300);
		assert_eq!(store.cursor("podkit-app-1-2"), Some(500));
		store.maintain();
		drop(store);
		assert_eq!(LogStore::open(&logs).cursor("podkit-app-1-2"), Some(500));
	}

	#[test]
	fn logstore_the_sweep_applies_the_retention_of_the_settings() {
		let dir = tempfile::tempdir().unwrap();
		let store = LogStore::open(&dir.path().join("logs"));
		store.append("app-1", &[record(1, "x")]).unwrap();
		let (bytes, files, _) = store.files().usage();
		assert!(bytes > 0 && files == 1);
		// 16 MiB is the smallest cap, so nothing is deleted for size here.
		store.set_config(config(7, MIN_MAX_BYTES)).unwrap();
		assert_eq!(store.status().files, 1);
		assert_eq!(store.status().export_dropped, 0);
	}

	#[tokio::test]
	async fn logstore_search_runs_through_the_store() {
		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(LogStore::open(&dir.path().join("logs")));
		store
			.append("app-1", &[record(10, "alpha"), record(20, "beta")])
			.unwrap();
		let result = store
			.search(LogSearchRequest {
				services: vec!["app-1".into()],
				since: None,
				until: None,
				text: Some("BET".into()),
				regex: false,
				levels: Vec::new(),
				deployment: None,
				limit: 10,
				newest: true,
			})
			.await
			.unwrap();
		assert_eq!(result.records.len(), 1);
		assert_eq!(result.records[0].line, "beta");
	}
}
