//! The template catalog on disk, and fetching a new one.
//!
//! Templates are not in the binary. They come from one place, the podkit-templates
//! repository, whose website publishes `index.json` and `catalog.tar.gz`
//! (its `docs/catalog.md`). An administrator asks for a refresh; podkit downloads both,
//! checks the archive against the hash in the index, unpacks it under
//! `<data dir>/catalog` and reads the gallery from there, with no network, until the next
//! refresh.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::registry;
use crate::error::ServerError;
use crate::net::outbound::read_capped;
use crate::state::AppState;

/// The index format this podkit reads.
const FORMAT: u32 = 1;
const MAX_INDEX_BYTES: usize = 8 << 20;
const MAX_ARCHIVE_BYTES: usize = 64 << 20;
/// What the archive may unpack to: a small archive can claim to hold far more.
const MAX_UNPACKED_BYTES: u64 = 256 << 20;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Deserialize)]
struct Index {
	format: u32,
	commit: String,
	archive: Archive,
	#[serde(default)]
	templates: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
	id: String,
	revision: String,
}

impl Index {
	fn read(self, dir: &Path) -> registry::Registry {
		let revisions = self
			.templates
			.into_iter()
			.map(|e| (e.id, e.revision))
			.collect();
		registry::read(dir, self.commit, &revisions)
	}
}

#[derive(Deserialize)]
struct Archive {
	name: String,
	sha256: String,
}

/// What a refresh brought in.
#[derive(Debug, Serialize)]
pub struct Refreshed {
	/// Commit of the template repository.
	pub commit: String,
	/// Templates in the gallery.
	pub templates: usize,
	/// Templates this podkit could not load, with why. They are left out.
	pub skipped: Vec<(String, String)>,
}

fn dir(data_dir: &Path) -> PathBuf {
	data_dir.join("catalog")
}

/// Reads the catalog a previous refresh left in `data_dir`, if any. Called once at start.
pub fn open(data_dir: &Path) {
	let dir = dir(data_dir);
	if let Some(index) = index_of(&dir) {
		registry::install(index.read(&dir));
	}
}

/// `server check-templates <dir>`: loads every template under `<dir>/templates` the way a
/// refresh does and says which do not load. The template repository's checks cannot know
/// that; this is how its catalog is tried against a podkit before it is published.
#[must_use]
pub fn check_cli() -> Option<i32> {
	let args: Vec<String> = std::env::args().collect();
	if args.get(1).map(String::as_str) != Some("check-templates") {
		return None;
	}
	let Some(dir) = args.get(2) else {
		eprintln!("usage: server check-templates <dir with a templates/ folder>");
		return Some(2);
	};
	let registry = registry::read(
		Path::new(dir),
		String::new(),
		&std::collections::BTreeMap::new(),
	);
	for (id, why) in &registry.errors {
		eprintln!("{id}: {why}");
	}
	println!(
		"{} templates load, {} do not",
		registry.templates.len(),
		registry.errors.len()
	);
	Some(i32::from(
		!registry.errors.is_empty() || registry.templates.is_empty(),
	))
}

fn index_of(dir: &Path) -> Option<Index> {
	serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()
}

fn failed(what: &str, why: impl std::fmt::Display) -> ServerError {
	ServerError::Import(format!("The template catalog {what}: {why}."))
}

async fn fetch(s: &AppState, url: &str, max: usize) -> Result<Vec<u8>, ServerError> {
	let response = s
		.outbound
		.get(url)
		.await
		.map_err(|d| failed("was refused by the outbound policy", d.reason()))?
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| failed("could not be reached", e.without_url()))?;
	if !response.status().is_success() {
		return Err(failed("answered", response.status()));
	}
	read_capped(response, max)
		.await
		.map(|b| b.to_vec())
		.map_err(|e| failed("could not be read", format!("{e:?}")))
}

/// Unpacks the archive into the empty `into`. Only plain files below `templates/` are
/// taken, and none may leave `into`.
fn unpack(archive: &[u8], into: &Path) -> std::io::Result<()> {
	let bad = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
	let mut unpacked = 0u64;
	for entry in tar::Archive::new(GzDecoder::new(archive)).entries()? {
		let mut entry = entry?;
		unpacked = unpacked.saturating_add(entry.size());
		if unpacked > MAX_UNPACKED_BYTES {
			return Err(bad("the archive unpacks to too much".to_owned()));
		}
		let path = entry.path()?.into_owned();
		if entry.header().entry_type() != tar::EntryType::Regular || !path.starts_with("templates")
		{
			return Err(bad(format!("unexpected entry {}", path.display())));
		}
		if !entry.unpack_in(into)? {
			return Err(bad(format!("{} leaves the catalog", path.display())));
		}
	}
	Ok(())
}

/// Downloads the catalog from `Config::template_catalog_url` and makes it the gallery.
/// The old one stays in place when anything fails.
///
/// # Errors
/// `Import` when the catalog cannot be fetched, is for a newer podkit, or does not match
/// its hash; I/O errors from the data directory.
pub async fn refresh(s: &AppState) -> Result<Refreshed, ServerError> {
	// Two refreshes at once would move the same folders under each other.
	static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
	let _one_at_a_time = RUNNING.lock().await;
	let base = s.config.template_catalog_url.trim_end_matches('/');
	let index_bytes = fetch(s, &format!("{base}/index.json"), MAX_INDEX_BYTES).await?;
	let index: Index =
		serde_json::from_slice(&index_bytes).map_err(|e| failed("has an unreadable index", e))?;
	if index.format != FORMAT {
		return Err(failed(
			"needs another podkit",
			format!(
				"it is format {}, this podkit reads format {FORMAT}",
				index.format
			),
		));
	}
	if index.archive.name.contains('/') {
		return Err(failed(
			"has an unusable index",
			"the archive name is a path",
		));
	}
	let archive = fetch(
		s,
		&format!("{base}/{}", index.archive.name),
		MAX_ARCHIVE_BYTES,
	)
	.await?;
	let hash = Sha256::digest(&archive)
		.iter()
		.fold(String::new(), |mut s, b| {
			let _ = write!(s, "{b:02x}");
			s
		});
	if hash != index.archive.sha256 {
		return Err(failed(
			"was not accepted",
			"the archive does not match the hash in the index",
		));
	}

	let live = dir(&s.config.data_dir);
	let registry = tokio::task::spawn_blocking(move || -> std::io::Result<registry::Registry> {
		let next = live.with_extension("new");
		let previous = live.with_extension("old");
		for stale in [&next, &previous] {
			if stale.exists() {
				std::fs::remove_dir_all(stale)?;
			}
		}
		std::fs::create_dir_all(&next)?;
		unpack(&archive, &next)?;
		std::fs::write(next.join("index.json"), &index_bytes)?;
		if live.exists() {
			std::fs::rename(&live, &previous)?;
		}
		std::fs::rename(&next, &live)?;
		if previous.exists() {
			std::fs::remove_dir_all(&previous)?;
		}
		Ok(index.read(&live))
	})
	.await
	.map_err(|e| failed("could not be unpacked", e))??;

	let refreshed = Refreshed {
		commit: registry.commit.clone(),
		templates: registry.templates.len(),
		skipped: registry.errors.clone(),
	};
	registry::install(registry);
	Ok(refreshed)
}
