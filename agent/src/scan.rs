//! `POST /v1/scan`: finds known vulnerabilities in an image without reading
//! it twice. Syft turns the image into a `CycloneDX` SBOM and Grype matches the
//! SBOM against its database; both run as throwaway containers with a CPU and
//! memory cap (`docs/spikes/scanning.md`). A rescan skips Syft and feeds the
//! stored SBOM to Grype, so it never reads the image again.
//!
//! Everything a scan creates is removed whatever happens: the work directory
//! (a [`tempfile::TempDir`], dropped on success, failure and cancellation) and
//! the scanner containers (force-removed, and by a drop guard if the request
//! is abandoned). Only the Grype database volume outlives a scan.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bollard::query_parameters::WaitContainerOptionsBuilder;
use futures_util::StreamExt;
use proto::{ScanFinding, ScanRequest, ScanResult};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::podman::{ContainerSpec, Podman, PodmanError};

/// Pinned: a scanner upgrade is a code change, measured first.
/// The Syft image.
pub const SYFT_IMAGE: &str = "docker.io/anchore/syft:v1.57.2";
/// The Grype image.
pub const GRYPE_IMAGE: &str = "docker.io/anchore/grype:v0.122.1";
/// Holds Grype's database (about 3.5 GB unpacked) between scans.
pub const DB_VOLUME: &str = "podkit-scanner-db";
/// The database is refreshed at most this often.
const DB_MAX_AGE: Duration = Duration::from_hours(24);
/// Grype refuses a database older than 5 days by default; a scan with a stale
/// one is still more useful than none, and the result says when it was built.
const DB_MAX_AGE_ENV: &str = "336h";
/// How long one scanner container may run.
const TOOL_TIMEOUT: Duration = Duration::from_mins(20);
/// Largest SBOM or Grype report read back.
const MAX_OUTPUT_BYTES: u64 = 256 << 20;
const NAME_PREFIX: &str = "podkit-scan-";
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Where a scan runs and what it runs; the defaults are the production ones.
#[derive(Debug, Clone)]
pub struct ScanConfig {
	/// Syft image reference.
	pub syft_image: String,
	/// Grype image reference.
	pub grype_image: String,
	/// Volume holding Grype's database.
	pub db_volume: String,
	/// The agent's data dir; scans work below `scan/` in it.
	pub data_dir: PathBuf,
}

impl ScanConfig {
	/// The production settings, working below `data_dir`.
	#[must_use]
	pub fn new(data_dir: &Path) -> Self {
		Self {
			syft_image: SYFT_IMAGE.to_owned(),
			grype_image: GRYPE_IMAGE.to_owned(),
			db_volume: DB_VOLUME.to_owned(),
			data_dir: data_dir.to_path_buf(),
		}
	}

	fn work_root(&self) -> PathBuf {
		self.data_dir.join("scan")
	}

	fn stamp(&self) -> PathBuf {
		self.work_root().join("db-updated")
	}
}

/// What went wrong, in words the person who deployed the image can read.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
	#[error("image {0} not found on this server")]
	/// The image is not on this server.
	ImageMissing(String),
	#[error("invalid scan request: {0}")]
	/// The request is malformed.
	Invalid(String),
	#[error("{0}")]
	/// A scanner or Podman failed.
	Failed(String),
}

impl From<PodmanError> for ScanError {
	fn from(e: PodmanError) -> Self {
		Self::Failed(e.to_string())
	}
}

impl From<std::io::Error> for ScanError {
	fn from(e: std::io::Error) -> Self {
		Self::Failed(format!("scan files: {e}"))
	}
}

/// The content id of `image` on this server.
///
/// # Errors
/// `ImageMissing` when it is not there; `Failed` when Podman is unreachable.
pub async fn resolve(podman: &Podman, image: &str) -> Result<String, ScanError> {
	let inspect = podman
		.inspect_image(image)
		.await?
		.ok_or_else(|| ScanError::ImageMissing(image.to_owned()))?;
	inspect
		.id
		.filter(|id| !id.is_empty())
		.ok_or_else(|| ScanError::Failed("image has no id".to_owned()))
}

