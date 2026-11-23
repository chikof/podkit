//! Volume archives: `tar` of a Podman volume to a gzip file, that file as the
//! parts of an S3 multipart upload, and an archive back into a volume.
//!
//! The volume is mounted into a throwaway helper container (read-only for a
//! backup) and read or written through Podman's archive endpoint, so the helper
//! image only has to exist: nothing runs in it except `sleep` and, to empty a
//! volume, `find`. Docker's compat API documents the endpoint:
//! <https://docs.docker.com/reference/api/engine/version/v1.43/#tag/Container/operation/ContainerArchive>
//! (`path` ending in `/.` archives the directory's contents, not the directory).
//!
//! Each entry point sends exactly one terminal event on `tx`. The route drops the
//! future when the client disconnects, so the helper container and every temp
//! file are owned by guards.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::{
	DownloadFromContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use futures_util::{StreamExt, stream};
use proto::{Event, Stream, UploadPartsRequest, VolumeDumpRequest, VolumeRestoreRequest};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;

use crate::Ctx;
use crate::backup::{
	StderrLines, TempFile, backups_dir, body_prefix, create_private_file, drain_output, exec_sh,
	exit_code, exit_failure, hex, http_error, inflate,
};
use crate::model_download::{ensure_image, valid_ref};
use crate::podman::{ContainerSpec, Podman};

