//! Moral / ethical alignment (BL-33) — the D&D-style 9-box. **Distinct from
//! [`crate::comp::Alignment`]**, which is the AI-faction relationship
//! (Wild/Enemy/Npc/Tame/…). `Ethos` is held as two **scores** so it can drift
//! with a character's deeds; the discrete 9-box is derived from them.
//! See `docs/design/specs/2026-06-22-alignment-system-design.md`.
use super::Alignment;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, VecStorage};

/// Moral axis (Good–Evil), derived from `Ethos.good_evil`.
// `Hash` is needed so this can key a `HashMap` in a manifest asset (a
// per-alignment-band content table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Moral {
    Good,
    Neutral,
    Evil,
}

/// Ethical axis (Lawful–Chaotic), derived from `Ethos.law_chaos`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
    Lawful,
    Neutral,
    Chaotic,
}

impl Moral {
    /// The `Moral` alignments a party NPC may be rolled as, given the calling
    /// player's own `Moral` (BL-82 `/make_party`): a Good player never gets
    /// an Evil companion and vice versa; Neutral tolerates any of the three.
    /// Pure/data-only — no ECS access — so it's cheaply unit-testable in
    /// isolation from the rest of `/make_party`.
    pub fn compatible_npc_morals(self) -> &'static [Moral] {
        match self {
            Moral::Good => &[Moral::Good, Moral::Neutral],
            Moral::Neutral => &[Moral::Good, Moral::Neutral, Moral::Evil],
            Moral::Evil => &[Moral::Evil, Moral::Neutral],
        }
    }
}

/// A character's moral/ethical alignment. Two scores in `[-BOUND, BOUND]`
/// (`good_evil`: −Evil…+Good, `law_chaos`: −Chaotic…+Lawful); the classic 9-box
/// (Lawful Good … Chaotic Evil) is derived via
/// [`Ethos::moral`]/[`Ethos::order`]. Synced to clients; persisted per-PC.
/// Default (all-zero scores) = **True Neutral**.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ethos {
    pub good_evil: i16,
    pub law_chaos: i16,
}

impl Ethos {
    /// Score clamp bound.
    pub const BOUND: i16 = 100;
    /// Moral drift for slaying a hostile NPC — mildly Good and Lawful.
    pub const KILL_HOSTILE_DRIFT: (i16, i16) = (5, 2);
    /// Moral drift `(Δgood_evil, Δlaw_chaos)` a PC takes for killing a peaceful
    /// NPC — a strong pull to Evil, a touch Chaotic. First-pass magnitude;
    /// tuning deferred to telemetry / `game-balance-designer` (spec §6.2).
    pub const KILL_INNOCENT_DRIFT: (i16, i16) = (-15, -5);
    /// Score a freshly-chosen non-Neutral axis starts at (creation).
    pub const START: i16 = 66;
    /// `|score| > THRESHOLD` ⇒ a non-Neutral axis.
    pub const THRESHOLD: i16 = 33;

    fn moral_of(score: i16) -> Moral {
        if score > Self::THRESHOLD {
            Moral::Good
        } else if score < -Self::THRESHOLD {
            Moral::Evil
        } else {
            Moral::Neutral
        }
    }

    fn order_of(score: i16) -> Order {
        if score > Self::THRESHOLD {
            Order::Lawful
        } else if score < -Self::THRESHOLD {
            Order::Chaotic
        } else {
            Order::Neutral
        }
    }

    /// Discrete moral axis.
    pub fn moral(self) -> Moral { Self::moral_of(self.good_evil) }

    /// Discrete ethical axis.
    pub fn order(self) -> Order { Self::order_of(self.law_chaos) }

    /// The discrete 9-box as `(order, moral)`.
    pub fn alignment(self) -> (Order, Moral) { (self.order(), self.moral()) }

    /// Build from a chosen discrete 9-box (character creation): each
    /// non-Neutral axis starts at ±`START`, Neutral at 0.
    pub fn from_box(order: Order, moral: Moral) -> Self {
        let good_evil = match moral {
            Moral::Good => Self::START,
            Moral::Neutral => 0,
            Moral::Evil => -Self::START,
        };
        let law_chaos = match order {
            Order::Lawful => Self::START,
            Order::Neutral => 0,
            Order::Chaotic => -Self::START,
        };
        Self {
            good_evil,
            law_chaos,
        }
    }

    /// Seed a moral alignment from an NPC's AI-faction [`Alignment`] (BL-33
    /// Phase 2): a coarse default for spawned NPCs until an explicit per-config
    /// value or (later) AURORA drift refines it. Beasts/objects with no moral
    /// agency map to True Neutral.
    pub fn from_ai_alignment(alignment: Alignment) -> Self {
        match alignment {
            // Hostiles — bandits, cultists, monsters.
            Alignment::Enemy => Self::from_box(Order::Neutral, Moral::Evil),
            // Friendly townsfolk — villagers, guards, merchants.
            Alignment::Npc => Self::from_box(Order::Lawful, Moral::Good),
            // Beasts, pets, tamed creatures, passive objects: no moral agency.
            Alignment::Wild | Alignment::Tame | Alignment::Owned(_) | Alignment::Passive => {
                Self::default()
            },
        }
    }

