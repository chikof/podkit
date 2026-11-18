//! `GET /v1/stats`: one live sample of the host and of every podkit container.
//!
//! Every source is parsed by a pure function over a string (or a bollard
//! response), so the tests run on fixtures. Collection never fails: a missing
//! tool or file just leaves that figure empty. CPU figures are deltas, so the
//! previous sample is kept in memory; the first one after a start waits a
//! moment and reads twice.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use bollard::models::{ContainerStatsResponse, ContainerSummaryStateEnum};
use futures_util::StreamExt;
use proto::{ContainerStats, DiskStats, HostStats, ResourceRef, Stats};

use crate::hardware::{NVIDIA_SMI, run};
use crate::podman::{Podman, summary_name};

const SERVICE_LABEL: &str = "podkit.service";
const CONTAINER_PREFIX: &str = "podkit-";
const FIRST_SAMPLE_WAIT: Duration = Duration::from_millis(250);
const STATS_TIMEOUT: Duration = Duration::from_secs(5);
const CONCURRENT_CONTAINERS: usize = 8;
const NVIDIA_ARGS: [&str; 2] = [
	"--query-gpu=utilization.gpu,memory.used",
	"--format=csv,noheader,nounits",
];

/// Cumulative CPU time of the host from `/proc/stat`, in jiffies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
	/// Time not spent idle.
	pub busy: u64,
	/// All time.
	pub total: u64,
}

/// Cumulative CPU time of one container and of the whole host, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupCpu {
	/// Time the container used.
	pub container: u64,
	/// Time all CPUs of the host spent, as the engine reports it.
	pub system: u64,
}

static PREV_HOST_CPU: Mutex<Option<CpuTimes>> = Mutex::new(None);
static PREV_CONTAINER_CPU: Mutex<Vec<(String, CgroupCpu)>> = Mutex::new(Vec::new());

/// Gathers a sample. `graph_root` is Podman's storage dir when known;
/// `data_dir` is the agent's.
pub async fn collect(graph_root: Option<&str>, data_dir: &Path, podman: &Podman) -> Stats {
	let (host, containers) = tokio::join!(host(graph_root, data_dir), containers(podman));
	Stats { host, containers }
}

async fn host(graph_root: Option<&str>, data_dir: &Path) -> HostStats {
	let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
	let (mem_used, mem_total) = parse_meminfo(&meminfo);
	let net = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
	let (net_rx, net_tx) = parse_net_dev(&net);

	let mut paths: Vec<PathBuf> = Vec::new();
	if let Some(root) = graph_root.filter(|r| !r.is_empty()) {
		paths.push(PathBuf::from(root));
	}
	paths.push(data_dir.to_path_buf());
	paths.dedup();
	let mut disks = Vec::new();
	for path in paths {
		if let Some(disk) = disk_stats(&path).await {
			disks.push(disk);
		}
	}

	let gpu = nvidia_usage().await;
	HostStats {
		cpu_percent: host_cpu().await,
		mem_used,
		mem_total,
		disks,
		net_rx,
		net_tx,
		gpu_util: gpu.map(|g| g.0),
		vram_used: gpu.map(|g| g.1),
	}
}

fn read_cpu_times() -> Option<CpuTimes> {
	parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
}

/// Busy share since the previous call. The first call after a start has no
/// previous sample, so it takes a second reading a moment later.
async fn host_cpu() -> Option<f64> {
	let now = read_cpu_times()?;
	let prev = PREV_HOST_CPU
		.lock()
		.map_or(None, |mut prev| prev.replace(now));
	if let Some(prev) = prev {
		return cpu_percent(prev, now);
	}
	tokio::time::sleep(FIRST_SAMPLE_WAIT).await;
	let later = read_cpu_times()?;
	if let Ok(mut prev) = PREV_HOST_CPU.lock() {
		*prev = Some(later);
	}
	cpu_percent(now, later)
}

async fn disk_stats(path: &Path) -> Option<DiskStats> {
	let out = run("df", &["-Pk", &path.to_string_lossy()]).await?;
	let (used, free, total) = parse_df(&out)?;
	Some(DiskStats {
		path: path.to_string_lossy().into_owned(),
		used,
		free,
		total,
	})
}