/// Small and ships `sleep` and `find`.
pub const IMAGE: &str = "docker.io/library/alpine:3.21";
const MOUNT: &str = "/data";
/// Archive and restore calls run as long as the volume is big.
const DOCKER_TIMEOUT_SECS: u64 = 12 * 3600;
/// A compressed archive larger than this is refused, so a runaway volume cannot
/// fill the host's disk.
const MAX_ARCHIVE_BYTES: u64 = 1 << 40;
const PART_ATTEMPTS: u32 = 3;
const PART_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Archives the volume to `<data_dir>/backups/{backup_id}.tar.gz`.
pub async fn dump(ctx: Arc<Ctx>, req: VolumeDumpRequest, tx: mpsc::Sender<Event>) {
	let event = match dump_inner(&ctx, &req, &tx).await {
		Ok(event) => event,
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// PUTs the archive file to the presigned part urls, then deletes it.
pub async fn upload_parts(ctx: Arc<Ctx>, req: UploadPartsRequest, tx: mpsc::Sender<Event>) {
	let event = match upload_parts_inner(&ctx, &req, &tx).await {
		Ok(etags) => Event::PartsUploaded { etags },
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// Downloads an archive, checks its checksum, then empties the volume and
/// extracts into it.
pub async fn restore(ctx: Arc<Ctx>, req: VolumeRestoreRequest, tx: mpsc::Sender<Event>) {
	let event = match restore_inner(&ctx, &req, &tx).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

// --- tar stream scanner ---

const BLOCK: usize = 512;

/// Counts the entries of a tar stream as it goes by, without keeping any of it.
///
/// Walks the 512-byte headers: the size field says how many data blocks follow.
/// Extension headers (pax `x`/`g`, GNU long names `L`/`K`) belong to the entry
/// after them and are not counted; the zero blocks that end an archive are not
/// entries either.
#[derive(Debug, Default)]
pub struct TarScan {
	header: Vec<u8>,
	skip: u64,
	entries: u64,
	bytes: u64,
}

impl TarScan {
	/// Feeds the next bytes of the stream.
	pub fn feed(&mut self, mut data: &[u8]) {
		self.bytes += data.len() as u64;
		while !data.is_empty() {
			if self.skip > 0 {
				let n = usize::try_from(self.skip).map_or(data.len(), |s| s.min(data.len()));
				self.skip -= n as u64;
				data = &data[n..];
				continue;
			}
			let take = (BLOCK - self.header.len()).min(data.len());
			self.header.extend_from_slice(&data[..take]);
			data = &data[take..];
			if self.header.len() == BLOCK {
				self.block();
				self.header.clear();
			}
		}
	}

	/// Entries seen so far.
	#[must_use]
	pub fn entries(&self) -> u64 {
		self.entries
	}

	/// Bytes fed so far.
	#[must_use]
	pub fn bytes(&self) -> u64 {
		self.bytes
	}

	fn block(&mut self) {
		if self.header.iter().all(|&b| b == 0) {
			return;
		}
		let size = parse_size(&self.header[124..136]);
		let flag = self.header[156];
		// Links and directories carry no data even where the size field is set.
		let has_data = !matches!(flag, b'1'..=b'6');
		if has_data {
			self.skip = size.div_ceil(BLOCK as u64) * BLOCK as u64;
		}
		if !matches!(flag, b'x' | b'g' | b'L' | b'K') {
			self.entries += 1;
		}
	}
}

/// The size field: octal text, or big-endian binary when the top bit is set
/// (GNU, for entries above 8 GiB).
fn parse_size(field: &[u8]) -> u64 {
	if field.first().is_some_and(|b| b & 0x80 != 0) {
		return field[1..]
			.iter()
			.fold(0u64, |acc, &b| acc.wrapping_shl(8) | u64::from(b));
	}
	field
		.iter()
		.skip_while(|&&b| b == b' ')
		.take_while(|&&b| b != 0 && b != b' ')
		.fold(0u64, |acc, &b| {
			acc.wrapping_mul(8) + u64::from(b.wrapping_sub(b'0') & 7)
		})
}

// --- helper container ---

/// Force-removes the helper container when the call ends or is cancelled.
struct Helper {
	podman: Podman,
	name: String,
}

impl Drop for Helper {
	fn drop(&mut self) {
		let (podman, name) = (self.podman.clone(), std::mem::take(&mut self.name));
		tokio::spawn(async move {
			if let Err(e) = podman.remove(&name).await {
				tracing::warn!(container = %name, error = %e, "removing volume helper failed");
			}
		});
	}
}

/// A client for the long archive calls: the shared one gives up after 15 minutes.
fn long_docker(ctx: &Ctx) -> Result<Docker, String> {
	Docker::connect_with_socket(
		&ctx.podman_socket,
		DOCKER_TIMEOUT_SECS,
		bollard::API_DEFAULT_VERSION,
	)
	.map_err(|e| format!("connect to podman: {e}"))
}

async fn start_helper(
	ctx: &Ctx,
	name: &str,
	volume: &str,
	read_only: bool,
	tx: &mpsc::Sender<Event>,
) -> Result<Helper, String> {
	let podman = ctx.podman();
	ensure_image(&podman, IMAGE, tx).await?;
	// A helper left by a crashed agent would hold the name.
	podman
		.remove(name)
		.await
		.map_err(|e| format!("removing stale helper failed: {e}"))?;
	let mode = if read_only { "ro" } else { "rw" };
	podman
		.create_container(ContainerSpec {
			name: name.to_owned(),
			image: IMAGE.to_owned(),
			command: Some(vec!["sleep".to_owned(), "86400".to_owned()]),
			labels: vec![("podkit.volume-helper".to_owned(), volume.to_owned())],
			binds: vec![format!("{volume}:{MOUNT}:{mode}")],
			stop_timeout_s: Some(0),
			..Default::default()
		})
		.await
		.map_err(|e| format!("helper create failed: {e}"))?;
	let guard = Helper {
		podman: podman.clone(),
		name: name.to_owned(),
	};
	podman
		.start(name)
		.await
		.map_err(|e| format!("helper start failed: {e}"))?;
	Ok(guard)
}

async fn log(tx: &mpsc::Sender<Event>, line: String) -> Result<(), String> {
	tx.send(Event::Log {
		stream: Stream::System,
		line,
	})
	.await
	.map_err(|_| "client disconnected".to_owned())
}

fn check_volume(volume: &str) -> Result<(), String> {
	if valid_ref(volume, false) {
		Ok(())
	} else {
		Err(format!("invalid volume name {volume:?}"))
	}
}

// --- dump ---

struct ArchiveSink {
	file: File,
	hasher: Sha256,
	bytes: u64,
}

impl Write for ArchiveSink {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let n = self.file.write(buf)?;
		self.hasher.update(&buf[..n]);
		self.bytes += n as u64;
		if self.bytes > MAX_ARCHIVE_BYTES {
			return Err(io::ErrorKind::FileTooLarge.into());
		}
		Ok(n)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.file.flush()
	}
}

/// What [`write_archive`] measured.
#[derive(Debug, PartialEq, Eq)]
pub struct Written {
	/// Compressed bytes in the file.
	pub size: u64,
	/// Hex SHA-256 of the compressed file.
	pub sha256: String,
	/// Entries in the tar stream.
	pub entries: u64,
	/// Bytes of the tar stream.
	pub raw_bytes: u64,
}

/// Gzips the tar chunks arriving on `rx` into `file`, hashing the compressed
/// bytes and counting the tar entries on the way.
///
/// # Errors
/// Writing the file fails, or the compressed archive outgrows [`MAX_ARCHIVE_BYTES`]
/// ([`io::ErrorKind::FileTooLarge`]).
pub fn write_archive(file: File, mut rx: mpsc::Receiver<Bytes>) -> io::Result<Written> {
	let mut scan = TarScan::default();
	let mut encoder = GzEncoder::new(
		ArchiveSink {
			file,
			hasher: Sha256::new(),
			bytes: 0,
		},
		Compression::default(),
	);
	while let Some(chunk) = rx.blocking_recv() {
		scan.feed(&chunk);
		encoder.write_all(&chunk)?;
	}
	let mut sink = encoder.finish()?;
	sink.flush()?;
	Ok(Written {
		size: sink.bytes,
		sha256: hex(&sink.hasher.finalize()),
		entries: scan.entries(),
		raw_bytes: scan.bytes(),
	})
}

async fn dump_inner(
	ctx: &Ctx,
	req: &VolumeDumpRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<Event, String> {
	check_volume(&req.volume)?;
	let docker = long_docker(ctx)?;
	docker
		.inspect_volume(&req.volume)
		.await
		.map_err(|e| format!("volume {} is not available: {e}", req.volume))?;
	let dir = backups_dir(ctx).await?;
	let guard = TempFile::new(dir.join(format!("{}.tar.gz", req.backup_id)));
	// Opened here, not in the blocking task, so dropping this future always leaves nothing to delete late.
	let file = create_private_file(&guard.path).await?.into_std().await;

	let name = format!("podkit-volbk-{}", req.backup_id);
	let _helper = start_helper(ctx, &name, &req.volume, true, tx).await?;
	log(tx, format!("archiving {} read-only", req.volume)).await?;

	let options = DownloadFromContainerOptionsBuilder::default()
		.path(&format!("{MOUNT}/."))
		.build();
	let mut archive = std::pin::pin!(docker.download_from_container(&name, Some(options)));
	let (chunk_tx, chunk_rx) = mpsc::channel::<Bytes>(16);
	let writer = tokio::task::spawn_blocking(move || write_archive(file, chunk_rx));

	let mut stream_error = None;
	while let Some(item) = archive.next().await {
		match item {
			Ok(chunk) => {
				// A closed channel means the writer stopped; its result is read below.
				if chunk_tx.send(chunk).await.is_err() {
					break;
				}
			}
			Err(e) => {
				stream_error = Some(format!("reading the volume: {e}"));
				break;
			}
		}
	}
	drop(chunk_tx);

	let written = writer
		.await
		.map_err(|e| format!("archive writer failed: {e}"))?
		.map_err(|e| {
			if e.kind() == io::ErrorKind::FileTooLarge {
				format!(
					"archive exceeds the {} GiB limit (compressed)",
					MAX_ARCHIVE_BYTES >> 30
				)
			} else {
				format!("write archive: {e}")
			}
		})?;
	if let Some(message) = stream_error {
		return Err(message);
	}
	let path = guard.keep();
	Ok(Event::VolumeDumped {
		path: path.to_string_lossy().into_owned(),
		size: written.size,
		sha256: written.sha256,
		entries: written.entries,
		raw_bytes: written.raw_bytes,
	})
}

// --- upload ---

/// `(offset, length)` of each part of a `len`-byte file.
#[must_use]
pub fn part_ranges(len: u64, part_size: u64) -> Vec<(u64, u64)> {
	if part_size == 0 {
		return Vec::new();
	}
	let mut out = Vec::new();
	let mut offset = 0;
	while offset < len {
		let n = part_size.min(len - offset);
		out.push((offset, n));
		offset += n;
	}
	if out.is_empty() {
		out.push((0, 0));
	}
	out
}

async fn put_part(
	ctx: &Ctx,
	path: &std::path::Path,
	url: &str,
	offset: u64,
	len: u64,
) -> Result<String, String> {
	use tokio::io::{AsyncReadExt, AsyncSeekExt};
	let mut file = tokio::fs::File::open(path)
		.await
		.map_err(|e| format!("open {}: {e}", path.display()))?;
	file.seek(io::SeekFrom::Start(offset))
		.await
		.map_err(|e| format!("seek {}: {e}", path.display()))?;
	// Content-Length is required: S3 rejects chunked PUTs.
	let resp = ctx
		.http
		.put(url)
		.header(reqwest::header::CONTENT_LENGTH, len)
		.body(reqwest::Body::wrap_stream(ReaderStream::new(
			file.take(len),
		)))
		.send()
		.await
		.map_err(|e| format!("S3 upload failed: {}", http_error(e)))?;
	let status = resp.status();
	if !status.is_success() {
		return Err(format!(
			"S3 upload failed: {status} {}",
			body_prefix(resp).await
		));
	}
	resp.headers()
		.get(reqwest::header::ETAG)
		.and_then(|v| v.to_str().ok())
		.map(str::to_owned)
		.ok_or_else(|| "S3 answered a part without an ETag".to_owned())
}

async fn upload_parts_inner(
	ctx: &Ctx,
	req: &UploadPartsRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<Vec<String>, String> {
	let backups = ctx.data_dir.join("backups");
	let path = PathBuf::from(&req.path);
	if !path.starts_with(&backups) || path.components().any(|c| matches!(c, Component::ParentDir)) {
		return Err(format!(
			"refusing to upload {}: not inside {}",
			path.display(),
			backups.display()
		));
	}
	let _guard = TempFile::new(path.clone());
	let len = tokio::fs::metadata(&path)
		.await
		.map_err(|e| format!("stat {}: {e}", path.display()))?
		.len();
	let ranges = part_ranges(len, req.part_size);
	if ranges.len() != req.urls.len() {
		return Err(format!(
			"{} part urls for {} parts",
			req.urls.len(),
			ranges.len()
		));
	}

	let mut etags = Vec::with_capacity(ranges.len());
	for (i, ((offset, n), url)) in ranges.iter().zip(&req.urls).enumerate() {
		let mut attempt = 1;
		let etag = loop {
			match put_part(ctx, &path, url, *offset, *n).await {
				Ok(etag) => break etag,
				Err(e) if attempt < PART_ATTEMPTS => {
					log(
						tx,
						format!(
							"part {} failed ({e}); retrying ({attempt}/{PART_ATTEMPTS})",
							i + 1
						),
					)
					.await?;
					attempt += 1;
					tokio::time::sleep(PART_RETRY_DELAY).await;
				}
				Err(e) => return Err(format!("part {}: {e}", i + 1)),
			}
		};
		etags.push(etag);
		log(tx, format!("uploaded part {}/{}", i + 1, ranges.len())).await?;
	}
	Ok(etags)
}

// --- restore ---

/// Downloads `url` into `path`, returning the SHA-256 of what arrived.
async fn download_to(ctx: &Ctx, url: &str, path: &std::path::Path) -> Result<String, String> {
	let resp = ctx
		.http
		.get(url)
		.send()
		.await
		.map_err(|e| format!("S3 download failed: {}", http_error(e)))?;
	let status = resp.status();
	if !status.is_success() {
		return Err(format!(
			"S3 download failed: {status} {}",
			body_prefix(resp).await
		));
	}
	let mut file = create_private_file(path).await?;
	let mut hasher = Sha256::new();
	let mut body = Box::pin(resp.bytes_stream());
	while let Some(chunk) = body.next().await {
		let chunk = chunk.map_err(|e| format!("download interrupted: {}", http_error(e)))?;
		hasher.update(&chunk);
		file.write_all(&chunk)
			.await
			.map_err(|e| format!("write {}: {e}", path.display()))?;
	}
	file.flush()
		.await
		.map_err(|e| format!("write {}: {e}", path.display()))?;
	Ok(hex(&hasher.finalize()))
}

async fn restore_inner(
	ctx: &Ctx,
	req: &VolumeRestoreRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	check_volume(&req.volume)?;
	let dir = backups_dir(ctx).await?;
	let seq = crate::backup::next_restore_seq();
	let guard = TempFile::new(dir.join(format!("{seq}.volume-restore")));

	log(tx, "downloading the archive".to_owned()).await?;
	let sha256 = download_to(ctx, &req.url, &guard.path).await?;
	// Nothing in the volume is touched before the archive is known to be whole.
	if !sha256.eq_ignore_ascii_case(&req.sha256) {
		return Err("the downloaded archive does not match its recorded checksum".to_owned());
	}

	let podman = ctx.podman();
	podman
		.ensure_volume(&req.volume)
		.await
		.map_err(|e| format!("volume {} setup failed: {e}", req.volume))?;
	let docker = long_docker(ctx)?;
	let name = format!("podkit-volrs-{seq}");
	let _helper = start_helper(ctx, &name, &req.volume, false, tx).await?;

	if req.wipe {
		log(tx, format!("emptying {}", req.volume)).await?;
		let script = format!("find {MOUNT} -mindepth 1 -delete");
		let exec = exec_sh(&docker, &name, script, false).await?;
		let mut stderr = StderrLines::default();
		drain_output(exec.output, tx, &mut stderr).await?;
		let code = exit_code(&docker, &exec.id).await?;
		if code != 0 {
			return Err(exit_failure("emptying the volume", code, &stderr));
		}
	}

	log(tx, format!("extracting into {}", req.volume)).await?;
	let (chunks, decoder) = inflate_file(&guard.path)?;
	let body = stream::unfold(chunks, |mut rx| async move {
		rx.recv().await.map(|item| (item, rx))
	});
	let options = UploadToContainerOptionsBuilder::default()
		.path(MOUNT)
		.build();
	let uploaded = docker
		.upload_to_container(&name, Some(options), bollard::body_try_stream(body))
		.await;
	// The decoder's error is the root cause when the stream ended in `Err`.
	let inflated = decoder
		.await
		.map_err(|e| format!("decompress task failed: {e}"))?;
	inflated.map_err(|e| format!("decompress: {e}"))?;
	uploaded.map_err(|e| format!("extracting into {}: {e}", req.volume))?;
	Ok(())
}

type Decoded = tokio::task::JoinHandle<io::Result<()>>;

/// Gunzips `path` into [`crate::backup::RESTORE_CHUNK`]-sized chunks.
fn inflate_file(
	path: &std::path::Path,
) -> Result<(mpsc::Receiver<io::Result<Bytes>>, Decoded), String> {
	let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
	let (tx, rx) = mpsc::channel(16);
	let handle = tokio::task::spawn_blocking(move || inflate(BufReader::new(file), &tx));
	Ok((rx, handle))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A tar of a small tree: a directory, a file, a symlink, an empty file and a
	/// file with a name too long for the old header, which makes a GNU `L` entry.
	fn sample_tar() -> (Vec<u8>, u64) {
		let mut builder = tar::Builder::new(Vec::new());
		let mut dir = tar::Header::new_gnu();
		dir.set_entry_type(tar::EntryType::Directory);
		dir.set_mode(0o755);
		dir.set_size(0);
		builder.append_data(&mut dir, "sub/", io::empty()).unwrap();
		let body = vec![7u8; 1300];
		let mut file = tar::Header::new_gnu();
		file.set_size(body.len() as u64);
		file.set_mode(0o644);
		builder
			.append_data(&mut file, "sub/a.bin", body.as_slice())
			.unwrap();
		let mut empty = tar::Header::new_gnu();
		empty.set_size(0);
		empty.set_mode(0o644);
		builder
			.append_data(&mut empty, "sub/empty", io::empty())
			.unwrap();
		let mut link = tar::Header::new_gnu();
		link.set_entry_type(tar::EntryType::Symlink);
		link.set_size(0);
		builder.append_link(&mut link, "sub/link", "a.bin").unwrap();
		let long = format!("sub/{}", "n".repeat(150));
		let mut longfile = tar::Header::new_gnu();
		longfile.set_size(3);
		longfile.set_mode(0o600);
		builder
			.append_data(&mut longfile, &long, &b"abc"[..])
			.unwrap();
		(builder.into_inner().unwrap(), 5)
	}

	#[test]
	fn tar_scan_counts_entries_not_extension_headers_or_padding() {
		let (tar, entries) = sample_tar();
		let mut scan = TarScan::default();
		scan.feed(&tar);
		assert_eq!(scan.entries(), entries);
		assert_eq!(scan.bytes(), tar.len() as u64);
	}

	#[test]
	fn tar_scan_gives_the_same_answer_for_any_chunking() {
		let (tar, entries) = sample_tar();
		for size in [1, 7, 511, 512, 513, 4096] {
			let mut scan = TarScan::default();
			for chunk in tar.chunks(size) {
				scan.feed(chunk);
			}
			assert_eq!(scan.entries(), entries, "chunks of {size}");
		}
	}

	#[test]
	fn tar_scan_of_an_empty_volume_has_no_entries() {
		let tar = tar::Builder::new(Vec::new()).into_inner().unwrap();
		let mut scan = TarScan::default();
		scan.feed(&tar);
		assert_eq!(scan.entries(), 0);
		assert_eq!(scan.bytes(), 1024);
	}

	#[test]
	fn size_field_reads_octal_and_gnu_binary() {
		assert_eq!(parse_size(b"00000001750\0"), 0o1750);
		assert_eq!(parse_size(b"        750 "), 0o750);
		let mut binary = [0u8; 12];
		binary[0] = 0x80;
		binary[9..].copy_from_slice(&[0x01, 0x00, 0x00]);
		assert_eq!(parse_size(&binary), 65536);
	}

	#[test]
	fn archive_is_gzip_of_the_stream_with_a_matching_checksum() {
		let (tar, entries) = sample_tar();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("a.tar.gz");
		let file = File::create(&path).unwrap();
		let (tx, rx) = mpsc::channel(4);
		let writer = std::thread::spawn(move || write_archive(file, rx));
		for chunk in tar.chunks(1000) {
			tx.blocking_send(Bytes::copy_from_slice(chunk)).unwrap();
		}
		drop(tx);
		let written = writer.join().unwrap().unwrap();

		let stored = std::fs::read(&path).unwrap();
		assert_eq!(written.size, stored.len() as u64);
		assert_eq!(written.sha256, hex(&Sha256::digest(&stored)));
		assert_eq!(written.entries, entries);
		assert_eq!(written.raw_bytes, tar.len() as u64);

		let mut back = Vec::new();
		io::copy(&mut flate2::read::GzDecoder::new(&stored[..]), &mut back).unwrap();
		assert_eq!(back, tar);
		// The tar crate reads every entry back, in order.
		let names: Vec<_> = tar::Archive::new(&back[..])
			.entries()
			.unwrap()
			.map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
			.collect();
		assert_eq!(names.len() as u64, entries);
		assert_eq!(names[1], "sub/a.bin");
	}

	#[test]
	fn parts_cover_the_file_and_the_last_takes_the_rest() {
		assert_eq!(part_ranges(10, 4), [(0, 4), (4, 4), (8, 2)]);
		assert_eq!(part_ranges(8, 4), [(0, 4), (4, 4)]);
		assert_eq!(part_ranges(3, 64), [(0, 3)]);
		// An empty file is one empty part, which S3 accepts.
		assert_eq!(part_ranges(0, 64), [(0, 0)]);
		assert!(part_ranges(5, 0).is_empty());
	}
}
