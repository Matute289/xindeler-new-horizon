//! What a body is physically able to do when it moves through the world.
//!
//! Before this module, the entire traversal-capability system was one line:
//!
//! ```ignore
//! pub fn can_climb(&self) -> bool { matches!(self, Body::Humanoid(_)) }
//! ```
//!
//! Every one of the 224 shipped creature species answered `false`, including
//! the ones that obviously should not — a cave spider, a gnoll, a wyvern — so
//! any obstacle whose only route was a climb was a one-way valve: passable by
//! players, impassable by every NPC in the game.
//!
//! # Morphology in, capabilities out
//!
//! The table in `assets/common/body_traversal.ron` records **what a creature
//! is**, not **what it is allowed to do**: a contact structure ([`Grip`]), what
//! the body's long axis can do ([`Frame`]), and a weight-bearing limb count.
//! [`TraversalMorphology::derive`] turns those plus the body's own mass and
//! height into [`TraversalCapabilities`].
//!
//! That indirection is the whole design, and it is load-bearing for a reason a
//! boolean `climb: true` column cannot serve: a boolean records the
//! *conclusion* and throws away the *reason*. A new clawed, quadrupedal
//! construct added to the bestiary would need somebody to notice its row and
//! decide again. Authoring the morphology instead means it inherits the answer
//! by construction — and `traversal_rule_admits_a_future_clawed_construct`
//! locks that in.
//!
//! It also changes what reviewing a row means. `climb: true` can only be
//! checked against taste (*should a crocodile climb?*); `grip: Claws` can be
//! checked against fact (*does a crocodile have claws?* — yes). Disagreements
//! then move to [`TraversalMorphology::derive`], where they are argued once
//! instead of 224 times.
//!
//! # Why a new axis rather than `CreatureTags`
//!
//! [`crate::comp::creature_type::CreatureTags`] already carries `FLYING` and
//! `AQUATIC` and has 30 spare bits, so extending it looks cheaper. It is the
//! wrong home: that axis is *lore* classification, it is rebuilt every tick by
//! the buff system, and auras union into it — a predicate read by collision and
//! pathfinding should not be a field a buff can edit by accident. Its own
//! module doc warns against conflating it with the body-shape axis, and this is
//! the body-shape axis.
//!
//! # What is deliberately not here
//!
//! `FLY` and `SWIM` are **derived from the engine**, not authored:
//! [`Body::fly_thrust`] and [`Body::swim_thrust`] already answer both, and a
//! second authored copy could only drift from them.

use common_assets::{AssetExt, AssetHandle, Ron};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

use super::{
    Body, arthropod, biped_large, biped_small, bird_large, bird_medium, crustacean, dragon,
    fish_medium, fish_small, golem, quadruped_low, quadruped_medium, quadruped_small, theropod,
};

bitflags::bitflags! {
    /// What a body can physically do when moving through the world.
    ///
    /// Never authored directly — see [`TraversalMorphology::derive`], which is
    /// the only thing that produces one of these.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
    pub struct TraversalCapabilities: u16 {
        /// May enter `CharacterState::Climb` and hold a vertical surface.
        const CLIMB   = 1 << 0;
        /// May wallrun. Strictly a subset of `CLIMB`: running up a wall needs
        /// everything hanging from one does, plus the build to do it at speed.
        const WALLRUN = 1 << 1;
        /// May adopt a crouched posture.
        const CROUCH  = 1 << 2;
        /// May adopt a prone posture. Independent of `CROUCH` in both
        /// directions: a serpent is prone-shaped already and cannot crouch.
        const PRONE   = 1 << 3;
        /// The cross-section compresses, so the body can enter a passage
        /// narrower than its resting width.
        ///
        /// ⚪ Derived but **not consumed yet** — nothing narrows a collision
        /// cylinder or admits a body to a tight gap on the strength of it. It
        /// is derived anyway because it is a property of the build the rest of
        /// the table already describes, so leaving it out would mean deriving
        /// it from scratch later rather than reading it.
        const SQUEEZE = 1 << 4;
        /// May cross a gap by jumping.
        const JUMP    = 1 << 5;
        /// Self-propelled in liquid, as opposed to merely buoyant. Derived.
        const SWIM    = 1 << 6;
        /// Powered flight. Derived.
        const FLY     = 1 << 7;
        /// Moves through solid ground. Nothing derives this yet; it exists so
        /// that the first burrower is a rule change and not a schema change.
        const BURROW  = 1 << 8;
    }
}