    /// The moral drift `(Δgood_evil, Δlaw_chaos)` a *kill* applies to the
    /// killer, keyed on the victim's AI faction ([`Alignment`]) — BL-33 §6.2.
    /// `None` for a morally-neutral kill (beasts, pets, tamed, objects).
    pub fn kill_drift(victim: Alignment) -> Option<(i16, i16)> {
        match victim {
            // Peaceful folk — villagers, guards, merchants.
            Alignment::Npc => Some(Self::KILL_INNOCENT_DRIFT),
            // Hostiles — bandits, cultists, monsters.
            Alignment::Enemy => Some(Self::KILL_HOSTILE_DRIFT),
            // No moral weight.
            Alignment::Wild | Alignment::Tame | Alignment::Owned(_) | Alignment::Passive => None,
        }
    }

    /// Both scores clamped into `[-BOUND, BOUND]`. Used to sanitise a
    /// client-supplied alignment server-side (never trust the wire value).
    pub fn clamped(self) -> Self {
        Self {
            good_evil: self.good_evil.clamp(-Self::BOUND, Self::BOUND),
            law_chaos: self.law_chaos.clamp(-Self::BOUND, Self::BOUND),
        }
    }

    /// Drift the alignment by a deed (BL-33 §6.2), clamped to `[-BOUND,
    /// BOUND]`.
    pub fn nudge(&mut self, d_good_evil: i16, d_law_chaos: i16) {
        self.good_evil = (self.good_evil + d_good_evil).clamp(-Self::BOUND, Self::BOUND);
        self.law_chaos = (self.law_chaos + d_law_chaos).clamp(-Self::BOUND, Self::BOUND);
    }
}

impl Component for Ethos {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_true_neutral() {
        let e = Ethos::default();
        assert_eq!(e.moral(), Moral::Neutral);
        assert_eq!(e.order(), Order::Neutral);
    }

    #[test]
    fn thresholds_derive_the_box() {
        assert_eq!(Ethos::moral_of(34), Moral::Good);
        assert_eq!(Ethos::moral_of(33), Moral::Neutral);
        assert_eq!(Ethos::moral_of(-33), Moral::Neutral);
        assert_eq!(Ethos::moral_of(-34), Moral::Evil);
        assert_eq!(Ethos::order_of(34), Order::Lawful);
        assert_eq!(Ethos::order_of(-34), Order::Chaotic);
    }

    #[test]
    fn from_box_round_trips_the_corners() {
        for (order, moral) in [
            (Order::Lawful, Moral::Good),
            (Order::Chaotic, Moral::Evil),
            (Order::Neutral, Moral::Neutral),
        ] {
            let e = Ethos::from_box(order, moral);
            assert_eq!(e.alignment(), (order, moral));
        }
    }

    #[test]
    fn from_ai_alignment_seeds_sensible_boxes() {
        assert_eq!(
            Ethos::from_ai_alignment(Alignment::Enemy).moral(),
            Moral::Evil
        );
        let townsfolk = Ethos::from_ai_alignment(Alignment::Npc);
        assert_eq!(
            (townsfolk.order(), townsfolk.moral()),
            (Order::Lawful, Moral::Good)
        );
        // Beasts have no moral agency.
        assert_eq!(Ethos::from_ai_alignment(Alignment::Wild), Ethos::default());
    }

    #[test]
    fn kill_drift_only_for_moral_victims() {
        assert_eq!(
            Ethos::kill_drift(Alignment::Npc),
            Some(Ethos::KILL_INNOCENT_DRIFT)
        );
        assert_eq!(
            Ethos::kill_drift(Alignment::Enemy),
            Some(Ethos::KILL_HOSTILE_DRIFT)
        );
        assert_eq!(Ethos::kill_drift(Alignment::Wild), None);
        // Enough innocent kills flip a True-Neutral PC to Evil.
        let mut pc = Ethos::default();
        for _ in 0..5 {
            let (dge, dlc) = Ethos::kill_drift(Alignment::Npc).unwrap();
            pc.nudge(dge, dlc);
        }
        assert_eq!(pc.moral(), Moral::Evil);
    }

    #[test]
    fn compatible_npc_morals_excludes_the_opposite_pole() {
        assert_eq!(Moral::Good.compatible_npc_morals(), &[
            Moral::Good,
            Moral::Neutral
        ]);
        assert_eq!(Moral::Evil.compatible_npc_morals(), &[
            Moral::Evil,
            Moral::Neutral
        ]);
        assert_eq!(Moral::Neutral.compatible_npc_morals(), &[
            Moral::Good,
            Moral::Neutral,
            Moral::Evil
        ]);
        // A Good player never rolls an Evil companion, and vice versa.
        assert!(!Moral::Good.compatible_npc_morals().contains(&Moral::Evil));
        assert!(!Moral::Evil.compatible_npc_morals().contains(&Moral::Good));
        // Neutral is always in range (never excluded from a compatible party).
        for player_moral in [Moral::Good, Moral::Neutral, Moral::Evil] {
            assert!(
                player_moral
                    .compatible_npc_morals()
                    .contains(&Moral::Neutral)
            );
        }
    }

    #[test]
    fn nudge_clamps_and_can_flip_the_box() {
        let mut e = Ethos::from_box(Order::Neutral, Moral::Good); // good_evil = +66
        e.nudge(-200, 0); // a heinous deed
        assert_eq!(e.good_evil, -Ethos::BOUND); // clamped, not overflowed
        assert_eq!(e.moral(), Moral::Evil);
        e.nudge(500, 500);
        assert_eq!(e.good_evil, Ethos::BOUND);
        assert_eq!(e.law_chaos, Ethos::BOUND);
    }
}
