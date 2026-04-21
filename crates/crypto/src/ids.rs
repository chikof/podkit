use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 2026-04-21 00:00:00 UTC (ms)
const CUSTOM_EPOCH_MS: u64 = 1_776_729_600_000;

const MACHINE_ID_BITS: u64 = 10;
const SEQUENCE_BITS: u64 = 12;
const MAX_MACHINE_ID: u64 = (1 << MACHINE_ID_BITS) - 1;
const MAX_SEQUENCE: u64 = (1 << SEQUENCE_BITS) - 1;
const MACHINE_ID_SHIFT: u64 = SEQUENCE_BITS;
const TIMESTAMP_SHIFT: u64 = SEQUENCE_BITS + MACHINE_ID_BITS;

// FNV-1a 64-bit constants from the FNV spec (http://www.isthe.com/chongo/tech/comp/fnv/).
/// The initial basis - chosen empirically to minimize hash collisions.
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
/// The multiplier - a 64-bit prime with good bit-avalanche properties.
const FNV_PRIME: u64 = 1_099_511_628_211;

struct SnowflakeGenerator {
	machine_id: u64,
	sequence: u64,
	last_ms: u64,
}

impl SnowflakeGenerator {
	fn new() -> Self {
		Self {
			machine_id: machine_id(),
			sequence: 0,
			last_ms: 0,
		}
	}

	fn next(&mut self) -> u64 {
		let mut now = now_ms();

		if now < self.last_ms {
			while now < self.last_ms {
				now = now_ms();
			}
		}

		if now == self.last_ms {
			self.sequence = (self.sequence + 1) & MAX_SEQUENCE;
			if self.sequence == 0 {
				while now <= self.last_ms {
					now = now_ms();
				}
			}
		} else {
			self.sequence = 0;
		}

		self.last_ms = now;

		(now << TIMESTAMP_SHIFT) | (self.machine_id << MACHINE_ID_SHIFT) | self.sequence
	}
}

/// Returns milliseconds since `CUSTOM_EPOCH_MS`.
fn now_ms() -> u64 {
	u64::try_from(
		SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("system clock is before the Unix epoch")
			.as_millis(),
	)
	.expect("couldn't convert to u64")
		- CUSTOM_EPOCH_MS
}

/// Derives a 10-bit machine ID.
///
/// Priority:
///   1. `PODKIT_MACHINE_ID` env var (0–1023).
///   2. FNV-1a hash of `HOSTNAME` env var + process ID, folded to 10 bits.
fn machine_id() -> u64 {
	if let Ok(val) = std::env::var("PODKIT_MACHINE_ID")
		&& let Ok(id) = val.parse::<u64>()
	{
		return id & MAX_MACHINE_ID;
	}

	let hostname = std::env::var("HOSTNAME").unwrap_or("podkit".to_string());
	let pid = std::process::id().to_string();

	let mut hash = FNV_OFFSET;
	for byte in hostname.bytes().chain(pid.bytes()) {
		hash ^= u64::from(byte);
		hash = hash.wrapping_mul(FNV_PRIME);
	}

	hash & MAX_MACHINE_ID
}

static GENERATOR: LazyLock<Mutex<SnowflakeGenerator>> =
	LazyLock::new(|| Mutex::new(SnowflakeGenerator::new()));

/// Generates a unique Snowflake ID.
///
/// Layout: 41-bit ms timestamp | 10-bit machine ID | 12-bit sequence.
/// Monotonically increasing, time-sortable, safe for concurrent use.
///
/// # Panics
/// Panics if the internal mutex is poisoned (should never happen in normal operation).
pub fn generate_id() -> i64 {
	GENERATOR
		.lock()
		.expect("snowflake generator mutex poisoned")
		.next() as i64
}