async fn nvidia_usage() -> Option<(f64, u64)> {
	for program in NVIDIA_SMI {
		if let Some(out) = run(program, &NVIDIA_ARGS).await {
			return parse_nvidia_usage(&out);
		}
	}
	None
}

async fn containers(podman: &Podman) -> Vec<ContainerStats> {
	let Ok(summaries) = podman.list(CONTAINER_PREFIX).await else {
		return Vec::new();
	};
	let targets: Vec<(String, String, ResourceRef)> = summaries
		.iter()
		.filter(|s| s.state == Some(ContainerSummaryStateEnum::RUNNING))
		.filter_map(|s| {
			let id = s.id.clone()?;
			let name = summary_name(s)?.to_string();
			let service = s.labels.as_ref()?.get(SERVICE_LABEL)?;
			Some((id, name, resource_of(service)?))
		})
		.collect();

	let samples: Vec<(String, String, ResourceRef, ContainerStatsResponse)> =
		futures_util::stream::iter(targets)
			.map(|(id, name, resource)| async move {
				let sample = tokio::time::timeout(STATS_TIMEOUT, podman.stats_once(&id))
					.await
					.ok()?
					.ok()??;
				Some((id, name, resource, sample))
			})
			.buffer_unordered(CONCURRENT_CONTAINERS)
			.filter_map(std::future::ready)
			.collect()
			.await;

	let mut prev = PREV_CONTAINER_CPU
		.lock()
		.map(|mut p| std::mem::take(&mut *p))
		.unwrap_or_default();
	let mut current = Vec::with_capacity(samples.len());
	let mut out = Vec::with_capacity(samples.len());
	for (id, name, resource, sample) in samples {
		let cpu = cgroup_cpu(&sample);
		let before = prev
			.iter()
			.find(|(k, _)| *k == id)
			.map(|(_, v)| *v)
			.or_else(|| precpu(&sample));
		let cpu_percent = cpu
			.zip(before)
			.and_then(|(now, before)| container_cpu_percent(before, now));
		if let Some(cpu) = cpu {
			current.push((id, cpu));
		}
		out.push(ContainerStats {
			name,
			resource,
			cpu_percent,
			mem_used: container_mem(&sample),
		});
	}
	prev.clear();
	if let Ok(mut p) = PREV_CONTAINER_CPU.lock() {
		*p = current;
	}
	out.sort_by(|a, b| a.name.cmp(&b.name));
	out
}

/// Busy and total jiffies from the aggregate `cpu ` line of `/proc/stat`.
#[must_use]
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
	let line = text.lines().find(|l| l.starts_with("cpu "))?;
	let fields: Vec<u64> = line
		.split_whitespace()
		.skip(1)
		.map_while(|f| f.parse().ok())
		.collect();
	// user nice system idle iowait irq softirq steal; guest time is already in user.
	if fields.len() < 4 {
		return None;
	}
	let total: u64 = fields.iter().take(8).sum();
	let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
	Some(CpuTimes {
		busy: total.saturating_sub(idle),
		total,
	})
}