/// How a body's limbs meet a surface — the single most load-bearing descriptor,
/// because it decides how much weight the body can hold against a wall.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub enum Grip {
    /// No limbs that meet a surface at all: serpents, worms, gastropods.
    None,
    /// Flippers and fins. Propulsion in water, useless against rock.
    Fins,
    /// Hooves. Excellent on ground, no purchase on a vertical face.
    Hooves,
    /// Soft pads without usable claws: canids, elephants, lagomorphs.
    Pads,
    /// Opposable hands, or forelimbs used as such.
    Hands,
    /// Curved keratin claws that dig in — felids, ursids, crocodilians, drakes.
    Claws,
    /// Raptorial talons that penetrate rather than grip, which is why they
    /// scale to bodies no hand could hold up.
    Talons,
    /// Chitinous hooks and adhesive setae — arthropods, geckos.
    Hooks,
}

/// The numbers the derivation rule weighs the roster against.
///
/// Authored rather than compiled in, because these are the knobs a balance pass
/// reaches for — *"bears should top out heavier"*, *"wallrunning is too
/// generous"* — and needing a Rust change and a release to answer that is the
/// thing this table exists to avoid. They live in the same file as the roster
/// so that a row and the rule it is judged by cannot drift into different
/// assets.
///
/// `None`/`Fins`/`Hooves`/`Pads` have no entry: they are not "very heavy", they
/// are *no grip at all*, which is a fact about the limb rather than a number to
/// tune.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraversalTuning {
    /// Heaviest body hands can hold against a wall.
    pub hands_mass_ceiling: f32,
    /// Heaviest body claws can hold. A bear climbs a tree at half a tonne.
    pub claws_mass_ceiling: f32,
    /// Heaviest body that can wallrun. A wallrun carries momentum sideways
    /// across a face, which stops working long before a hang does.
    pub wallrun_mass_limit: f32,
    /// Shortest body for which crouching buys anything.
    pub min_crouch_height: f32,
    /// Shortest body for which lying down buys anything.
    pub min_prone_height: f32,
}

impl TraversalTuning {
    /// The heaviest body this contact structure can hold against a vertical
    /// surface, in kilograms.
    ///
    /// This is the one place the bestiary's physics is allowed to be generous,
    /// and it is deliberately concentrated here rather than scattered as 224
    /// exceptions: a twenty-tonne dragon holds a cliff on talons that sink into
    /// the rock, while a ten-tonne stone construct with hands does not.
    pub fn mass_ceiling(&self, grip: Grip) -> f32 {
        match grip {
            // Nothing to grip with. Not "very heavy" — none.
            Grip::None | Grip::Fins | Grip::Hooves | Grip::Pads => 0.0,
            Grip::Hands => self.hands_mass_ceiling,
            Grip::Claws => self.claws_mass_ceiling,
            // Penetrating or adhesive, so limited by the surface rather than by
            // the grip. Not a tunable: no number would mean anything here.
            Grip::Talons | Grip::Hooks => f32::INFINITY,
        }
    }
}

/// What the body's long axis can do.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub enum Frame {
    /// Does not fold: statues, constructs, armoured shells, and animals too
    /// massive to hold themselves in any posture but standing.
    Rigid,
    /// A spine that bends — the ordinary vertebrate case.
    Flexible,
    /// Long, limbless and laterally compressible.
    Serpentine,
    /// No fixed shape at all.
    Amorphous,
}

/// The authored description of one species' build.
///
/// Every field is required and there is no `Default`, on purpose: a species
/// that nobody described should fail the load by name, not quietly inherit
/// somebody else's body plan.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraversalMorphology {
    pub grip: Grip,
    pub frame: Frame,
    /// Weight-bearing limbs. Zero for a serpent or a fish.
    pub limbs: u8,
}