/// Runs a scan or a rescan.
///
/// # Errors
/// See [`ScanError`]. A scanner killed for memory says so and names the cap.
pub async fn run(
	podman: &Podman,
	config: &ScanConfig,
	req: &ScanRequest,
) -> Result<ScanResult, ScanError> {
	validate(req)?;
	std::fs::create_dir_all(config.work_root())?;
	let work = tempfile::Builder::new()
		.prefix("scan-")
		.tempdir_in(config.work_root())?;
	let mut guard = ContainerGuard::new(podman.clone());

	let mut image_id = String::new();
	let sbom = if let Some(image) = &req.image {
		image_id = resolve(podman, image).await?;
		export_image(podman, image, &work.path().join("img.tar")).await?;
		ensure_tool_image(podman, &config.syft_image).await?;
		let name = guard.name("syft");
		run_tool(
			podman,
			ContainerSpec {
				name,
				image: config.syft_image.clone(),
				command: Some(vec![
					"docker-archive:/in/img.tar".to_owned(),
					"-o".to_owned(),
					"cyclonedx-json=/in/sbom.json".to_owned(),
					"-q".to_owned(),
				]),
				env: vec![("SYFT_CHECK_FOR_APP_UPDATE".to_owned(), "false".to_owned())],
				binds: vec![format!("{}:/in:Z", work.path().display())],
				..tool_spec(req)
			},
			req.memory_mb,
		)
		.await?;
		let sbom = read_limited(&work.path().join("sbom.json")).await?;
		// The archive is the biggest file; free it before Grype runs.
		let _ = tokio::fs::remove_file(work.path().join("img.tar")).await;
		sbom
	} else {
		let sbom = req.sbom.clone().unwrap_or_default();
		tokio::fs::write(work.path().join("sbom.json"), &sbom).await?;
		sbom
	};

	ensure_tool_image(podman, &config.grype_image).await?;
	podman.ensure_volume(&config.db_volume).await?;
	if db_stale(config) {
		let name = guard.name("db");
		let update = run_tool(
			podman,
			grype_spec(config, req, name, &work, &["db", "update"]),
			req.memory_mb,
		)
		.await;
		match update {
			Ok(()) => {
				let _ = tokio::fs::write(config.stamp(), unix_now().to_string()).await;
			}
			// A database that is a day old is fine; none at all fails the scan below.
			Err(e) => {
				tracing::warn!(error = %e, "grype db update failed, scanning with the current database");
			}
		}
	}
	let name = guard.name("grype");
	run_tool(
		podman,
		grype_spec(
			config,
			req,
			name,
			&work,
			&[
				"sbom:/in/sbom.json",
				"-o",
				"json",
				"--file",
				"/in/grype.json",
				"-q",
			],
		),
		req.memory_mb,
	)
	.await?;
	let report = read_limited(&work.path().join("grype.json")).await?;
	let (findings, db_built) = parse_grype(&report).map_err(ScanError::Failed)?;

	guard.disarm();
	Ok(ScanResult {
		image_id,
		sbom: req.image.as_ref().map(|_| sbom),
		findings,
		db_built,
		tools: tools_label(config),
	})
}

fn validate(req: &ScanRequest) -> Result<(), ScanError> {
	match (&req.image, &req.sbom) {
		(Some(image), None) if !image.trim().is_empty() && !image.starts_with('-') => {}
		(None, Some(sbom)) if sbom.trim_start().starts_with('{') => {}
		_ => {
			return Err(ScanError::Invalid(
				"give either an image or an SBOM".to_owned(),
			));
		}
	}
	if !(0.1..=16.0).contains(&req.cpus) {
		return Err(ScanError::Invalid("cpus must be 0.1 to 16".to_owned()));
	}
	if !(256..=65_536).contains(&req.memory_mb) {
		return Err(ScanError::Invalid(
			"memory must be 256 to 65536 MiB".to_owned(),
		));
	}
	Ok(())
}