/// Busy share between two readings, 0 to 100. `None` when no time passed or
/// the counters went backwards.
#[must_use]
pub fn cpu_percent(prev: CpuTimes, now: CpuTimes) -> Option<f64> {
	let total = now.total.checked_sub(prev.total).filter(|t| *t > 0)?;
	let busy = now.busy.checked_sub(prev.busy)?;
	#[allow(clippy::cast_precision_loss)]
	Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

/// Used and total bytes of `/proc/meminfo`; used is total minus available
/// (or free, before `MemAvailable` existed).
#[must_use]
pub fn parse_meminfo(text: &str) -> (u64, u64) {
	let kb = |key: &str| -> Option<u64> {
		text.lines()
			.find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
			.and_then(|rest| rest.split_whitespace().next()?.parse().ok())
	};
	let total = kb("MemTotal").unwrap_or(0) * 1024;
	let available = kb("MemAvailable").or_else(|| kb("MemFree")).unwrap_or(0) * 1024;
	(total.saturating_sub(available), total)
}

/// Received and sent bytes summed over the physical interfaces of
/// `/proc/net/dev`. Loopback, virtual ethernet pairs and bridges are left out:
/// their traffic is also counted on the interface it leaves through.
#[must_use]
pub fn parse_net_dev(text: &str) -> (u64, u64) {
	const VIRTUAL: [&str; 7] = ["lo", "veth", "podman", "cni", "docker", "br-", "virbr"];
	let mut rx = 0u64;
	let mut tx = 0u64;
	for line in text.lines() {
		let Some((name, counters)) = line.split_once(':') else {
			continue;
		};
		let name = name.trim();
		if VIRTUAL.iter().any(|v| name.starts_with(v)) {
			continue;
		}
		let fields: Vec<u64> = counters
			.split_whitespace()
			.map_while(|f| f.parse().ok())
			.collect();
		// Receive: bytes is field 0. Transmit: bytes is field 8.
		if let (Some(r), Some(t)) = (fields.first(), fields.get(8)) {
			rx = rx.saturating_add(*r);
			tx = tx.saturating_add(*t);
		}
	}
	(rx, tx)
}

/// Used, free and total bytes from `df -Pk <path>` (1024-byte blocks).
#[must_use]
pub fn parse_df(text: &str) -> Option<(u64, u64, u64)> {
	let line = text.lines().nth(1)?;
	let fields: Vec<&str> = line.split_whitespace().collect();
	// Filesystem, 1024-blocks, Used, Available, Capacity, Mounted on.
	let total: u64 = fields.get(1)?.parse().ok()?;
	let used: u64 = fields.get(2)?.parse().ok()?;
	let free: u64 = fields.get(3)?.parse().ok()?;
	Some((used * 1024, free * 1024, total * 1024))
}

/// The busiest GPU's utilisation (percent) and the VRAM in use over all GPUs
/// (bytes) from `nvidia-smi --query-gpu=utilization.gpu,memory.used
/// --format=csv,noheader,nounits`. `None` when no line parses (no GPU, or an
/// error message instead of CSV).
#[must_use]
pub fn parse_nvidia_usage(text: &str) -> Option<(f64, u64)> {
	let mut util: Option<f64> = None;
	let mut vram_mb = 0u64;
	let mut seen = false;
	for line in text.lines() {
		let mut cols = line.split(',').map(str::trim);
		let (Some(u), Some(m), None) = (cols.next(), cols.next(), cols.next()) else {
			continue;
		};
		// `[N/A]` on a column means the driver cannot tell; skip the line.
		let (Ok(u), Ok(m)) = (u.parse::<f64>(), m.parse::<u64>()) else {
			continue;
		};
		seen = true;
		util = Some(util.map_or(u, |best| best.max(u)));
		vram_mb += m;
	}
	seen.then(|| (util.unwrap_or(0.0), vram_mb * 1024 * 1024))
}

/// The podkit resource behind a `podkit.service` label: `app-<id>`,
/// `db-<id>`, `model-<id>` or `stack-<id>-<service>`.
#[must_use]
pub fn resource_of(service: &str) -> Option<ResourceRef> {
	let (kind, rest) = service.split_once('-')?;
	let (kind, id, compose) = match kind {
		"app" => ("application", rest, None),
		"db" => ("database", rest, None),
		"model" => ("model", rest, None),
		"stack" => {
			let (id, name) = rest.split_once('-')?;
			if name.is_empty() {
				return None;
			}
			("stack", id, Some(name.to_string()))
		}
		_ => return None,
	};
	Some(ResourceRef {
		kind: kind.to_string(),
		id: id.parse().ok().filter(|id| *id > 0)?,
		service: compose,
	})
}

fn cgroup_cpu(stats: &ContainerStatsResponse) -> Option<CgroupCpu> {
	let cpu = stats.cpu_stats.as_ref()?;
	Some(CgroupCpu {
		container: cpu.cpu_usage.as_ref()?.total_usage?,
		system: cpu.system_cpu_usage?,
	})
}

/// The engine's own previous reading, when it fills one in.
fn precpu(stats: &ContainerStatsResponse) -> Option<CgroupCpu> {
	let cpu = stats.precpu_stats.as_ref()?;
	let sample = CgroupCpu {
		container: cpu.cpu_usage.as_ref()?.total_usage?,
		system: cpu.system_cpu_usage?,
	};
	(sample.system > 0).then_some(sample)
}

/// Share of the whole host's CPU a container used between two readings,
/// 0 to 100. `None` when no host time passed or the counters went backwards.
#[must_use]
pub fn container_cpu_percent(prev: CgroupCpu, now: CgroupCpu) -> Option<f64> {
	let system = now.system.checked_sub(prev.system).filter(|s| *s > 0)?;
	let used = now.container.checked_sub(prev.container)?;
	#[allow(clippy::cast_precision_loss)]
	Some((used as f64 / system as f64 * 100.0).clamp(0.0, 100.0))
}

/// Memory in use without the page cache, as `docker stats` shows it: usage
/// minus `inactive_file` (cgroup v2) or `total_inactive_file`/`cache` (v1).
#[must_use]
pub fn container_mem(stats: &ContainerStatsResponse) -> u64 {
	let Some(memory) = stats.memory_stats.as_ref() else {
		return 0;
	};
	let usage = memory.usage.unwrap_or(0);
	let cache = memory.stats.as_ref().map_or(0, |s| {
		["inactive_file", "total_inactive_file", "cache"]
			.iter()
			.find_map(|k| s.get(*k).copied())
			.unwrap_or(0)
	});
	usage.saturating_sub(cache)
}

#[cfg(test)]
mod tests {
	use super::*;

	const PROC_STAT_A: &str = "cpu  4705 150 1120 16250 520 30 45 0 0 0\n\
		cpu0 1200 30 300 4000 130 10 11 0 0 0\n\
		cpu1 3505 120 820 12250 390 20 34 0 0 0\n\
		intr 12345\nctxt 98765\n";
	const PROC_STAT_B: &str = "cpu  4905 150 1220 16350 520 30 45 0 0 0\n\
		cpu0 1300 30 330 4050 130 10 11 0 0 0\n";

	#[test]
	fn stats_proc_stat_parses_the_aggregate_line() {
		let t = parse_proc_stat(PROC_STAT_A).unwrap();
		// user+nice+system+idle+iowait+irq+softirq = 22820; idle+iowait = 16770.
		assert_eq!(t.total, 22820);
		assert_eq!(t.busy, 6050);
	}

	#[test]
	fn stats_cpu_percent_is_a_delta() {
		let a = parse_proc_stat(PROC_STAT_A).unwrap();
		let b = parse_proc_stat(PROC_STAT_B).unwrap();
		// 300 busy of 400 total jiffies passed.
		let pct = cpu_percent(a, b).unwrap();
		assert!((pct - 75.0).abs() < 1e-9, "{pct}");
		// No time passed, or a counter reset: no figure.
		assert_eq!(cpu_percent(a, a), None);
		assert_eq!(cpu_percent(b, a), None);
	}

	#[test]
	fn stats_proc_stat_garbage_is_none() {
		assert_eq!(parse_proc_stat(""), None);
		assert_eq!(parse_proc_stat("cpu0 1 2 3 4\n"), None);
		assert_eq!(parse_proc_stat("cpu  1 2\n"), None);
	}

	#[test]
	fn stats_meminfo_used_is_total_minus_available() {
		let text = "MemTotal:       32768000 kB\nMemFree:         1000000 kB\n\
			MemAvailable:   20000000 kB\nBuffers: 5 kB\n";
		assert_eq!(parse_meminfo(text), (12_768_000 * 1024, 32_768_000 * 1024));
		// Old kernels: MemFree stands in for MemAvailable.
		assert_eq!(
			parse_meminfo("MemTotal: 100 kB\nMemFree: 40 kB\n"),
			(60 * 1024, 100 * 1024)
		);
		assert_eq!(parse_meminfo("nonsense"), (0, 0));
	}

	#[test]
	fn stats_net_dev_sums_physical_interfaces_only() {
		let text = "Inter-|   Receive                                                |  Transmit\n \
			face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
			    lo: 9000 10 0 0 0 0 0 0 9000 10 0 0 0 0 0 0\n\
			enp5s0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n\
			wlan0: 500 5 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n\
			veth1a2b: 777 1 0 0 0 0 0 0 888 1 0 0 0 0 0 0\n\
			podman0: 555 1 0 0 0 0 0 0 666 1 0 0 0 0 0 0\n";
		assert_eq!(parse_net_dev(text), (1500, 2100));
		assert_eq!(parse_net_dev(""), (0, 0));
	}

	#[test]
	fn stats_df_reports_used_free_and_total_in_bytes() {
		let text = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
			/dev/sda1        100000000  40000000  55000000      43% /\n";
		assert_eq!(
			parse_df(text),
			Some((40_000_000 * 1024, 55_000_000 * 1024, 100_000_000 * 1024))
		);
		assert_eq!(parse_df("df: nope"), None);
	}

	#[test]
	fn stats_nvidia_csv_takes_the_busiest_gpu_and_all_vram() {
		let text = "37, 1024\n91, 20480\n";
		assert_eq!(
			parse_nvidia_usage(text),
			Some((91.0, (1024 + 20480) * 1024 * 1024))
		);
		assert_eq!(parse_nvidia_usage("0, 0\n"), Some((0.0, 0)));
	}

	#[test]
	fn stats_nvidia_csv_without_a_gpu_is_none() {
		assert_eq!(parse_nvidia_usage(""), None);
		assert_eq!(
			parse_nvidia_usage("NVIDIA-SMI has failed because it couldn't communicate"),
			None
		);
		assert_eq!(parse_nvidia_usage("[N/A], [N/A]\n"), None);
	}

	#[test]
	fn stats_service_label_maps_to_a_resource() {
		let r = resource_of("app-76699709579710464").unwrap();
		assert_eq!(
			(r.kind.as_str(), r.id, r.service),
			("application", 76_699_709_579_710_464, None)
		);
		assert_eq!(resource_of("db-5").unwrap().kind, "database");
		assert_eq!(resource_of("model-9").unwrap().kind, "model");
		let s = resource_of("stack-12-web-api").unwrap();
		assert_eq!((s.kind.as_str(), s.id), ("stack", 12));
		assert_eq!(s.service.as_deref(), Some("web-api"));
		for bad in [
			"",
			"app",
			"app-",
			"app-x",
			"app-0",
			"stack-3",
			"stack-3-",
			"gateway-1",
			"traefik",
		] {
			assert_eq!(resource_of(bad), None, "{bad}");
		}
	}

	/// A `GET /containers/{id}/stats?stream=false` body of cgroup v2.
	const BOLLARD_STATS: &str = r#"{
		"read": "2026-11-18T18:00:30.123456789Z",
		"preread": "2026-11-18T18:00:29.123456789Z",
		"id": "abc123",
		"name": "/podkit-app-1-2",
		"cpu_stats": {
			"cpu_usage": {"total_usage": 2000000000, "usage_in_kernelmode": 500000000, "usage_in_usermode": 1500000000},
			"system_cpu_usage": 80000000000,
			"online_cpus": 8
		},
		"precpu_stats": {
			"cpu_usage": {"total_usage": 1000000000},
			"system_cpu_usage": 40000000000,
			"online_cpus": 8
		},
		"memory_stats": {
			"usage": 104857600,
			"limit": 8589934592,
			"stats": {"anon": 70000000, "file": 40000000, "inactive_file": 20971520}
		}
	}"#;

	fn bollard_stats() -> ContainerStatsResponse {
		serde_json::from_str(BOLLARD_STATS).unwrap()
	}

	#[test]
	fn stats_container_cpu_is_a_share_of_the_host() {
		let s = bollard_stats();
		let now = cgroup_cpu(&s).unwrap();
		let before = precpu(&s).unwrap();
		// 1 s of container time over 40 s of summed host time: 2.5 %.
		let pct = container_cpu_percent(before, now).unwrap();
		assert!((pct - 2.5).abs() < 1e-9, "{pct}");
		assert_eq!(container_cpu_percent(now, now), None);
		assert_eq!(container_cpu_percent(now, before), None);
	}

	#[test]
	fn stats_container_without_a_previous_reading_has_none() {
		let mut s = bollard_stats();
		s.precpu_stats = Some(bollard::models::ContainerCpuStats::default());
		assert_eq!(precpu(&s), None);
	}

	#[test]
	fn stats_container_memory_drops_the_page_cache() {
		assert_eq!(container_mem(&bollard_stats()), 104_857_600 - 20_971_520);
		let v1: ContainerStatsResponse =
			serde_json::from_str(r#"{"memory_stats":{"usage":1000,"stats":{"cache":300}}}"#)
				.unwrap();
		assert_eq!(container_mem(&v1), 700);
		let bare: ContainerStatsResponse = serde_json::from_str("{}").unwrap();
		assert_eq!(container_mem(&bare), 0);
	}
}
