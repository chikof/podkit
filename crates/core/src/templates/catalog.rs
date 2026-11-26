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
/// Files in an archive: three a template, with room for ten times today's catalog.
const MAX_ENTRIES: usize = 20_000;
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
			// `template_deploys.revision` holds 32 characters; a longer one is no revision.
			.filter(|e| e.revision.len() <= 32)
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
	recover(&dir);
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

/// A refresh that died between its two renames left the catalog under its old name.
fn recover(live: &Path) {
	let previous = live.with_extension("old");
	if !live.exists() && previous.exists() {
		let _ = std::fs::rename(&previous, live);
	}
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
	// The reader is cut off too: `tar` reads a long-name or extension record whole into
	// memory before this loop sees an entry.
	let bounded = std::io::Read::take(GzDecoder::new(archive), MAX_UNPACKED_BYTES);
	for (count, entry) in tar::Archive::new(bounded).entries()?.enumerate() {
		if count >= MAX_ENTRIES {
			return Err(bad(format!("more than {MAX_ENTRIES} files")));
		}
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

/// Unpacks a checked archive beside the catalog, reads it, and only then puts it in the
/// catalog's place. Nothing is replaced by a catalog this podkit cannot read.
fn replace(
	live: &Path,
	archive: &[u8],
	index_bytes: &[u8],
	index: Index,
) -> std::io::Result<registry::Registry> {
	let next = live.with_extension("new");
	let previous = live.with_extension("old");
	for stale in [&next, &previous] {
		if stale.exists() {
			std::fs::remove_dir_all(stale)?;
		}
	}
	std::fs::create_dir_all(&next)?;
	unpack(archive, &next)?;
	std::fs::write(next.join("index.json"), index_bytes)?;
	let registry = index.read(&next);
	if registry.templates.is_empty() {
		let why = registry.errors.first().map_or_else(
			|| "it holds no template".to_owned(),
			|(id, why)| format!("this podkit can read none of its templates ({id}: {why})"),
		);
		std::fs::remove_dir_all(&next)?;
		return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, why));
	}
	if live.exists() {
		std::fs::rename(live, &previous)?;
	}
	if let Err(e) = std::fs::rename(&next, live) {
		let _ = std::fs::rename(&previous, live);
		return Err(e);
	}
	// The new catalog is in place: failing to delete the old one must not undo that.
	let _ = std::fs::remove_dir_all(&previous);
	Ok(registry)
}

/// Downloads the catalog from `Config::template_catalog_url` and makes it the gallery.
/// The old one stays in place when anything fails, and when this podkit can read none of
/// the new one's templates.
///
/// # Errors
/// `Import` when the catalog cannot be fetched, is for a newer podkit, does not match its
/// hash or cannot be read; I/O errors from the data directory.
pub async fn refresh(s: &AppState) -> Result<Refreshed, ServerError> {
	// A caller that goes away (a closed tab) must not stop this half-way: disk and memory
	// would disagree, and the next refresh would start under this one's renames.
	let s = s.clone();
	tokio::spawn(async move { refresh_now(&s).await })
		.await
		.map_err(|e| failed("could not be replaced", e))?
}

async fn refresh_now(s: &AppState) -> Result<Refreshed, ServerError> {
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
	let registry =
		tokio::task::spawn_blocking(move || replace(&live, &archive, &index_bytes, index))
			.await
			.map_err(|e| failed("could not be unpacked", e))?
			.map_err(|e| failed("was not accepted", e))?;

	let refreshed = Refreshed {
		commit: registry.commit.clone(),
		templates: registry.templates.len(),
		skipped: registry.errors.clone(),
	};
	registry::install(registry);
	Ok(refreshed)
}

#[cfg(test)]
mod tests {
	use std::io::Write as _;

	use super::*;