fn tool_spec(req: &ScanRequest) -> ContainerSpec {
	ContainerSpec {
		labels: vec![("podkit.scan".to_owned(), "1".to_owned())],
		memory_bytes: Some(i64::from(req.memory_mb) << 20),
		cpu_cores: Some(req.cpus),
		options: proto::ContainerOptions {
			// Root inside a rootless container is the agent's own user, which is what
			// lets the tools write the bind-mounted work dir.
			user: Some("0".to_owned()),
			..Default::default()
		},
		..Default::default()
	}
}

fn grype_spec(
	config: &ScanConfig,
	req: &ScanRequest,
	name: String,
	work: &tempfile::TempDir,
	args: &[&str],
) -> ContainerSpec {
	let env = vec![
		("GRYPE_DB_CACHE_DIR".to_owned(), "/db".to_owned()),
		("GRYPE_DB_AUTO_UPDATE".to_owned(), "false".to_owned()),
		(
			"GRYPE_DB_MAX_ALLOWED_BUILT_AGE".to_owned(),
			DB_MAX_AGE_ENV.to_owned(),
		),
		("GRYPE_CHECK_FOR_APP_UPDATE".to_owned(), "false".to_owned()),
	];
	ContainerSpec {
		name,
		image: config.grype_image.clone(),
		command: Some(args.iter().map(|a| (*a).to_owned()).collect()),
		env,
		binds: vec![
			format!("{}:/in:Z", work.path().display()),
			format!("{}:/db", config.db_volume),
		],
		..tool_spec(req)
	}
}

fn tools_label(config: &ScanConfig) -> String {
	let version = |image: &str| {
		image
			.rsplit(':')
			.next()
			.unwrap_or("")
			.trim_start_matches('v')
			.to_owned()
	};
	format!(
		"syft@{} grype@{}",
		version(&config.syft_image),
		version(&config.grype_image)
	)
}

fn unix_now() -> u64 {
	SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.map_or(0, |d| d.as_secs())
}

/// Whether the database was last refreshed more than a day ago (or never).
fn db_stale(config: &ScanConfig) -> bool {
	let updated = std::fs::read_to_string(config.stamp())
		.ok()
		.and_then(|s| s.trim().parse::<u64>().ok());
	updated.is_none_or(|at| unix_now().saturating_sub(at) >= DB_MAX_AGE.as_secs())
}

async fn ensure_tool_image(podman: &Podman, image: &str) -> Result<(), ScanError> {
	if podman.inspect_image(image).await?.is_some() {
		return Ok(());
	}
	let mut pull = std::pin::pin!(podman.pull_image(image));
	while let Some(item) = pull.next().await {
		item.map_err(|e| ScanError::Failed(format!("pulling {image} failed: {e}")))?;
	}
	Ok(())
}

/// Streams `podman save` of one image into `path`.
async fn export_image(podman: &Podman, image: &str, path: &Path) -> Result<(), ScanError> {
	let mut file = tokio::fs::File::create(path).await?;
	let mut stream = std::pin::pin!(podman.docker().export_image(image));
	while let Some(chunk) = stream.next().await {
		let chunk = chunk.map_err(|e| ScanError::Failed(format!("exporting the image: {e}")))?;
		file.write_all(&chunk).await?;
	}
	file.flush().await?;
	Ok(())
}

async fn read_limited(path: &Path) -> Result<String, ScanError> {
	let meta = tokio::fs::metadata(path)
		.await
		.map_err(|_| ScanError::Failed("the scanner wrote no output".to_owned()))?;
	if meta.len() > MAX_OUTPUT_BYTES {
		return Err(ScanError::Failed(
			"the scanner output is too large".to_owned(),
		));
	}
	Ok(tokio::fs::read_to_string(path).await?)
}

/// Creates, runs and removes one scanner container.
async fn run_tool(podman: &Podman, spec: ContainerSpec, memory_mb: u32) -> Result<(), ScanError> {
	let name = spec.name.clone();
	let result = tokio::time::timeout(TOOL_TIMEOUT, execute(podman, spec, memory_mb))
		.await
		.unwrap_or_else(|_| Err(ScanError::Failed("the scanner took too long".to_owned())));
	if let Err(e) = podman.remove(&name).await {
		tracing::warn!(container = %name, error = %e, "removing scanner container failed");
	}
	result
}

