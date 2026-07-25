//! `DmEvent.narrative.world_rumor` feeds the real chronicle/lore system,
//! which does not exist yet. [`ChronicleLog`] is an explicit stand-in: a
//! plain in-memory, append-only, bounded-length log. The real system
//! replaces this resource later without touching `common_oracle::DmEvent`.

use std::collections::VecDeque;

/// Clamp bounds for [`ChronicleLog`] (anti-chaos, same posture as
/// `common_oracle::dm_event::bounds`): a misbehaving or malicious ORACLE
/// event stream dropping many `world_rumor`-carrying files must not grow
/// this resource unboundedly — oldest entries are dropped once the cap is
/// hit.
pub mod bounds {
    /// Maximum number of entries [`super::ChronicleLog`] retains.
    pub const MAX_ENTRIES: usize = 1024;
}

/// In-memory, append-only chronicle-hook log. Bounded to
/// [`bounds::MAX_ENTRIES`] (oldest entries drop first) so this resource
/// cannot grow without limit for the lifetime of a long-running server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChronicleLog(VecDeque<String>);

impl ChronicleLog {
    /// Appends `text`, dropping the oldest entry first if already at
    /// [`bounds::MAX_ENTRIES`].
    pub fn push(&mut self, text: impl Into<String>) {
        if self.0.len() >= bounds::MAX_ENTRIES {
            self.0.pop_front();
        }
        self.0.push_back(text.into());
    }

    /// Iterates entries oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &str> { self.0.iter().map(String::as_str) }

    #[must_use]
    pub fn len(&self) -> usize { self.0.len() }

    #[must_use]
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chronicle_log_is_bounded() {
        let mut log = ChronicleLog::default();
        for i in 0..bounds::MAX_ENTRIES + 10 {
            log.push(format!("entry {i}"));
        }
        assert_eq!(log.len(), bounds::MAX_ENTRIES);
        // The oldest 10 entries ("entry 0".."entry 9") must have been
        // dropped; the log now starts at "entry 10".
        assert_eq!(log.iter().next(), Some("entry 10"));
    }

    #[test]
    fn push_then_iter_preserves_order() {
        let mut log = ChronicleLog::default();
        log.push("first");
        log.push("second");
        assert_eq!(log.iter().collect::<Vec<_>>(), vec!["first", "second"]);
    }
}