	/// A catalog archive of `files` (path, content), as the template repository builds it.
	fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
		let mut tar = tar::Builder::new(Vec::new());
		for (path, content) in files {
			let mut header = tar::Header::new_gnu();
			header.set_size(content.len() as u64);
			header.set_mode(0o644);
			header.set_cksum();
			// `append_data` refuses `..`; a hostile archive is not made with it.
			header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
			header.set_cksum();
			tar.append(&header, *content).unwrap();
		}
		let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
		gz.write_all(&tar.into_inner().unwrap()).unwrap();
		gz.finish().unwrap()
	}

	fn fixture(id: &str, file: &str) -> Vec<u8> {
		let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
		std::fs::read(dir.join(id).join(file)).unwrap()
	}

	fn index(revision: &str) -> (Vec<u8>, Index) {
		let bytes = serde_json::json!({
			"format": FORMAT,
			"commit": "abc1234",
			"archive": { "name": "catalog.tar.gz", "sha256": "" },
			"templates": [{ "id": "gotify", "revision": revision }],
		})
		.to_string()
		.into_bytes();
		let index = serde_json::from_slice(&bytes).unwrap();
		(bytes, index)
	}

	fn gotify() -> Vec<u8> {
		let toml = fixture("gotify", "template.toml");
		let compose = fixture("gotify", "docker-compose.yml");
		let logo = fixture("gotify", "logo.svg");
		archive(&[
			("templates/gotify/template.toml", &toml),
			("templates/gotify/docker-compose.yml", &compose),
			("templates/gotify/logo.svg", &logo),
		])
	}

	#[test]
	fn a_catalog_replaces_the_one_there_and_is_read_back_with_its_revisions() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.join("templates/old")).unwrap();

		let (bytes, parsed) = index("0123456789abcdef");
		let registry = replace(&live, &gotify(), &bytes, parsed).unwrap();
		assert_eq!(registry.commit, "abc1234");
		assert_eq!(registry.templates["gotify"].revision, "0123456789abcdef");
		assert!(!live.join("templates/old").exists());
		assert!(!live.with_extension("old").exists());
		assert!(!live.with_extension("new").exists());

		// What a restart reads is the same catalog.
		let again = index_of(&live).unwrap().read(&live);
		assert_eq!(again.templates["gotify"].revision, "0123456789abcdef");
		assert!(again.errors.is_empty());
	}

	#[test]
	fn a_catalog_this_podkit_cannot_read_leaves_the_one_there() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.join("templates/kept")).unwrap();

		let unreadable = archive(&[
			(
				"templates/x/template.toml",
				b"id = \"x\"\nfrom_the_future = true\n",
			),
			("templates/x/docker-compose.yml", b"services: {}\n"),
		]);
		let (bytes, parsed) = index("");
		let refused = replace(&live, &unreadable, &bytes, parsed).err().unwrap();
		assert!(refused.to_string().contains("can read none"), "{refused}");
		assert!(live.join("templates/kept").exists());
		assert!(!live.with_extension("new").exists());
	}

	#[test]
	fn an_archive_may_only_hold_plain_files_under_templates() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		for path in [
			"templates/../../escaped",
			"elsewhere/file",
			"/etc/passwd-like",
		] {
			let (bytes, parsed) = index("");
			let hostile = archive(&[(path, b"x")]);
			assert!(replace(&live, &hostile, &bytes, parsed).is_err(), "{path}");
		}
		assert!(!tmp.path().join("escaped").exists());
		assert!(!live.exists());
	}

	#[test]
	fn an_archive_that_claims_more_than_it_may_hold_is_cut_off() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		// A long-name record is read whole into memory by `tar`: this one says 1 GiB.
		let mut header = tar::Header::new_gnu();
		header.set_entry_type(tar::EntryType::GNULongName);
		header.set_size(1 << 30);
		header.as_mut_bytes()[..13].copy_from_slice(b"././@LongLink");
		header.set_cksum();
		let mut raw = header.as_bytes().to_vec();
		raw.resize(raw.len() + (1 << 30), 0);
		let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
		gz.write_all(&raw).unwrap();
		let (bytes, parsed) = index("");
		assert!(replace(&live, &gz.finish().unwrap(), &bytes, parsed).is_err());
	}

	#[test]
	fn a_revision_too_long_to_record_counts_as_none() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		let (bytes, parsed) = index(&"f".repeat(64));
		let registry = replace(&live, &gotify(), &bytes, parsed).unwrap();
		assert_eq!(registry.templates["gotify"].revision, "");
	}

	#[test]
	fn a_refresh_that_died_between_its_renames_is_put_back_at_start() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.with_extension("old").join("templates")).unwrap();
		recover(&live);
		assert!(live.join("templates").exists());

		// A catalog that is there is never overwritten by a leftover.
		std::fs::create_dir_all(live.with_extension("old").join("stale")).unwrap();
		recover(&live);
		assert!(!live.join("stale").exists());
	}
}