async fn execute(podman: &Podman, spec: ContainerSpec, memory_mb: u32) -> Result<(), ScanError> {
	let name = spec.name.clone();
	podman.create_container(spec).await?;
	podman.start(&name).await?;
	let options = WaitContainerOptionsBuilder::default()
		.condition("not-running")
		.build();
	let code = match podman
		.docker()
		.wait_container(&name, Some(options))
		.next()
		.await
	{
		Some(Ok(done)) => done.status_code,
		Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
		Some(Err(e)) => return Err(PodmanError::from(e).into()),
		None => 0,
	};
	if code == 0 {
		return Ok(());
	}
	let oom = podman
		.inspect(&name)
		.await?
		.and_then(|i| i.state)
		.and_then(|s| s.oom_killed)
		.unwrap_or(false);
	if oom || code == 137 {
		return Err(ScanError::Failed(format!(
			"the scanner ran out of memory at {memory_mb} MiB; raise the scan memory limit in the server settings"
		)));
	}
	let tail = log_tail(podman, &name).await;
	Err(ScanError::Failed(format!(
		"the scanner exited with {code}: {tail}"
	)))
}

async fn log_tail(podman: &Podman, name: &str) -> String {
	let mut text = String::new();
	let mut logs = std::pin::pin!(podman.logs(name, Some(8), false));
	while let Some(Ok(chunk)) = logs.next().await {
		text.push_str(&String::from_utf8_lossy(chunk.as_ref()));
	}
	let text = text.trim();
	let cut = text.len().saturating_sub(400);
	let start = (cut..=text.len())
		.find(|i| text.is_char_boundary(*i))
		.unwrap_or(text.len());
	text[start..].to_owned()
}

/// Removes the scanner containers of a request that was abandoned half way
/// (the control plane went away, so the handler future was dropped).
struct ContainerGuard {
	podman: Podman,
	names: Vec<String>,
	armed: bool,
}

impl ContainerGuard {
	fn new(podman: Podman) -> Self {
		Self {
			podman,
			names: Vec::new(),
			armed: true,
		}
	}

