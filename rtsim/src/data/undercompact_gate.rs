use serde::{Deserialize, Serialize};

/// Identifies one of the two vault levers in the Undercompact gate
/// antechamber (COW-7b). Deliberately not tied to a world position here --
/// `world::layer::cromatolis_interior::UndercompactGateAntechamberGeometry`
/// owns that, and resolves it fresh from the authored data; this type only
/// ever needs to distinguish "the first lever" from "the second lever" for
/// persistence.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UndercompactGateLever {
    A,
    B,
}

/// World-persistent state of the Undercompact gate antechamber's two-lever
/// puzzle (COW-7b): which levers have been pulled, and whether the sealed
/// gate has been solved. Once `solved` is set it is **never** cleared --
/// the puzzle stays open forever, including across a server restart, which
/// is the whole reason this lives in rtsim's persisted `Data` rather than as
/// transient ECS/world-gen state (world-gen re-carves the plug
/// deterministically from RON on every first chunk load, with no memory of its
/// own of whether it was ever cleared).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UndercompactGateLevers {
    lever_a: bool,
    lever_b: bool,
    #[serde(default)]
    solved: bool,
}

impl UndercompactGateLevers {
    /// Marks `lever` as pulled. Idempotent: pulling an already-active lever
    /// changes nothing.
    ///
    /// Returns `true` only for the one call that *newly* solves the puzzle
    /// (both levers now active, and it was not already solved) -- the
    /// caller uses this to gate the one-shot plug-clear write so a repeat or
    /// duplicate activation can never re-queue it.
    pub fn activate(&mut self, lever: UndercompactGateLever) -> bool {
        match lever {
            UndercompactGateLever::A => self.lever_a = true,
            UndercompactGateLever::B => self.lever_b = true,
        }
        if !self.solved && self.lever_a && self.lever_b {
            self.solved = true;
            true
        } else {
            false
        }
    }

    pub fn is_lever_active(&self, lever: UndercompactGateLever) -> bool {
        match lever {
            UndercompactGateLever::A => self.lever_a,
            UndercompactGateLever::B => self.lever_b,
        }
    }

    /// Whether the gate has ever been solved. One-way: never reverts to
    /// `false` once `true`, even if a future caller somehow un-set a lever.
    pub fn is_solved(&self) -> bool { self.solved }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activating_one_lever_alone_does_not_solve_the_gate() {
        let mut levers = UndercompactGateLevers::default();
        let newly_solved = levers.activate(UndercompactGateLever::A);
        assert!(!newly_solved);
        assert!(!levers.is_solved());
        assert!(levers.is_lever_active(UndercompactGateLever::A));
        assert!(!levers.is_lever_active(UndercompactGateLever::B));
    }

    #[test]
    fn activating_both_levers_solves_the_gate_exactly_once() {
        let mut levers = UndercompactGateLevers::default();
        assert!(!levers.activate(UndercompactGateLever::A));
        // The second, *different* lever is the one that actually solves it.
        assert!(levers.activate(UndercompactGateLever::B));
        assert!(levers.is_solved());

        // Re-activating either lever afterward must never report "newly
        // solved" again -- the caller relies on this to avoid a double
        // clear of the plug.
        assert!(!levers.activate(UndercompactGateLever::A));
        assert!(!levers.activate(UndercompactGateLever::B));
        assert!(levers.is_solved());
    }

    #[test]
    fn activating_an_already_active_lever_is_a_harmless_no_op() {
        let mut levers = UndercompactGateLevers::default();
        assert!(!levers.activate(UndercompactGateLever::A));
        assert!(!levers.activate(UndercompactGateLever::A));
        assert!(!levers.is_solved());
        assert!(levers.is_lever_active(UndercompactGateLever::A));
    }

    /// A save written before this registry existed must still load, at the
    /// same rtsim data version -- the same additive `#[serde(default)]`
    /// contract `Data::banished`'s own test pins end to end.
    #[test]
    fn round_trips_through_the_real_wire_codec() {
        let mut levers = UndercompactGateLevers::default();
        levers.activate(UndercompactGateLever::A);
        levers.activate(UndercompactGateLever::B);

        let encoded = rmp_serde::to_vec_named(&levers).expect("serialise");
        let decoded: UndercompactGateLevers = rmp_serde::from_slice(&encoded).expect("deserialise");
        assert!(decoded.is_solved());
        assert!(decoded.is_lever_active(UndercompactGateLever::A));
        assert!(decoded.is_lever_active(UndercompactGateLever::B));
    }
}
