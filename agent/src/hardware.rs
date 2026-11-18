//! `GET /v1/hardware`: CPU, memory, disks, GPUs and CDI specs of this host.
//!
//! Every source is parsed by a pure function over a string (or a directory
//! root, for sysfs and CDI) so the tests run on fixtures. Collection never
//! fails: a missing tool or file just leaves that part empty.

use std::path::{Path, PathBuf};
use std::time::Duration;

use proto::{Cdi, Cpu, Disk, Gpu, Hardware, Memory};
use tokio::process::Command;

const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
const CDI_DIRS: [&str; 2] = ["/etc/cdi", "/var/run/cdi"];
const AMD_VENDOR_ID: &str = "0x1002";

/// Gathers a snapshot. `graph_root` is Podman's storage dir when known;
/// `data_dir` is the agent's.
pub async fn collect(graph_root: Option<&str>, data_dir: &Path) -> Hardware {
	let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
	let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();

	let mut paths: Vec<PathBuf> = Vec::new();
	if let Some(root) = graph_root.filter(|r| !r.is_empty()) {
		paths.push(PathBuf::from(root));
	}
	paths.push(data_dir.to_path_buf());
	paths.dedup();
	let mut disks = Vec::new();
	for path in paths {
		if let Some(disk) = disk_usage(&path).await {
			disks.push(disk);
		}
	}

	let mut gpus = nvidia_smi().await;
	gpus.extend(read_amd_gpus(Path::new("/sys/class/drm")));

	Hardware {
		cpu: parse_cpuinfo(&cpuinfo),
		memory: parse_meminfo(&meminfo),
		disks,
		gpus,
		cdi: read_cdi(&CDI_DIRS.map(PathBuf::from)),
	}
}

const NVIDIA_ARGS: [&str; 2] = [
	"--query-gpu=name,memory.total,memory.used",
	"--format=csv,noheader,nounits",
];

/// `$PATH` first, then where NixOS and distro packages put it (the agent's
/// service environment often has a minimal `$PATH`).
pub(crate) const NVIDIA_SMI: [&str; 3] = [
	"nvidia-smi",
	"/run/current-system/sw/bin/nvidia-smi",
	"/usr/bin/nvidia-smi",
];

async fn nvidia_smi() -> Vec<Gpu> {
	for program in NVIDIA_SMI {
		if let Some(out) = run(program, &NVIDIA_ARGS).await {
			return parse_nvidia_smi(&out);
		}
	}
	Vec::new()
}