	fn name(&mut self, role: &str) -> String {
		let name = format!(
			"{NAME_PREFIX}{role}-{}-{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		);
		self.names.push(name.clone());
		name
	}

	fn disarm(&mut self) {
		self.armed = false;
	}
}

impl Drop for ContainerGuard {
	fn drop(&mut self) {
		if !self.armed {
			return;
		}
		let (podman, names) = (self.podman.clone(), std::mem::take(&mut self.names));
		if let Ok(handle) = tokio::runtime::Handle::try_current() {
			handle.spawn(async move {
				for name in names {
					let _ = podman.remove(&name).await;
				}
			});
		}
	}
}

/// Removes scanner containers and work directories an earlier agent process
/// left behind. Run once at start-up, before requests are served.
pub async fn sweep(podman: &Podman, data_dir: &Path) {
	match podman.list(NAME_PREFIX).await {
		Ok(containers) => {
			for c in containers {
				if let Some(name) = crate::podman::summary_name(&c) {
					let _ = podman.remove(name).await;
				}
			}
		}
		Err(e) => tracing::warn!(error = %e, "listing scanner containers failed"),
	}
	let Ok(entries) = std::fs::read_dir(data_dir.join("scan")) else {
		return;
	};
	for entry in entries.flatten() {
		if entry.file_name().to_string_lossy().starts_with("scan-") {
			let _ = std::fs::remove_dir_all(entry.path());
		}
	}
}

// --- reading Grype's report -----------------------------------------------------------------

#[derive(Deserialize)]
struct Report {
	#[serde(default)]
	matches: Vec<Match>,
	#[serde(default)]
	descriptor: Descriptor,
}

#[derive(Deserialize, Default)]
struct Descriptor {
	#[serde(default)]
	db: DbInfo,
}

#[derive(Deserialize, Default)]
struct DbInfo {
	#[serde(default)]
	status: DbStatus,
}

#[derive(Deserialize, Default)]
struct DbStatus {
	#[serde(default)]
	built: Option<String>,
}

#[derive(Deserialize)]
struct Match {
	vulnerability: Vulnerability,
	#[serde(default, rename = "relatedVulnerabilities")]
	related: Vec<Vulnerability>,
	artifact: Artifact,
}

#[derive(Deserialize)]
struct Artifact {
	name: String,
	#[serde(default)]
	version: String,
}

#[derive(Deserialize, Default)]
struct Vulnerability {
	id: String,
	#[serde(default, rename = "dataSource")]
	data_source: String,
	#[serde(default)]
	severity: String,
	#[serde(default)]
	urls: Vec<String>,
	#[serde(default)]
	cvss: Vec<Cvss>,
	#[serde(default)]
	epss: Vec<Epss>,
	#[serde(default, rename = "knownExploited")]
	known_exploited: Vec<serde_json::Value>,
	#[serde(default)]
	fix: Fix,
}

#[derive(Deserialize)]
struct Cvss {
	#[serde(default)]
	metrics: CvssMetrics,
}

#[derive(Deserialize, Default)]
struct CvssMetrics {
	#[serde(default, rename = "baseScore")]
	base_score: Option<f64>,
}

#[derive(Deserialize)]
struct Epss {
	#[serde(default)]
	epss: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Fix {
	#[serde(default)]
	versions: Vec<String>,
}

fn severity(raw: &str) -> &'static str {
	match raw.to_ascii_lowercase().as_str() {
		"critical" => "critical",
		"high" => "high",
		"medium" => "medium",
		"low" | "negligible" => "low",
		_ => "unknown",
	}
}

fn rank(severity: &str) -> u8 {
	match severity {
		"critical" => 4,
		"high" => 3,
		"medium" => 2,
		"low" => 1,
		_ => 0,
	}
}

fn max_of(values: impl Iterator<Item = f64>) -> Option<f64> {
	values.fold(None, |best, v| Some(best.map_or(v, |b: f64| b.max(v))))
}

/// Reduces Grype's JSON report to findings and the database build time. The
/// same vulnerability found in one package twice (two locations) is one
/// finding; CVSS, EPSS and KEV come from the match or its related CVE.
///
/// # Errors
/// The text is not a Grype report.
pub fn parse_grype(json: &str) -> Result<(Vec<ScanFinding>, Option<String>), String> {
	let report: Report =
		serde_json::from_str(json).map_err(|e| format!("cannot read the scanner report: {e}"))?;
	let mut findings: Vec<ScanFinding> = Vec::new();
	for m in &report.matches {
		let all = || std::iter::once(&m.vulnerability).chain(&m.related);
		let finding = ScanFinding {
			vuln_id: m.vulnerability.id.clone(),
			package: m.artifact.name.clone(),
			version: m.artifact.version.clone(),
			fixed_version: (!m.vulnerability.fix.versions.is_empty())
				.then(|| m.vulnerability.fix.versions.join(", ")),
			severity: severity(&m.vulnerability.severity).to_owned(),
			cvss: max_of(all().flat_map(|v| v.cvss.iter().filter_map(|c| c.metrics.base_score))),
			epss: max_of(all().flat_map(|v| v.epss.iter().filter_map(|e| e.epss))),
			kev: all().any(|v| !v.known_exploited.is_empty()),
			url: Some(m.vulnerability.data_source.clone())
				.filter(|u| !u.is_empty())
				.or_else(|| m.vulnerability.urls.first().cloned()),
		};
		match findings.iter_mut().find(|f| {
			f.vuln_id == finding.vuln_id
				&& f.package == finding.package
				&& f.version == finding.version
		}) {
			Some(existing) => {
				existing.kev |= finding.kev;
				existing.cvss = max_of(existing.cvss.into_iter().chain(finding.cvss));
				existing.epss = max_of(existing.epss.into_iter().chain(finding.epss));
				if existing.fixed_version.is_none() {
					existing.fixed_version = finding.fixed_version;
				}
			}
			None => findings.push(finding),
		}
	}
	findings.sort_by(|a, b| {
		rank(&b.severity)
			.cmp(&rank(&a.severity))
			.then_with(|| a.package.cmp(&b.package))
			.then_with(|| a.vuln_id.cmp(&b.vuln_id))
	});
	Ok((findings, report.descriptor.db.status.built))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Cut from a real Grype 0.122.1 report of `alpine:3.14` (db v6.1.10), plus a KEV
	/// match and a Go advisory with its CVE under `relatedVulnerabilities`.
	pub(crate) const REPORT: &str = include_str!("../tests/fixtures/grype-report.json");

	#[test]
	fn the_report_reduces_to_findings() {
		let (findings, built) = parse_grype(REPORT).unwrap();
		assert_eq!(built.as_deref(), Some("2026-11-19T18:41:05Z"));
		assert_eq!(findings.len(), 5, "{findings:#?}");

		let openssl = findings
			.iter()
			.find(|f| f.vuln_id == "CVE-2023-2650")
			.unwrap();
		assert_eq!(openssl.package, "libcrypto1.1");
		assert_eq!(openssl.version, "1.1.1t-r2");
		assert_eq!(openssl.severity, "medium");
		assert_eq!(openssl.cvss, Some(6.5));
		assert_eq!(openssl.epss, Some(0.75358));
		assert!(!openssl.kev);
		assert_eq!(openssl.fixed_version, None);
		assert_eq!(
			openssl.url.as_deref(),
			Some("https://nvd.nist.gov/vuln/detail/CVE-2023-2650")
		);
	}

	#[test]
	fn fixes_kev_and_related_scores_are_read() {
		let (findings, _) = parse_grype(REPORT).unwrap();
		let kev = findings
			.iter()
			.find(|f| f.vuln_id == "CVE-2022-0001")
			.unwrap();
		assert!(kev.kev);
		assert_eq!(kev.severity, "critical");
		assert_eq!(kev.fixed_version.as_deref(), Some("1.2.3, 1.3.0"));

		// The advisory id carries no scores of its own; the related CVE does.
		let go = findings
			.iter()
			.find(|f| f.vuln_id == "GHSA-xxxx-yyyy-zzzz")
			.unwrap();
		assert_eq!(go.cvss, Some(7.5));
		assert_eq!(go.epss, Some(0.2));
		assert_eq!(go.severity, "high");
	}

	#[test]
	fn severities_map_and_the_worst_come_first() {
		let (findings, _) = parse_grype(REPORT).unwrap();
		let order: Vec<&str> = findings.iter().map(|f| f.severity.as_str()).collect();
		assert_eq!(order, ["critical", "high", "medium", "low", "unknown"]);
		assert_eq!(severity("Negligible"), "low");
		assert_eq!(severity(""), "unknown");
	}

	#[test]
	fn a_vulnerability_found_twice_in_one_package_is_one_finding() {
		let (findings, _) = parse_grype(REPORT).unwrap();
		let twice = findings
			.iter()
			.filter(|f| f.vuln_id == "CVE-2022-0001")
			.count();
		assert_eq!(twice, 1);
	}

	#[test]
	fn garbage_is_an_error_not_a_clean_scan() {
		assert!(parse_grype("not json").is_err());
		assert!(parse_grype("{\"matches\": 3}").is_err());
		assert_eq!(parse_grype("{}").unwrap().0.len(), 0);
	}

	#[test]
	fn requests_name_one_input_and_sane_caps() {
		let ok = ScanRequest {
			image: Some("alpine:3".to_owned()),
			sbom: None,
			cpus: 1.0,
			memory_mb: 1024,
		};
		assert!(validate(&ok).is_ok());
		for bad in [
			ScanRequest {
				image: None,
				..ok.clone()
			},
			ScanRequest {
				sbom: Some("{}".to_owned()),
				..ok.clone()
			},
			ScanRequest {
				image: Some("--help".to_owned()),
				..ok.clone()
			},
			ScanRequest {
				image: None,
				sbom: Some("nope".to_owned()),
				..ok.clone()
			},
			ScanRequest {
				cpus: 0.0,
				..ok.clone()
			},
			ScanRequest {
				memory_mb: 16,
				..ok.clone()
			},
		] {
			assert!(validate(&bad).is_err(), "{bad:?}");
		}
		let rescan = ScanRequest {
			image: None,
			sbom: Some("{\"bomFormat\":\"CycloneDX\"}".to_owned()),
			..ok
		};
		assert!(validate(&rescan).is_ok());
	}

	#[test]
	fn the_database_is_refreshed_at_most_daily() {
		let dir = tempfile::tempdir().unwrap();
		let config = ScanConfig::new(dir.path());
		std::fs::create_dir_all(config.work_root()).unwrap();
		assert!(db_stale(&config), "no stamp means never updated");
		std::fs::write(config.stamp(), unix_now().to_string()).unwrap();
		assert!(!db_stale(&config));
		std::fs::write(config.stamp(), (unix_now() - 25 * 3600).to_string()).unwrap();
		assert!(db_stale(&config));
		assert_eq!(tools_label(&config), "syft@1.57.2 grype@0.122.1");
	}

	/// Nothing a scan makes may outlive it.
	async fn assert_clean(podman: &Podman, config: &ScanConfig) {
		let containers = podman.list(NAME_PREFIX).await.unwrap();
		assert!(containers.is_empty(), "scanner containers left behind");
		let left: Vec<_> = std::fs::read_dir(config.work_root())
			.unwrap()
			.flatten()
			.map(|e| e.file_name().to_string_lossy().into_owned())
			.filter(|n| n.starts_with("scan-"))
			.collect();
		assert!(left.is_empty(), "work dirs left behind: {left:?}");
	}

	/// Needs rootless Podman and the network: scans an old Alpine, rescans its SBOM,
	/// and checks that no container or file is left. `PODKIT_SCAN_LIVE_DB_VOLUME`
	/// names a Grype database volume to reuse (saves the 200 MB download).
	#[tokio::test]
	#[ignore = "needs Podman and the network"]
	async fn scan_live_finds_vulnerabilities_and_cleans_up() {
		const IMAGE: &str = "docker.io/library/alpine:3.14";
		let podman = Podman::connect(&crate::podman::local_socket_path()).unwrap();
		let data = tempfile::tempdir().unwrap();
		let mut config = ScanConfig::new(data.path());
		let own_volume = std::env::var("PODKIT_SCAN_LIVE_DB_VOLUME").is_err();
		config.db_volume = std::env::var("PODKIT_SCAN_LIVE_DB_VOLUME")
			.unwrap_or_else(|_| "podkit-scanner-db-livetest".to_owned());
		ensure_tool_image(&podman, IMAGE).await.unwrap();

		// A scan of an image that is not there fails cleanly.
		let missing = ScanRequest {
			image: Some("localhost/podkit-no-such-image:0".to_owned()),
			sbom: None,
			cpus: 1.0,
			memory_mb: 1024,
		};
		assert!(matches!(
			run(&podman, &config, &missing).await,
			Err(ScanError::ImageMissing(_))
		));
		assert_clean(&podman, &config).await;

		let request = ScanRequest {
			image: Some(IMAGE.to_owned()),
			sbom: None,
			cpus: 1.0,
			memory_mb: 1024,
		};
		let scan = run(&podman, &config, &request).await.unwrap();
		assert!(scan.image_id.starts_with("sha256:"), "{}", scan.image_id);
		let sbom = scan.sbom.clone().expect("the scan returns its SBOM");
		assert!(sbom.contains("\"bomFormat\""), "not CycloneDX");
		assert!(
			scan.findings
				.iter()
				.any(|f| f.severity == "high" || f.severity == "critical"),
			"alpine 3.14 must have a high or critical finding: {} findings",
			scan.findings.len()
		);
		assert!(scan.db_built.is_some());
		assert_clean(&podman, &config).await;

		// The rescan never reads the image: it works from the SBOM alone.
		let rescan = run(
			&podman,
			&config,
			&ScanRequest {
				image: None,
				sbom: Some(sbom),
				..request
			},
		)
		.await
		.unwrap();
		assert!(rescan.sbom.is_none());
		assert_eq!(rescan.findings.len(), scan.findings.len());
		assert_clean(&podman, &config).await;

		if own_volume {
			let _ = podman.remove_volume(&config.db_volume).await;
		}
	}
}