impl TraversalMorphology {
    /// Turn a build into a set of capabilities.
    ///
    /// The **only** place a traversal capability is decided. Everything else in
    /// the engine asks this, directly or through
    /// [`Body::traversal_capabilities`].
    pub fn derive(
        &self,
        mass_kg: f32,
        height: f32,
        tuning: &TraversalTuning,
    ) -> TraversalCapabilities {
        let mut caps = TraversalCapabilities::empty();

        // Climbing needs three things at once: something to grip with that can
        // bear this much body, at least two limbs to do it with, and a frame
        // that folds against the wall. A rigid body fails the last one however
        // good its claws are, which is why an armoured snapper and a stone
        // construct both stay on the ground.
        if self.frame == Frame::Flexible
            && self.limbs >= 2
            && mass_kg <= tuning.mass_ceiling(self.grip)
        {
            caps |= TraversalCapabilities::CLIMB;
            // Wallrunning is a run, not a hang: upright, and light enough to
            // carry its own momentum across the face.
            if self.limbs == 2 && mass_kg <= tuning.wallrun_mass_limit {
                caps |= TraversalCapabilities::WALLRUN;
            }
        }

        // Posture. Both are pointless on a body already shorter than the gap
        // they would buy, so each is gated on having somewhere to fold to.
        if self.frame == Frame::Flexible && height >= tuning.min_crouch_height {
            caps |= TraversalCapabilities::CROUCH;
        }
        if matches!(
            self.frame,
            Frame::Flexible | Frame::Serpentine | Frame::Amorphous
        ) && height >= tuning.min_prone_height
        {
            caps |= TraversalCapabilities::PRONE;
        }

        // A cross-section with no rigid skeleton holding it open.
        if matches!(self.frame, Frame::Serpentine | Frame::Amorphous) {
            caps |= TraversalCapabilities::SQUEEZE;
        }

        // Legs that push.
        if self.limbs >= 2 && !matches!(self.grip, Grip::None | Grip::Fins) {
            caps |= TraversalCapabilities::JUMP;
        }

        caps
    }
}

/// The authored table, one required row per species.
///
/// Shaped exactly like the per-species `AllSpecies<T>` structs the body modules
/// already define: a **struct** with one field per species, not a map. A RON
/// file of that shape cannot omit a species — serde fails the load, by name, at
/// start-up, for free. Adding a species to the enum adds a field to the struct,
/// and this file stops deserialising until somebody fills it in.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BodyTraversal {
    pub tuning: TraversalTuning,
    pub arthropod: arthropod::AllSpecies<TraversalMorphology>,
    pub biped_large: biped_large::AllSpecies<TraversalMorphology>,
    pub biped_small: biped_small::AllSpecies<TraversalMorphology>,
    pub bird_large: bird_large::AllSpecies<TraversalMorphology>,
    pub bird_medium: bird_medium::AllSpecies<TraversalMorphology>,
    pub crustacean: crustacean::AllSpecies<TraversalMorphology>,
    pub dragon: dragon::AllSpecies<TraversalMorphology>,
    pub fish_medium: fish_medium::AllSpecies<TraversalMorphology>,
    pub fish_small: fish_small::AllSpecies<TraversalMorphology>,
    pub golem: golem::AllSpecies<TraversalMorphology>,
    pub quadruped_low: quadruped_low::AllSpecies<TraversalMorphology>,
    pub quadruped_medium: quadruped_medium::AllSpecies<TraversalMorphology>,
    pub quadruped_small: quadruped_small::AllSpecies<TraversalMorphology>,
    pub theropod: theropod::AllSpecies<TraversalMorphology>,
}

impl BodyTraversal {
    /// The authored build for a body, or `None` for the body kinds that are not
    /// a species roster at all.
    ///
    /// `Humanoid` is absent from the file on purpose and answered by
    /// [`HUMANOID_MORPHOLOGY`]: it is not an `AllSpecies` roster but one body
    /// plan with a continuous scaler, exactly as `Body::mass` already treats
    /// it. `Object`, `Item`, `Ship` and `Plugin` are not creatures.
    pub fn get(&self, body: &Body) -> Option<TraversalMorphology> {
        Some(match body {
            Body::Humanoid(_) => HUMANOID_MORPHOLOGY,
            Body::Arthropod(b) => self.arthropod[&b.species],
            Body::BipedLarge(b) => self.biped_large[&b.species],
            Body::BipedSmall(b) => self.biped_small[&b.species],
            Body::BirdLarge(b) => self.bird_large[&b.species],
            Body::BirdMedium(b) => self.bird_medium[&b.species],
            Body::Crustacean(b) => self.crustacean[&b.species],
            Body::Dragon(b) => self.dragon[&b.species],
            Body::FishMedium(b) => self.fish_medium[&b.species],
            Body::FishSmall(b) => self.fish_small[&b.species],
            Body::Golem(b) => self.golem[&b.species],
            Body::QuadrupedLow(b) => self.quadruped_low[&b.species],
            Body::QuadrupedMedium(b) => self.quadruped_medium[&b.species],
            Body::QuadrupedSmall(b) => self.quadruped_small[&b.species],
            Body::Theropod(b) => self.theropod[&b.species],
            Body::Object(_) | Body::Item(_) | Body::Ship(_) | Body::Plugin(_) => return None,
        })
    }
}

