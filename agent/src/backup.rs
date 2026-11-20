//! Database dump, upload and restore, executed inside database containers.
//!
//! Each entry point sends exactly one terminal event on `tx`. The route drops the future when the
//! client disconnects, so every temp file is owned by a [`TempFile`] guard.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::query_parameters::UploadToContainerOptionsBuilder;
use bytes::Bytes;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use futures_util::{StreamExt, stream};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;

use crate::Ctx;
use proto::{DumpRequest, Event, RestoreRequest, UploadRequest};

// shortcut: single PUT caps at 5 GiB, add multipart upload when users need larger dumps
const MAX_DUMP_BYTES: u64 = 5 << 30;
pub(crate) const RESTORE_CHUNK: usize = 64 * 1024;
const STDERR_TAIL_LINES: usize = 20;
const MAX_STDERR_LINE: usize = 8 * 1024;
const ERROR_BODY_BYTES: usize = 512;
const CONTAINER_RESTORE_DIR: &str = "/tmp";
const CONTAINER_RESTORE_FILE: &str = "podkit-restore";
const CONTAINER_RESTORE_PATH: &str = "/tmp/podkit-restore";

static RESTORE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A number no other restore in this process uses, for temp file and helper names.
pub(crate) fn next_restore_seq() -> u64 {
	RESTORE_SEQ.fetch_add(1, Ordering::Relaxed)
}

pub(crate) type ExecOutput =
	Pin<Box<dyn futures_util::Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;
type ExecInput = Pin<Box<dyn AsyncWrite + Send>>;

