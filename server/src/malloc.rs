//! glibc malloc settings for a small idle footprint.
//!
//! By default glibc gives every thread its own arena (up to eight per core) and keeps up to
//! 128 KiB of freed memory at the top of the heap. The server is I/O-bound, so a couple of arenas
//! are plenty and handing freed memory back sooner is cheap: about 0.6 MB less idle RSS.
//! `GLIBC_TUNABLES` still wins when someone sets it.

/// Applies the settings. Call it first thing in `main`, before the runtime starts its threads.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn tune() {
	const M_TRIM_THRESHOLD: i32 = -1;
	const M_ARENA_MAX: i32 = -8;
	unsafe extern "C" {
		fn mallopt(param: i32, value: i32) -> i32;
	}
	if std::env::var_os("GLIBC_TUNABLES").is_some() {
		return;
	}
	// SAFETY: `mallopt` only reads two integers and is safe to call before other threads exist.
	unsafe {
		mallopt(M_ARENA_MAX, 2);
		mallopt(M_TRIM_THRESHOLD, 64 * 1024);
	}
}

/// Other C libraries have no `mallopt`.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn tune() {}