impl Body {
    /// What this body can physically do when it moves through the world.
    ///
    /// Reads the authored build for the species and puts it through
    /// [`TraversalMorphology::derive`] with the body's own mass and height, so
    /// that a single ten-tonne construct and a single house cat are told apart
    /// by the same rule rather than by two lists.
    ///
    /// Body kinds that are not creatures — objects, items, ships, plugin
    /// bodies — have no capabilities at all, which is the correct answer for a
    /// thrown rock.
    ///
    /// Flight and swimming are folded in from [`Body::fly_thrust`] and
    /// [`Body::swim_thrust`] rather than authored, so the table can never
    /// disagree with the physics about whether something flies.
    ///
    /// 🔵 Consumers should read this (or the resolved value an entity carries)
    /// rather than matching on `Body` themselves. It is the seam where a
    /// class-innate or spell-granted capability would later be unioned in, and
    /// a consumer that reaches around it welds that door shut.
    pub fn traversal_capabilities(&self) -> TraversalCapabilities {
        let handle = body_traversal();
        let table = handle.read();
        let mut caps = table
            .0
            .get(self)
            .map(|morph| morph.derive(self.mass().0, self.height(), &table.0.tuning))
            .unwrap_or_else(TraversalCapabilities::empty);

        if self.fly_thrust().is_some() {
            caps |= TraversalCapabilities::FLY;
        }
        if self.swim_thrust().is_some() {
            caps |= TraversalCapabilities::SWIM;
        }
        caps
    }
}

/// The playable body plan: two hands, a spine, and light enough to wallrun.
///
/// Held in code rather than in the table so that the player's capabilities
/// cannot be changed by a content edit; `player_capabilities_are_unchanged`
/// locks the derived result.
pub const HUMANOID_MORPHOLOGY: TraversalMorphology = TraversalMorphology {
    grip: Grip::Hands,
    frame: Frame::Flexible,
    limbs: 2,
};

lazy_static! {
    static ref BODY_TRAVERSAL: AssetHandle<Ron<BodyTraversal>> =
        Ron::<BodyTraversal>::load_expect("common.body_traversal");
}