/// Dumps the database in `req.container` to `<data_dir>/backups/{backup_id}.gz`.
pub async fn dump(ctx: Arc<Ctx>, req: DumpRequest, tx: mpsc::Sender<Event>) {
	let event = match dump_inner(&ctx, &req, &tx).await {
		Ok(event) => event,
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// PUTs a dump file to a presigned URL, then deletes it.
pub async fn upload(ctx: Arc<Ctx>, req: UploadRequest, tx: mpsc::Sender<Event>) {
	let event = match upload_inner(&ctx, &req).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// Streams a gzip dump from a presigned URL into the restore command of `req.container`.
pub async fn restore(ctx: Arc<Ctx>, req: RestoreRequest, tx: mpsc::Sender<Event>) {
	let event = match restore_inner(&ctx, &req, &tx).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// Deletes its file on drop unless [`TempFile::keep`] was called.
pub(crate) struct TempFile {
	pub(crate) path: PathBuf,
	armed: bool,
}

impl TempFile {
	pub(crate) fn new(path: PathBuf) -> Self {
		Self { path, armed: true }
	}

	pub(crate) fn keep(mut self) -> PathBuf {
		self.armed = false;
		self.path.clone()
	}
}

impl Drop for TempFile {
	fn drop(&mut self) {
		if !self.armed {
			return;
		}
		if let Err(e) = std::fs::remove_file(&self.path)
			&& e.kind() != io::ErrorKind::NotFound
		{
			tracing::warn!(path = %self.path.display(), error = %e, "failed to remove temp file");
		}
	}
}

/// Splits stderr into lines and remembers the last [`STDERR_TAIL_LINES`] of them.
#[derive(Default)]
pub(crate) struct StderrLines {
	partial: Vec<u8>,
	tail: VecDeque<String>,
}

impl StderrLines {
	pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
		let mut lines = Vec::new();
		let mut rest = bytes;
		while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
			self.partial.extend_from_slice(&rest[..pos]);
			lines.extend(self.take_line());
			rest = &rest[pos + 1..];
		}
		self.partial.extend_from_slice(rest);
		// Bounds memory when a tool writes progress output without newlines.
		if self.partial.len() >= MAX_STDERR_LINE {
			lines.extend(self.take_line());
		}
		lines
	}

	pub(crate) fn finish(&mut self) -> Option<String> {
		self.take_line()
	}

	fn take_line(&mut self) -> Option<String> {
		let line = String::from_utf8_lossy(&self.partial)
			.trim_end_matches('\r')
			.to_owned();
		self.partial.clear();
		if line.is_empty() {
			return None;
		}
		if self.tail.len() == STDERR_TAIL_LINES {
			self.tail.pop_front();
		}
		self.tail.push_back(line.clone());
		Some(line)
	}

	fn tail_text(&self) -> String {
		self.tail
			.iter()
			.map(String::as_str)
			.collect::<Vec<_>>()
			.join("\n")
	}
}

pub(crate) fn exit_failure(what: &str, code: i64, stderr: &StderrLines) -> String {
	let tail = stderr.tail_text();
	if tail.is_empty() {
		format!("{what} exited with code {code}")
	} else {
		format!("{what} exited with code {code}: {tail}")
	}
}

pub(crate) fn describe(e: &(dyn std::error::Error + 'static)) -> String {
	let mut out = e.to_string();
	let mut source = e.source();
	while let Some(cause) = source {
		let _ = write!(out, ": {cause}");
		source = cause.source();
	}
	out
}

// Presigned URLs carry credentials in the query string, so they never reach messages.
pub(crate) fn http_error(e: reqwest::Error) -> String {
	describe(&e.without_url())
}

pub(crate) async fn body_prefix(mut resp: reqwest::Response) -> String {
	let mut buf = Vec::new();
	while buf.len() < ERROR_BODY_BYTES {
		match resp.chunk().await {
			Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
			Ok(None) | Err(_) => break,
		}
	}
	buf.truncate(ERROR_BODY_BYTES);
	String::from_utf8_lossy(&buf).into_owned()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
	let mut out = String::with_capacity(bytes.len() * 2);
	for b in bytes {
		let _ = write!(out, "{b:02x}");
	}
	out
}

pub(crate) struct Exec {
	pub(crate) id: String,
	pub(crate) output: ExecOutput,
	pub(crate) input: ExecInput,
}

pub(crate) async fn exec_sh(
	docker: &Docker,
	container: &str,
	script: String,
	attach_stdin: bool,
) -> Result<Exec, String> {
	let created = docker
		.create_exec(
			container,
			CreateExecOptions::<String> {
				attach_stdin: Some(attach_stdin),
				attach_stdout: Some(true),
				attach_stderr: Some(true),
				cmd: Some(vec!["sh".to_owned(), "-c".to_owned(), script]),
				..Default::default()
			},
		)
		.await
		.map_err(|e| format!("create exec in {container}: {e}"))?;
	match docker.start_exec(&created.id, None).await {
		Ok(StartExecResults::Attached { output, input }) => Ok(Exec {
			id: created.id,
			output,
			input,
		}),
		Ok(StartExecResults::Detached) => Err("exec started detached".to_owned()),
		Err(e) => Err(format!("start exec in {container}: {e}")),
	}
}

/// Exit code of a finished exec. Podman can report `running` briefly after the output closes.
pub(crate) async fn exit_code(docker: &Docker, exec_id: &str) -> Result<i64, String> {
	for _ in 0..50 {
		let info = docker
			.inspect_exec(exec_id)
			.await
			.map_err(|e| format!("inspect exec: {e}"))?;
		if info.running != Some(true) {
			return info
				.exit_code
				.ok_or_else(|| "exec exit code unavailable".to_owned());
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	Err("exec still running after its output closed".to_owned())
}

pub(crate) async fn forward_stderr(tx: &mpsc::Sender<Event>, line: String) -> Result<(), String> {
	tx.send(Event::Log {
		stream: proto::Stream::Stderr,
		line,
	})
	.await
	.map_err(|_| "client disconnected".to_owned())
}

/// Consumes exec output, forwarding stderr lines and discarding stdout.
pub(crate) async fn drain_output(
	mut output: ExecOutput,
	tx: &mpsc::Sender<Event>,
	stderr: &mut StderrLines,
) -> Result<(), String> {
	while let Some(item) = output.next().await {
		if let LogOutput::StdErr { message } = item.map_err(|e| format!("exec stream: {e}"))? {
			for line in stderr.push(&message) {
				forward_stderr(tx, line).await?;
			}
		}
	}
	if let Some(line) = stderr.finish() {
		forward_stderr(tx, line).await?;
	}
	Ok(())
}

// --- dump ---

struct DumpSink {
	file: std::fs::File,
	hasher: Sha256,
	bytes: u64,
}

impl Write for DumpSink {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let n = self.file.write(buf)?;
		self.hasher.update(&buf[..n]);
		self.bytes += n as u64;
		if self.bytes > MAX_DUMP_BYTES {
			return Err(io::ErrorKind::FileTooLarge.into());
		}
		Ok(n)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.file.flush()
	}
}

fn write_dump(file: std::fs::File, mut rx: mpsc::Receiver<Bytes>) -> io::Result<(u64, String)> {
	let sink = DumpSink {
		file,
		hasher: Sha256::new(),
		bytes: 0,
	};
	let mut encoder = GzEncoder::new(sink, Compression::default());
	while let Some(chunk) = rx.blocking_recv() {
		encoder.write_all(&chunk)?;
	}
	let mut sink = encoder.finish()?;
	sink.flush()?;
	Ok((sink.bytes, hex(&sink.hasher.finalize())))
}

pub(crate) async fn create_private_file(path: &Path) -> Result<tokio::fs::File, String> {
	tokio::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(true)
		.mode(0o600)
		.open(path)
		.await
		.map_err(|e| format!("create {}: {e}", path.display()))
}

pub(crate) async fn backups_dir(ctx: &Ctx) -> Result<PathBuf, String> {
	let dir = ctx.data_dir.join("backups");
	tokio::fs::create_dir_all(&dir)
		.await
		.map_err(|e| format!("create {}: {e}", dir.display()))?;
	Ok(dir)
}

async fn dump_inner(
	ctx: &Ctx,
	req: &DumpRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<Event, String> {
	let cmd = req
		.engine
		.dump_cmd()
		.ok_or("redis backups are not supported")?;
	let dir = backups_dir(ctx).await?;
	let guard = TempFile::new(dir.join(format!("{}.gz", req.backup_id)));
	// Opened here, not in the blocking task, so dropping this future always leaves nothing to delete late.
	let file = create_private_file(&guard.path).await?.into_std().await;

	let mut exec = exec_sh(&ctx.docker, &req.container, cmd.to_owned(), false).await?;

	let (chunk_tx, chunk_rx) = mpsc::channel::<Bytes>(16);
	let writer = tokio::task::spawn_blocking(move || write_dump(file, chunk_rx));

	let mut stderr = StderrLines::default();
	let mut stream_error = None;
	while let Some(item) = exec.output.next().await {
		match item {
			Ok(LogOutput::StdOut { message } | LogOutput::Console { message }) => {
				// A closed channel means the writer stopped; its result is read below.
				if chunk_tx.send(message).await.is_err() {
					break;
				}
			}
			Ok(LogOutput::StdErr { message }) => {
				for line in stderr.push(&message) {
					forward_stderr(tx, line).await?;
				}
			}
			Ok(LogOutput::StdIn { .. }) => {}
			Err(e) => {
				stream_error = Some(format!("exec stream: {e}"));
				break;
			}
		}
	}
	drop(chunk_tx);

	let written = writer
		.await
		.map_err(|e| format!("dump writer failed: {e}"))?;
	let (size, sha256) = written.map_err(|e| {
		if e.kind() == io::ErrorKind::FileTooLarge {
			format!(
				"dump exceeds the {} GiB limit (compressed)",
				MAX_DUMP_BYTES >> 30
			)
		} else {
			format!("write dump: {e}")
		}
	})?;
	if let Some(message) = stream_error {
		return Err(message);
	}
	if let Some(line) = stderr.finish() {
		forward_stderr(tx, line).await?;
	}
	let code = exit_code(&ctx.docker, &exec.id).await?;
	if code != 0 {
		return Err(exit_failure("dump", code, &stderr));
	}

	let path = guard.keep();
	Ok(Event::DumpWritten {
		path: path.to_string_lossy().into_owned(),
		size,
		sha256,
	})
}

// --- upload ---

async fn upload_inner(ctx: &Ctx, req: &UploadRequest) -> Result<(), String> {
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

	let file = tokio::fs::File::open(&path)
		.await
		.map_err(|e| format!("open {}: {e}", path.display()))?;
	let len = file
		.metadata()
		.await
		.map_err(|e| format!("stat {}: {e}", path.display()))?
		.len();

	// Content-Length is required: S3 rejects chunked PUTs.
	let resp = ctx
		.http
		.put(&req.url)
		.header(reqwest::header::CONTENT_LENGTH, len)
		.body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
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
	Ok(())
}

// --- restore ---

/// Blocking [`Read`] over a channel of byte chunks. Channel close is EOF; an `Err` item is returned once.
pub(crate) struct ChannelReader {
	rx: mpsc::Receiver<io::Result<Bytes>>,
	current: Bytes,
}

impl ChannelReader {
	fn new(rx: mpsc::Receiver<io::Result<Bytes>>) -> Self {
		Self {
			rx,
			current: Bytes::new(),
		}
	}
}

impl Read for ChannelReader {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		if buf.is_empty() {
			return Ok(0);
		}
		while self.current.is_empty() {
			match self.rx.blocking_recv() {
				Some(Ok(chunk)) => self.current = chunk,
				Some(Err(e)) => return Err(e),
				None => return Ok(0),
			}
		}
		let n = buf.len().min(self.current.len());
		buf[..n].copy_from_slice(&self.current[..n]);
		bytes::Buf::advance(&mut self.current, n);
		Ok(n)
	}
}

/// Gunzips into [`RESTORE_CHUNK`]-sized chunks. Errors are also sent down `out` so the
/// consumer never mistakes a truncated stream for a complete one.
pub(crate) fn inflate<R: Read>(reader: R, out: &mpsc::Sender<io::Result<Bytes>>) -> io::Result<()> {
	let mut gz = GzDecoder::new(reader);
	loop {
		let mut buf = vec![0u8; RESTORE_CHUNK];
		let mut filled = 0;
		while filled < buf.len() {
			match gz.read(&mut buf[filled..]) {
				Ok(0) => break,
				Ok(n) => filled += n,
				Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
				Err(e) => {
					let _ = out.blocking_send(Err(io::Error::new(e.kind(), e.to_string())));
					return Err(e);
				}
			}
		}
		if filled == 0 {
			return Ok(());
		}
		let eof = filled < buf.len();
		buf.truncate(filled);
		if out.blocking_send(Ok(Bytes::from(buf))).is_err() || eof {
			return Ok(());
		}
	}
}

/// Returns the decompressed chunk stream and the future that drives download and decompression.
/// Both must be polled together; the future's error is the root cause when the stream ends in `Err`.
fn decode_gzip(
	resp: reqwest::Response,
) -> (
	mpsc::Receiver<io::Result<Bytes>>,
	impl Future<Output = Result<(), String>>,
) {
	let (chunk_tx, chunk_rx) = mpsc::channel(16);
	let (body_tx, body_rx) = mpsc::channel(16);
	let drive = async move {
		let decoder =
			tokio::task::spawn_blocking(move || inflate(ChannelReader::new(body_rx), &chunk_tx));
		let mut body = Box::pin(resp.bytes_stream());
		while let Some(item) = body.next().await {
			let item = item
				.map_err(|e| io::Error::other(format!("download interrupted: {}", http_error(e))));
			let failed = item.is_err();
			if body_tx.send(item).await.is_err() || failed {
				break;
			}
		}
		drop(body_tx);
		decoder
			.await
			.map_err(|e| format!("decompress task failed: {e}"))?
			.map_err(|e| format!("decompress: {e}"))
	};
	(chunk_rx, drive)
}

async fn download(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
	let resp = http
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
	Ok(resp)
}

enum PipeError {
	/// Download, decompression or output reading failed.
	Source(String),
	/// Writing to the exec's stdin failed after `written` bytes.
	Stdin { written: u64, message: String },
}

enum StdinError {
	/// Nothing reached the database; the upload fallback can still run.
	Unavailable(String),
	Failed(String),
}

async fn feed_stdin(
	mut input: ExecInput,
	mut chunks: mpsc::Receiver<io::Result<Bytes>>,
) -> Result<(), PipeError> {
	let mut written = 0u64;
	while let Some(chunk) = chunks.recv().await {
		let chunk = chunk.map_err(|e| PipeError::Source(e.to_string()))?;
		input
			.write_all(&chunk)
			.await
			.map_err(|e| PipeError::Stdin {
				written,
				message: format!("write to exec stdin: {e}"),
			})?;
		written += chunk.len() as u64;
	}
	// The exit code decides success, so a failed half-close is only logged.
	if let Err(e) = input.shutdown().await {
		tracing::debug!(error = %e, "closing exec stdin failed");
	}
	Ok(())
}

async fn restore_stdin(
	ctx: &Ctx,
	req: &RestoreRequest,
	cmd: &str,
	resp: &mut Option<reqwest::Response>,
	tx: &mpsc::Sender<Event>,
) -> Result<(), StdinError> {
	let Exec {
		id: exec_id,
		output,
		input,
	} = exec_sh(&ctx.docker, &req.container, cmd.to_owned(), true)
		.await
		.map_err(StdinError::Unavailable)?;
	let Some(resp) = resp.take() else {
		return Err(StdinError::Failed("download already consumed".to_owned()));
	};

	let (chunks, decode) = decode_gzip(resp);
	let mut stderr = StderrLines::default();
	let result = {
		let pipeline = async {
			let (fed, decoded) = tokio::join!(feed_stdin(input, chunks), decode);
			fed?;
			decoded.map_err(PipeError::Source)
		};
		let drain = async {
			drain_output(output, tx, &mut stderr)
				.await
				.map_err(PipeError::Source)
		};
		tokio::try_join!(pipeline, drain)
	};

	match result {
		Ok(_) => {
			let code = exit_code(&ctx.docker, &exec_id)
				.await
				.map_err(StdinError::Failed)?;
			if code == 0 {
				Ok(())
			} else {
				Err(StdinError::Failed(exit_failure("restore", code, &stderr)))
			}
		}
		Err(PipeError::Source(message)) => Err(StdinError::Failed(message)),
		Err(PipeError::Stdin { written, message }) => {
			// A command that dies on startup also breaks the pipe; its non-zero exit is the real answer.
			let code = exit_code(&ctx.docker, &exec_id).await;
			match code {
				Ok(code) if code != 0 => {
					Err(StdinError::Failed(exit_failure("restore", code, &stderr)))
				}
				_ if written == 0 => Err(StdinError::Unavailable(message)),
				_ => Err(StdinError::Failed(message)),
			}
		}
	}
}

/// Decompresses to a host file, copies it into the container as a tar, and restores from the file.
async fn restore_via_upload(
	ctx: &Ctx,
	req: &RestoreRequest,
	cmd: &str,
	resp: reqwest::Response,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	let dir = backups_dir(ctx).await?;
	let n = RESTORE_SEQ.fetch_add(1, Ordering::Relaxed);
	let guard = TempFile::new(dir.join(format!("{n}.restore")));
	let mut file = create_private_file(&guard.path).await?;

	let (mut chunks, decode) = decode_gzip(resp);
	let path = guard.path.clone();
	let write = async move {
		let mut size = 0u64;
		while let Some(chunk) = chunks.recv().await {
			let chunk = chunk.map_err(|e| e.to_string())?;
			file.write_all(&chunk)
				.await
				.map_err(|e| format!("write {}: {e}", path.display()))?;
			size += chunk.len() as u64;
		}
		file.flush()
			.await
			.map_err(|e| format!("write {}: {e}", path.display()))?;
		Ok::<u64, String>(size)
	};
	let (written, decoded) = tokio::join!(write, decode);
	let size = written?;
	decoded?;

	let mut header = tar::Header::new_gnu();
	header
		.set_path(CONTAINER_RESTORE_FILE)
		.map_err(|e| format!("tar header: {e}"))?;
	header.set_size(size);
	// World-readable: the exec runs as the container's own user, not root.
	header.set_mode(0o644);
	header.set_entry_type(tar::EntryType::Regular);
	header.set_cksum();
	let header_block = Bytes::copy_from_slice(header.as_bytes());
	let trailer = Bytes::from(vec![0u8; (512 - (size % 512) as usize) % 512 + 1024]);

	let source = tokio::fs::File::open(&guard.path)
		.await
		.map_err(|e| format!("open {}: {e}", guard.path.display()))?;
	let body = stream::once(async move { Ok::<_, io::Error>(header_block) })
		.chain(ReaderStream::new(source))
		.chain(stream::once(async move { Ok(trailer) }));
	let options = UploadToContainerOptionsBuilder::default()
		.path(CONTAINER_RESTORE_DIR)
		.build();
	ctx.docker
		.upload_to_container(
			&req.container,
			Some(options),
			bollard::body_try_stream(body),
		)
		.await
		.map_err(|e| format!("copy dump into {}: {e}", req.container))?;
	drop(guard);

	let script = format!(
		"{cmd} < {CONTAINER_RESTORE_PATH}; rc=$?; rm -f {CONTAINER_RESTORE_PATH}; exit $rc"
	);
	let exec = exec_sh(&ctx.docker, &req.container, script, false).await?;
	let mut stderr = StderrLines::default();
	drain_output(exec.output, tx, &mut stderr).await?;
	let code = exit_code(&ctx.docker, &exec.id).await?;
	if code != 0 {
		return Err(exit_failure("restore", code, &stderr));
	}
	Ok(())
}

async fn restore_inner(
	ctx: &Ctx,
	req: &RestoreRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	let cmd = req
		.engine
		.restore_cmd()
		.ok_or("redis restores are not supported")?;
	let mut resp = Some(download(&ctx.http, &req.url).await?);

	match restore_stdin(ctx, req, cmd, &mut resp, tx).await {
		Ok(()) => Ok(()),
		Err(StdinError::Failed(message)) => Err(message),
		Err(StdinError::Unavailable(why)) => {
			tracing::warn!(%why, "exec stdin unavailable, restoring via file upload");
			let _ = tx
				.send(Event::Log {
					stream: proto::Stream::System,
					line: format!("exec stdin unavailable ({why}); restoring via file upload"),
				})
				.await;
			let resp = match resp.take() {
				Some(resp) => resp,
				// The first attempt already consumed part of the body; presigned GETs are repeatable.
				None => download(&ctx.http, &req.url).await?,
			};
			restore_via_upload(ctx, req, cmd, resp, tx).await
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn chunk(data: &[u8]) -> Bytes {
		Bytes::copy_from_slice(data)
	}

	#[test]
	fn channel_reader_spans_chunk_boundaries_and_reports_eof() {
		let (tx, rx) = mpsc::channel(4);
		let sender = std::thread::spawn(move || {
			for part in [&b"he"[..], b"", b"llo wo", b"rld"] {
				tx.blocking_send(Ok(chunk(part))).unwrap();
			}
		});
		let mut reader = ChannelReader::new(rx);
		let mut buf = [0u8; 4];
		let mut sizes = Vec::new();
		let mut out = Vec::new();
		loop {
			let n = reader.read(&mut buf).unwrap();
			sizes.push(n);
			if n == 0 {
				break;
			}
			out.extend_from_slice(&buf[..n]);
		}
		sender.join().unwrap();
		assert_eq!(out, b"hello world");
		assert_eq!(sizes, [2, 4, 2, 3, 0]);
		assert_eq!(reader.read(&mut buf).unwrap(), 0);
	}

	#[test]
	fn channel_reader_read_to_end_over_many_small_chunks() {
		let (tx, rx) = mpsc::channel(2);
		let expected: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
		let data = expected.clone();
		let sender = std::thread::spawn(move || {
			for part in data.chunks(7) {
				tx.blocking_send(Ok(chunk(part))).unwrap();
			}
		});
		let mut out = Vec::new();
		ChannelReader::new(rx).read_to_end(&mut out).unwrap();
		sender.join().unwrap();
		assert_eq!(out, expected);
	}

	#[test]
	fn channel_reader_surfaces_stream_error_after_buffered_data() {
		let (tx, rx) = mpsc::channel(4);
		tx.blocking_send(Ok(chunk(b"ab"))).unwrap();
		tx.blocking_send(Err(io::Error::other("boom"))).unwrap();
		let mut reader = ChannelReader::new(rx);
		let mut buf = [0u8; 8];
		assert_eq!(reader.read(&mut buf).unwrap(), 2);
		let err = reader.read(&mut buf).unwrap_err();
		assert_eq!(err.to_string(), "boom");
	}

	#[test]
	fn inflate_emits_full_chunks_then_remainder() {
		let original: Vec<u8> = (0..=250u8).cycle().take(RESTORE_CHUNK * 2 + 123).collect();
		let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
		encoder.write_all(&original).unwrap();
		let gz = encoder.finish().unwrap();

		let (body_tx, body_rx) = mpsc::channel(4);
		let (out_tx, mut out_rx) = mpsc::channel(4);
		let worker = std::thread::spawn(move || inflate(ChannelReader::new(body_rx), &out_tx));
		for part in gz.chunks(1000) {
			body_tx.blocking_send(Ok(chunk(part))).unwrap();
		}
		drop(body_tx);

		let mut sizes = Vec::new();
		let mut out = Vec::new();
		while let Some(item) = out_rx.blocking_recv() {
			let bytes = item.unwrap();
			sizes.push(bytes.len());
			out.extend_from_slice(&bytes);
		}
		worker.join().unwrap().unwrap();
		assert_eq!(sizes, [RESTORE_CHUNK, RESTORE_CHUNK, 123]);
		assert_eq!(out, original);
	}

	#[test]
	fn inflate_reports_truncated_gzip_downstream() {
		let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
		encoder.write_all(&vec![7u8; 100_000]).unwrap();
		let mut gz = encoder.finish().unwrap();
		gz.truncate(gz.len() / 2);

		let (body_tx, body_rx) = mpsc::channel(4);
		let (out_tx, mut out_rx) = mpsc::channel(8);
		let worker = std::thread::spawn(move || inflate(ChannelReader::new(body_rx), &out_tx));
		body_tx.blocking_send(Ok(chunk(&gz))).unwrap();
		drop(body_tx);

		let mut saw_error = false;
		while let Some(item) = out_rx.blocking_recv() {
			saw_error |= item.is_err();
		}
		assert!(saw_error);
		assert!(worker.join().unwrap().is_err());
	}

	#[test]
	fn stderr_lines_split_across_chunks_and_keep_last_twenty() {
		let mut stderr = StderrLines::default();
		assert_eq!(stderr.push(b"first\nsec"), ["first"]);
		assert_eq!(stderr.push(b"ond\r\n\nthi"), ["second"]);
		assert_eq!(stderr.finish().as_deref(), Some("thi"));

		let mut stderr = StderrLines::default();
		for i in 0..30 {
			stderr.push(format!("line {i}\n").as_bytes());
		}
		let tail = stderr.tail_text();
		let lines: Vec<_> = tail.lines().collect();
		assert_eq!(lines.len(), STDERR_TAIL_LINES);
		assert_eq!(lines[0], "line 10");
		assert_eq!(lines[19], "line 29");
	}

	#[test]
	fn temp_file_guard_deletes_unless_kept() {
		let dir = tempfile::tempdir().unwrap();
		let gone = dir.path().join("gone");
		let kept = dir.path().join("kept");
		std::fs::write(&gone, b"x").unwrap();
		std::fs::write(&kept, b"x").unwrap();
		drop(TempFile::new(gone.clone()));
		let path = TempFile::new(kept.clone()).keep();
		assert!(!gone.exists());
		assert!(kept.exists());
		assert_eq!(path, kept);
	}
}
