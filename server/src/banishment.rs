//! Banishment lifecycle support.
//!
//! For now this module only owns the wall clock every banishment deadline is
//! measured against. The park / return / rehydrate maintenance pass
//! (`maintain(&mut Server)`) lands with task N38B21-H and joins this module.

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock seconds since the UNIX epoch — the only clock in this engine
/// that means the same thing before and after a server restart. See
/// `comp::Banished`'s doc comment for why `Time` and `TimeOfDay` do not.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole design hangs on a monotonic real-time clock that survives a
    /// restart; if this ever returns 0 the registry silently returns every
    /// banished creature at once.
    #[test]
    fn the_wall_clock_is_a_plausible_unix_timestamp() {
        // 2023-01-01T00:00:00Z — any real clock is well past this.
        assert!(now_unix_secs() > 1_672_531_200);
    }

    #[test]
    fn the_wall_clock_is_monotonic_across_calls() {
        let a = now_unix_secs();
        let b = now_unix_secs();
        assert!(b >= a);
    }
}