/// Stdout of a successful command; `None` when it is missing, fails or hangs.
pub(crate) async fn run(program: &str, args: &[&str]) -> Option<String> {
	let output = tokio::time::timeout(
		TOOL_TIMEOUT,
		Command::new(program).args(args).kill_on_drop(true).output(),
	)
	.await
	.ok()?
	.ok()?;
	output
		.status
		.success()
		.then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn disk_usage(path: &Path) -> Option<Disk> {
	let out = run("df", &["-Pk", &path.to_string_lossy()]).await?;
	let (free_mb, total_mb) = parse_df(&out)?;
	Some(Disk {
		path: path.to_string_lossy().into_owned(),
		free_mb,
		total_mb,
	})
}

/// `MemTotal` and `MemAvailable` of `/proc/meminfo` (kB) in MB. Without
/// `MemAvailable` (kernels before 3.14) it falls back to `MemFree`.
#[must_use]
pub fn parse_meminfo(text: &str) -> Memory {
	let kb = |key: &str| -> Option<u64> {
		text.lines()
			.find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
			.and_then(|rest| rest.split_whitespace().next()?.parse().ok())
	};
	Memory {
		total_mb: kb("MemTotal").unwrap_or(0) / 1024,
		available_mb: kb("MemAvailable").or_else(|| kb("MemFree")).unwrap_or(0) / 1024,
	}
}

/// The first `model name` (or `Hardware`/`Model` on ARM) and the count of
/// `processor` entries of `/proc/cpuinfo`.
#[must_use]
pub fn parse_cpuinfo(text: &str) -> Cpu {
	let mut threads = 0u32;
	let mut model = None;
	for line in text.lines() {
		let Some((key, value)) = line.split_once(':') else {
			continue;
		};
		match key.trim() {
			"processor" => threads += 1,
			"model name" | "Hardware" | "Model" if model.is_none() => {
				let value = value.trim();
				if !value.is_empty() {
					model = Some(value.to_string());
				}
			}
			_ => {}
		}
	}
	Cpu {
		model: model.unwrap_or_default(),
		threads,
	}
}

/// Rows of `nvidia-smi --query-gpu=name,memory.total,memory.used
/// --format=csv,noheader,nounits` (MiB). Unparsable rows are skipped.
#[must_use]
pub fn parse_nvidia_smi(text: &str) -> Vec<Gpu> {
	text.lines()
		.filter_map(|line| {
			// The name has no commas, but split from the right to be safe.
			let mut parts = line.rsplitn(3, ',');
			let used = parts.next()?.trim().parse().ok()?;
			let total = parts.next()?.trim().parse().ok()?;
			let name = parts.next()?.trim();
			(!name.is_empty()).then(|| Gpu {
				vendor: "nvidia".to_string(),
				name: name.to_string(),
				vram_mb: total,
				vram_used_mb: Some(used),
			})
		})
		.collect()
}

/// Free and total MB from `df -Pk <path>` (POSIX format: 1024-byte blocks).
#[must_use]
pub fn parse_df(text: &str) -> Option<(u64, u64)> {
	let line = text.lines().nth(1)?;
	let fields: Vec<&str> = line.split_whitespace().collect();
	// Filesystem, 1024-blocks, Used, Available, Capacity, Mounted on.
	let total: u64 = fields.get(1)?.parse().ok()?;
	let free: u64 = fields.get(3)?.parse().ok()?;
	Some((free / 1024, total / 1024))
}

/// One `/sys/class/drm/card*/device` from its file contents. `None` unless
/// it is an AMD device that reports VRAM.
#[must_use]
pub fn parse_amd_device(
	vendor: &str,
	vram_total: Option<&str>,
	vram_used: Option<&str>,
	product_name: Option<&str>,
	device_id: Option<&str>,
) -> Option<Gpu> {
	if vendor.trim() != AMD_VENDOR_ID {
		return None;
	}
	let bytes = |s: &str| s.trim().parse::<u64>().ok().map(|b| b / (1024 * 1024));
	let vram_mb = bytes(vram_total?)?;
	let name = product_name
		.map(str::trim)
		.filter(|n| !n.is_empty())
		.map(str::to_string)
		.or_else(|| device_id.map(|id| format!("AMD GPU {}", id.trim())))
		.unwrap_or_else(|| "AMD GPU".to_string());
	Some(Gpu {
		vendor: "amd".to_string(),
		name,
		vram_mb,
		vram_used_mb: vram_used.and_then(bytes),
	})
}

/// AMD GPUs under a `/sys/class/drm`-shaped directory.
#[must_use]
pub fn read_amd_gpus(drm: &Path) -> Vec<Gpu> {
	let Ok(entries) = std::fs::read_dir(drm) else {
		return Vec::new();
	};
	let mut cards: Vec<(u32, PathBuf)> = entries
		.flatten()
		.filter_map(|e| {
			let name = e.file_name().into_string().ok()?;
			// `card0`, not `card0-DP-1` or `renderD128`.
			let index = name.strip_prefix("card")?.parse().ok()?;
			Some((index, e.path().join("device")))
		})
		.collect();
	cards.sort();
	cards
		.iter()
		.filter_map(|(_, dir)| {
			let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
			parse_amd_device(
				&read("vendor")?,
				read("mem_info_vram_total").as_deref(),
				read("mem_info_vram_used").as_deref(),
				read("product_name").as_deref(),
				read("device").as_deref(),
			)
		})
		.collect()
}

/// The `kind` of a CDI spec (`kind: nvidia.com/gpu` in YAML, `"kind": "..."`
/// in JSON).
#[must_use]
pub fn parse_cdi_kind(text: &str) -> Option<String> {
	text.lines().find_map(|line| {
		let line = line.trim().trim_end_matches(',');
		let value = line
			.strip_prefix("kind:")
			.or_else(|| line.strip_prefix("\"kind\":"))?;
		let value = value.trim().trim_matches(['"', '\'']);
		(!value.is_empty()).then(|| value.to_string())
	})
}

/// Spec files (`*.yaml`, `*.yml`, `*.json`) in the standard CDI directories.
#[must_use]
pub fn host_cdi() -> Cdi {
	read_cdi(&CDI_DIRS.map(PathBuf::from))
}

/// Spec files (`*.yaml`, `*.yml`, `*.json`) in `dirs`. `available` is true
/// when there is at least one; `devices` lists their distinct kinds.
#[must_use]
pub fn read_cdi(dirs: &[PathBuf]) -> Cdi {
	let mut available = false;
	let mut devices: Vec<String> = Vec::new();
	for dir in dirs {
		let Ok(entries) = std::fs::read_dir(dir) else {
			continue;
		};
		for path in entries.flatten().map(|e| e.path()) {
			let is_spec = path
				.extension()
				.and_then(|e| e.to_str())
				.is_some_and(|e| matches!(e, "yaml" | "yml" | "json"));
			if !is_spec {
				continue;
			}
			available = true;
			if let Some(kind) = std::fs::read_to_string(&path)
				.ok()
				.and_then(|t| parse_cdi_kind(&t))
				&& !devices.contains(&kind)
			{
				devices.push(kind);
			}
		}
	}
	devices.sort();
	Cdi { available, devices }
}

#[cfg(test)]
mod tests {
	use super::*;

	const MEMINFO: &str = "MemTotal:       31457280 kB\nMemFree:         1048576 kB\nMemAvailable:   14680064 kB\nBuffers:           10 kB\n";
	const CPUINFO: &str = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 9850X3D 8-Core Processor\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 9850X3D 8-Core Processor\n";

	#[test]
	fn meminfo_in_mb() {
		assert_eq!(
			parse_meminfo(MEMINFO),
			Memory {
				total_mb: 30720,
				available_mb: 14336
			}
		);
	}

	#[test]
	fn meminfo_without_available_uses_free() {
		let m = parse_meminfo("MemTotal: 2048 kB\nMemFree: 1024 kB\n");
		assert_eq!((m.total_mb, m.available_mb), (2, 1));
	}

	#[test]
	fn meminfo_garbage_is_zero() {
		assert_eq!(parse_meminfo(""), Memory::default());
		assert_eq!(parse_meminfo("MemTotal: lots kB"), Memory::default());
	}

	#[test]
	fn cpuinfo_counts_processors() {
		assert_eq!(
			parse_cpuinfo(CPUINFO),
			Cpu {
				model: "AMD Ryzen 7 9850X3D 8-Core Processor".to_string(),
				threads: 2
			}
		);
		assert_eq!(parse_cpuinfo(""), Cpu::default());
	}

	#[test]
	fn nvidia_smi_one_gpu() {
		let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 5070, 12227, 1500\n");
		assert_eq!(
			gpus,
			vec![Gpu {
				vendor: "nvidia".to_string(),
				name: "NVIDIA GeForce RTX 5070".to_string(),
				vram_mb: 12227,
				vram_used_mb: Some(1500)
			}]
		);
	}

	#[test]
	fn nvidia_smi_two_gpus() {
		let gpus = parse_nvidia_smi("NVIDIA A100, 40960, 0\nNVIDIA A100, 40960, 512\n");
		assert_eq!(gpus.len(), 2);
		assert_eq!(gpus[1].vram_used_mb, Some(512));
	}

	#[test]
	fn nvidia_smi_empty_or_error_text_means_no_gpus() {
		assert!(parse_nvidia_smi("").is_empty());
		assert!(parse_nvidia_smi("No devices were found\n").is_empty());
		assert!(
			parse_nvidia_smi("NVIDIA-SMI has failed because it couldn't communicate").is_empty()
		);
	}

	#[test]
	fn df_posix_output() {
		let out = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/nvme0n1p2   1000000000 840000000 160000000      84% /\n";
		assert_eq!(parse_df(out), Some((156_250, 976_562)));
		assert_eq!(parse_df("Filesystem\n"), None);
		assert_eq!(parse_df(""), None);
	}

	#[test]
	fn amd_device_fixture() {
		let gpu = parse_amd_device(
			"0x1002\n",
			Some("17163091968\n"),
			Some("1073741824\n"),
			Some("Radeon RX 7900 XT\n"),
			Some("0x744c\n"),
		)
		.unwrap();
		assert_eq!(
			gpu,
			Gpu {
				vendor: "amd".to_string(),
				name: "Radeon RX 7900 XT".to_string(),
				vram_mb: 16368,
				vram_used_mb: Some(1024)
			}
		);
	}

	#[test]
	fn amd_device_without_product_name_or_used() {
		let gpu =
			parse_amd_device("0x1002", Some("8589934592"), None, None, Some("0x73bf\n")).unwrap();
		assert_eq!(gpu.name, "AMD GPU 0x73bf");
		assert_eq!(gpu.vram_used_mb, None);
	}

	#[test]
	fn amd_device_rejects_other_vendors_and_missing_vram() {
		assert!(parse_amd_device("0x10de", Some("8589934592"), None, None, None).is_none());
		// Integrated GPUs and Intel cards have no `mem_info_vram_total` we trust.
		assert!(parse_amd_device("0x1002", None, None, None, None).is_none());
	}

	#[test]
	fn amd_sysfs_tree() {
		let dir = tempfile::tempdir().unwrap();
		let device = dir.path().join("card1/device");
		std::fs::create_dir_all(&device).unwrap();
		std::fs::write(device.join("vendor"), "0x1002\n").unwrap();
		std::fs::write(device.join("mem_info_vram_total"), "4294967296\n").unwrap();
		std::fs::write(device.join("mem_info_vram_used"), "0\n").unwrap();
		// An NVIDIA card, a connector and a render node are all ignored.
		let nv = dir.path().join("card0/device");
		std::fs::create_dir_all(&nv).unwrap();
		std::fs::write(nv.join("vendor"), "0x10de\n").unwrap();
		std::fs::create_dir_all(dir.path().join("card1-DP-1")).unwrap();
		std::fs::create_dir_all(dir.path().join("renderD128")).unwrap();
		let gpus = read_amd_gpus(dir.path());
		assert_eq!(gpus.len(), 1);
		assert_eq!(gpus[0].vram_mb, 4096);
		assert!(read_amd_gpus(&dir.path().join("missing")).is_empty());
	}

	#[test]
	fn cdi_kind_yaml_and_json() {
		assert_eq!(
			parse_cdi_kind("cdiVersion: 0.5.0\nkind: nvidia.com/gpu\ndevices:\n").as_deref(),
			Some("nvidia.com/gpu")
		);
		assert_eq!(
			parse_cdi_kind("{\n  \"cdiVersion\": \"0.5.0\",\n  \"kind\": \"amd.com/gpu\",\n}")
				.as_deref(),
			Some("amd.com/gpu")
		);
		assert_eq!(parse_cdi_kind("devices: []"), None);
	}

	#[test]
	fn cdi_dirs() {
		let dir = tempfile::tempdir().unwrap();
		let missing = dir.path().join("nope");
		assert_eq!(read_cdi(std::slice::from_ref(&missing)), Cdi::default());

		std::fs::write(dir.path().join("README"), "not a spec").unwrap();
		assert!(!read_cdi(std::slice::from_ref(&dir.path().to_path_buf())).available);

		std::fs::write(dir.path().join("nvidia.yaml"), "kind: nvidia.com/gpu\n").unwrap();
		std::fs::write(dir.path().join("nvidia2.yaml"), "kind: nvidia.com/gpu\n").unwrap();
		let cdi = read_cdi(&[missing, dir.path().to_path_buf()]);
		assert!(cdi.available);
		assert_eq!(cdi.devices, vec!["nvidia.com/gpu".to_string()]);
	}

	#[tokio::test]
	async fn collect_never_fails() {
		let hw = collect(None, Path::new("/nonexistent-podkit-test")).await;
		// Whatever the host has, the call returns and the data dir is just absent.
		assert!(
			hw.disks
				.iter()
				.all(|d| d.path != "/nonexistent-podkit-test")
		);
	}
}