/// Read the authored table. Hot-reloads in dev builds.
pub fn body_traversal() -> AssetHandle<Ron<BodyTraversal>> { *BODY_TRAVERSAL }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::body::{humanoid, quadruped_low};

    fn caps_of(body: &Body) -> TraversalCapabilities { body.traversal_capabilities() }

    fn morph_of(body: &Body) -> Option<TraversalMorphology> { body_traversal().read().0.get(body) }

    fn humanoid() -> Body {
        Body::Humanoid(humanoid::Body::random_with(
            &mut rand::rng(),
            &humanoid::Species::Human,
        ))
    }

    /// The table must cover every species or the server does not start.
    /// `AllSpecies` makes that structural; this proves the asset is actually
    /// loadable rather than merely well-typed.
    #[test]
    fn every_species_has_an_authored_build() {
        for body in Body::iter() {
            if matches!(
                body,
                Body::Object(_) | Body::Item(_) | Body::Ship(_) | Body::Plugin(_)
            ) {
                assert!(morph_of(&body).is_none(), "{body:?} is not a creature");
            } else {
                assert!(
                    morph_of(&body).is_some(),
                    "{body:?} has no authored morphology",
                );
            }
        }
    }

    /// 🔴 The contract with the player: this must not change what a player
    /// character can do, at all.
    ///
    /// `SWIM` is in the set because humanoids have always had `swim_thrust`;
    /// it is newly *named* here, not newly granted.
    #[test]
    fn player_capabilities_are_unchanged() {
        assert_eq!(
            caps_of(&humanoid()),
            TraversalCapabilities::CLIMB
                | TraversalCapabilities::WALLRUN
                | TraversalCapabilities::CROUCH
                | TraversalCapabilities::PRONE
                | TraversalCapabilities::JUMP
                | TraversalCapabilities::SWIM,
        );
    }

    /// Wallrunning is a strictly harder ask than hanging, so it can never be
    /// granted without climbing.
    #[test]
    fn wallrun_implies_climb() {
        for body in Body::iter() {
            if matches!(body, Body::Plugin(_)) {
                continue;
            }
            let caps = caps_of(&body);
            if caps.contains(TraversalCapabilities::WALLRUN) {
                assert!(
                    caps.contains(TraversalCapabilities::CLIMB),
                    "{body:?} wallruns without climbing",
                );
            }
        }
    }

    /// Flight and swimming stay the engine's answer, never the table's, so the
    /// two cannot drift.
    #[test]
    fn flight_and_swimming_follow_the_engine() {
        for body in Body::iter() {
            if matches!(body, Body::Plugin(_)) {
                continue;
            }
            let caps = caps_of(&body);
            assert_eq!(
                caps.contains(TraversalCapabilities::FLY),
                body.fly_thrust().is_some(),
                "{body:?} FLY disagrees with fly_thrust",
            );
            assert_eq!(
                caps.contains(TraversalCapabilities::SWIM),
                body.swim_thrust().is_some(),
                "{body:?} SWIM disagrees with swim_thrust",
            );
        }
    }

    /// Nothing shipped burrows; the flag exists for the first thing that does.
    #[test]
    fn nothing_burrows_yet() {
        for body in Body::iter() {
            if matches!(body, Body::Plugin(_)) {
                continue;
            }
            assert!(!caps_of(&body).contains(TraversalCapabilities::BURROW));
        }
    }

    /// Dragons, wyverns and drakes grip cliff faces with talon and claw, so
    /// they climb. This falls out of `Grip::Talons` having no practical mass
    /// ceiling — a fact about the creature, not a case keyed off its name.
    #[test]
    fn dragons_wyverns_and_drakes_climb() {
        assert!(
            caps_of(&Body::Dragon(dragon::Body {
                species: dragon::Species::Reddragon,
                body_type: dragon::BodyType::Male,
            }))
            .contains(TraversalCapabilities::CLIMB),
            "a twenty-tonne dragon should still hold a cliff on its talons",
        );
        for species in [
            bird_large::Species::FlameWyvern,
            bird_large::Species::CloudWyvern,
            bird_large::Species::FrostWyvern,
            bird_large::Species::SeaWyvern,
            bird_large::Species::WealdWyvern,
        ] {
            assert!(
                caps_of(&Body::BirdLarge(bird_large::Body {
                    species,
                    body_type: bird_large::BodyType::Male,
                }))
                .contains(TraversalCapabilities::CLIMB),
                "{species:?} should climb",
            );
        }
        for species in [
            quadruped_low::Species::Lavadrake,
            quadruped_low::Species::Icedrake,
            quadruped_low::Species::Mossdrake,
        ] {
            assert!(
                caps_of(&Body::QuadrupedLow(quadruped_low::Body {
                    species,
                    body_type: quadruped_low::BodyType::Male,
                }))
                .contains(TraversalCapabilities::CLIMB),
                "{species:?} should climb",
            );
        }
    }

    /// No golem-bodied creature climbs today — but for its build, not for its
    /// body kind. The eight actual constructs are rigid; `Mogwai` is a fiend
    /// borrowing the same skeleton and is *not* rigid, and it stays off walls
    /// purely on the ten-tonne mass every golem-bodied creature currently
    /// shares. That distinction is the point: if those masses are ever given
    /// real per-species values, the fiend is the one that should start
    /// climbing, and it will, with no engine change.
    #[test]
    fn no_golem_bodied_creature_climbs_and_the_reason_is_its_build() {
        for species in golem::ALL_SPECIES {
            let body = Body::Golem(golem::Body {
                species,
                body_type: golem::BodyType::Male,
            });
            assert!(
                !caps_of(&body).contains(TraversalCapabilities::CLIMB),
                "{species:?} should not climb",
            );
            let morph = morph_of(&body).unwrap();
            if species == golem::Species::Mogwai {
                assert_eq!(morph.frame, Frame::Flexible, "the fiend bends");
                assert!(
                    caps_of(&body).contains(TraversalCapabilities::CROUCH),
                    "…and therefore has a posture, unlike the constructs",
                );
            } else {
                assert_eq!(morph.frame, Frame::Rigid, "{species:?} should be rigid");
                assert!(
                    !caps_of(&body)
                        .intersects(TraversalCapabilities::CROUCH | TraversalCapabilities::PRONE),
                    "{species:?} should have no posture",
                );
            }
        }
    }

    /// 🔴 The regression lock on future construct archetypes: a creature
    /// described as clawed, flexible and four-legged must inherit climbing from
    /// the same rule every other creature uses, with **no engine change and no
    /// second decision**.
    ///
    /// If this test ever has to be edited to keep passing, the indirection this
    /// module exists for has been lost.
    #[test]
    fn traversal_rule_admits_a_future_clawed_construct() {
        let clawed_construct = TraversalMorphology {
            grip: Grip::Claws,
            frame: Frame::Flexible,
            limbs: 4,
        };
        let handle = body_traversal();
        let table = handle.read();
        let tuning = &table.0.tuning;
        assert!(
            clawed_construct
                .derive(800.0, 2.4, tuning)
                .contains(TraversalCapabilities::CLIMB),
        );
        // …and the shipped humanoid-shaped construct still does not, purely on
        // its build.
        let stone_construct = TraversalMorphology {
            grip: Grip::Hands,
            frame: Frame::Rigid,
            limbs: 2,
        };
        assert!(
            !stone_construct
                .derive(10_000.0, 4.0, tuning)
                .contains(TraversalCapabilities::CLIMB),
        );
    }

    /// Serpents squeeze and do not climb, whatever they have at the front.
    #[test]
    fn a_serpentine_frame_squeezes_instead_of_climbing() {
        for body in Body::iter() {
            if matches!(body, Body::Plugin(_)) {
                continue;
            }
            let Some(morph) = morph_of(&body) else {
                continue;
            };
            if matches!(morph.frame, Frame::Serpentine | Frame::Amorphous) {
                let caps = caps_of(&body);
                assert!(
                    caps.contains(TraversalCapabilities::SQUEEZE)
                        && !caps.contains(TraversalCapabilities::CLIMB),
                    "{body:?} is serpentine/amorphous but its capabilities disagree",
                );
            }
        }
    }

    /// A rigid body never folds, so it never gains a posture — the property
    /// that keeps armoured and constructed creatures out of the crouch work.
    #[test]
    fn a_rigid_frame_has_no_posture() {
        for body in Body::iter() {
            if matches!(body, Body::Plugin(_)) {
                continue;
            }
            let Some(morph) = morph_of(&body) else {
                continue;
            };
            if morph.frame == Frame::Rigid {
                assert!(
                    !caps_of(&body)
                        .intersects(TraversalCapabilities::CROUCH | TraversalCapabilities::PRONE),
                    "{body:?} is rigid but has a posture",
                );
            }
        }
    }

    /// A census, printed rather than asserted on an exact number: the point of
    /// a derivation rule is that the roster's shape follows from it, so pinning
    /// the count would just be a second place to edit. What is asserted is that
    /// the answer is neither "nobody" (the bug this replaces) nor "everybody"
    /// (which would make the rule decorative).
    #[test]
    fn the_climbing_roster_is_a_real_subset() {
        let (mut climbers, mut total) = (0, 0);
        for body in Body::iter() {
            if morph_of(&body).is_none() {
                continue;
            }
            total += 1;
            if caps_of(&body).contains(TraversalCapabilities::CLIMB) {
                climbers += 1;
            }
        }
        println!("{climbers} of {total} species climb");
        assert!(climbers > 0, "the whole point is that somebody climbs");
        assert!(
            climbers * 4 < total * 3,
            "if three quarters of the bestiary climbs, the rule is not doing any work",
        );
    }

    /// The bulky `BipedLarge` are rigid and the agile ones are not — the split
    /// that keeps a lumbering troll on the ground while a werewolf goes up.
    #[test]
    fn biped_large_splits_by_build_not_by_size() {
        let climbs = |species| {
            caps_of(&Body::BipedLarge(biped_large::Body {
                species,
                body_type: biped_large::BodyType::Male,
            }))
            .contains(TraversalCapabilities::CLIMB)
        };
        for species in [
            biped_large::Species::Werewolf,
            biped_large::Species::Minotaur,
            biped_large::Species::Yeti,
            biped_large::Species::Wendigo,
            biped_large::Species::Strigoi,
        ] {
            assert!(climbs(species), "{species:?} should climb");
        }
        for species in [
            biped_large::Species::Cyclops,
            biped_large::Species::Cavetroll,
            biped_large::Species::Forgemaster,
            biped_large::Species::TerracottaBesieger,
        ] {
            assert!(!climbs(species), "{species:?} should not climb");
        }
    }
}
