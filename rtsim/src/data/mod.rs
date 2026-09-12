pub mod actor;
pub mod airship;
pub mod architect;
pub mod banished;
pub mod faction;
pub mod nature;
pub mod quest;
pub mod report;
pub mod sentiment;
pub mod site;
pub mod undercompact_gate;

pub use self::{
    actor::{Actor, Actors},
    banished::{BanishedCreature, BanishedKind, Banishments},
    faction::{Faction, FactionId, Factions},
    nature::Nature,
    quest::Quests,
    report::{Report, ReportId, ReportKind, Reports},
    sentiment::{Sentiment, Sentiments},
    site::{Site, SiteId, Sites},
    undercompact_gate::{UndercompactGateLever, UndercompactGateLevers},
};
use airship::AirshipSim;
use architect::Architect;
use common::{resources::TimeOfDay, rtsim::ActorId, terrain::TerrainOverrides};
use enum_map::{EnumArray, EnumMap, enum_map};
use serde::{Deserialize, Serialize, de, ser};
use std::{
    cmp::PartialEq,
    fmt,
    io::{Read, Write},
    marker::PhantomData,
};

/// The current version of rtsim data.
///
/// Note that this number does *not* need incrementing on every change: most
/// field removals/additions are fine. This number should only be incremented
/// when we wish to perform a *hard purge* of rtsim data.
pub const CURRENT_VERSION: u32 = 11;

#[derive(Clone, Serialize, Deserialize)]
pub struct Data {
    // Absence of field just implied version = 0
    #[serde(default)]
    pub version: u32,

    pub nature: Nature,
    #[serde(default)]
    pub actors: Actors,
    #[serde(default)]
    pub sites: Sites,
    #[serde(default)]
    pub factions: Factions,
    #[serde(default)]
    pub reports: Reports,
    #[serde(default)]
    pub architect: Architect,
    #[serde(default)]
    pub quests: Quests,
    /// Creatures temporarily removed from the world by a banishment effect,
    /// due to return at their own wall-clock deadline. Additive
    /// `#[serde(default)]` field: per `CURRENT_VERSION`'s doc comment above,
    /// this needs **no** version bump — an older save simply loads with an
    /// empty registry.
    #[serde(default)]
    pub banished: Banishments,

    /// COW-7b: which of the Undercompact gate antechamber's two vault
    /// levers have been pulled, and whether the gate has been solved.
    /// Additive `#[serde(default)]` field, same convention as `banished`
    /// above -- an older save simply loads with an unsolved, empty state.
    #[serde(default)]
    pub undercompact_gate: UndercompactGateLevers,

    /// Active regional terrain overrides (temperature/humidity events,
    /// craters, authored bespoke biome events, etc. -- see
    /// `common::terrain::regional_override`) that should survive a server
    /// restart. Additive `#[serde(default)]` field, same convention as
    /// `banished`/`undercompact_gate` above -- an older save simply loads
    /// with no active overrides. Ephemeral overrides (`ephemeral: true`,
    /// e.g. the `/terrain_override` admin command) are never written here --
    /// see `server/src/terrain_override.rs::apply`.
    #[serde(default)]
    pub terrain_overrides: TerrainOverrides,

    #[serde(default)]
    pub tick: u64,
    #[serde(default)]
    pub time_of_day: TimeOfDay,

    // If true, rtsim data will be ignored (and, hence, overwritten on next save) on load.
    #[serde(default)]
    pub should_purge: bool,

    #[serde(skip)]
    pub airship_sim: AirshipSim,
}

pub enum ReadError {
    Load(rmp_serde::decode::Error),
    // Preserve old data
    VersionMismatch(Box<Data>),
}

impl fmt::Debug for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Load(err) => err.fmt(f),
            Self::VersionMismatch(_) => write!(f, "VersionMismatch"),
        }
    }
}

pub type WriteError = rmp_serde::encode::Error;

impl Data {
    pub fn spawn_actor(&mut self, actor: Actor) -> ActorId {
        let home = actor.home;
        let id = self.actors.create_actor(actor);
        if let Some(home) = home.and_then(|home| self.sites.get_mut(home)) {
            home.population.insert(id);
        }
        id
    }

    pub fn from_reader<R: Read>(reader: R) -> Result<Box<Self>, ReadError> {
        rmp_serde::decode::from_read(reader)
            .map_err(ReadError::Load)
            .and_then(|data: Data| {
                if data.version == CURRENT_VERSION {
                    Ok(Box::new(data))
                } else {
                    Err(ReadError::VersionMismatch(Box::new(data)))
                }
            })
    }

    pub fn write_to<W: Write>(&self, mut writer: W) -> Result<(), WriteError> {
        rmp_serde::encode::write_named(&mut writer, self)
    }

    /// Perform whatever initial preparation is required for rtsim data to be
    /// ready for simulation.
    ///
    /// This might include populating caches, normalising data, etc.
    pub fn prepare(&mut self) {
        self.quests.prepare();
        self.banished.prepare();
    }
}

fn rugged_ser_enum_map<
    K: EnumArray<V> + Serialize,
    V: From<i16> + PartialEq + Serialize,
    S: ser::Serializer,
    const DEFAULT: i16,
>(
    map: &EnumMap<K, V>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.collect_map(map.iter().filter(|(_, v)| v != &&V::from(DEFAULT)))
}

fn rugged_de_enum_map<
    'a,
    K: EnumArray<V> + EnumArray<Option<V>> + Deserialize<'a>,
    V: From<i16> + Deserialize<'a>,
    D: de::Deserializer<'a>,
    const DEFAULT: i16,
>(
    de: D,
) -> Result<EnumMap<K, V>, D::Error> {
    struct Visitor<K, V, const DEFAULT: i16>(PhantomData<(K, V)>);

    impl<'de, K, V, const DEFAULT: i16> de::Visitor<'de> for Visitor<K, V, DEFAULT>
    where
        K: EnumArray<V> + EnumArray<Option<V>> + Deserialize<'de>,
        V: From<i16> + Deserialize<'de>,
    {
        type Value = EnumMap<K, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "a map")
        }

        fn visit_map<M: de::MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
            let mut entries = EnumMap::default();
            while let Some((key, value)) = access.next_entry()? {
                entries[key] = Some(value);
            }
            Ok(enum_map! { key => entries[key].take().unwrap_or_else(|| V::from(DEFAULT)) })
        }
    }

    de.deserialize_map(Visitor::<_, _, DEFAULT>(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::grid::Grid;
    use vek::Vec2;

    /// The exact wire shape of an rtsim save written *before* `banished`
    /// existed: every field `Data` has today except that one, in order, named
    /// the same way `write_named` names them.
    #[derive(Serialize)]
    struct PreBanishmentData {
        version: u32,
        nature: Nature,
        actors: Actors,
        sites: Sites,
        factions: Factions,
        reports: Reports,
        architect: Architect,
        quests: Quests,
        tick: u64,
        time_of_day: TimeOfDay,
        should_purge: bool,
    }

    /// `banished` is an additive `#[serde(default)]` field, which is why
    /// `CURRENT_VERSION` does **not** move for it (see the constant's own doc
    /// comment, and `quests`, which was added the same way). This pins that
    /// claim end to end: a payload missing the key entirely must still load
    /// through the real MessagePack codec, at the *unchanged* version, and
    /// come up with an empty registry. It fails loudly if anyone drops the
    /// `#[serde(default)]`.
    #[test]
    fn a_save_written_before_the_banishment_registry_still_loads_at_the_same_version() {
        let old = PreBanishmentData {
            version: CURRENT_VERSION,
            nature: Nature {
                chunks: Grid::populate_from(Vec2::new(1, 1), |_| nature::Chunk {
                    res: Default::default(),
                }),
            },
            actors: Default::default(),
            sites: Default::default(),
            factions: Default::default(),
            reports: Default::default(),
            architect: Default::default(),
            quests: Default::default(),
            tick: 7,
            time_of_day: TimeOfDay(1234.0),
            should_purge: false,
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old).expect("serialise the old save");

        let data = Data::from_reader(&encoded[..]).expect("an old save must still load");
        assert_eq!(data.tick, 7);
        assert!(data.banished.is_empty());
    }

    /// The exact wire shape of an rtsim save written *before*
    /// `undercompact_gate` existed (COW-7b): every field `Data` has today
    /// except that one.
    #[derive(Serialize)]
    struct PreUndercompactGateData {
        version: u32,
        nature: Nature,
        actors: Actors,
        sites: Sites,
        factions: Factions,
        reports: Reports,
        architect: Architect,
        quests: Quests,
        banished: Banishments,
        tick: u64,
        time_of_day: TimeOfDay,
        should_purge: bool,
    }

    /// `undercompact_gate` is an additive `#[serde(default)]` field, same
    /// convention as `banished` above, so `CURRENT_VERSION` does not move
    /// for it either. This pins that a save missing the key entirely still
    /// loads through the real MessagePack codec, at the unchanged version,
    /// with the puzzle unsolved.
    #[test]
    fn a_save_written_before_the_undercompact_gate_registry_still_loads_at_the_same_version() {
        let old = PreUndercompactGateData {
            version: CURRENT_VERSION,
            nature: Nature {
                chunks: Grid::populate_from(Vec2::new(1, 1), |_| nature::Chunk {
                    res: Default::default(),
                }),
            },
            actors: Default::default(),
            sites: Default::default(),
            factions: Default::default(),
            reports: Default::default(),
            architect: Default::default(),
            quests: Default::default(),
            banished: Default::default(),
            tick: 11,
            time_of_day: TimeOfDay(4321.0),
            should_purge: false,
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old).expect("serialise the old save");

        let data = Data::from_reader(&encoded[..]).expect("an old save must still load");
        assert_eq!(data.tick, 11);
        assert!(!data.undercompact_gate.is_solved());
    }

    /// The exact wire shape of an rtsim save written *before*
    /// `terrain_overrides` existed: every field `Data` has today except that
    /// one.
    #[derive(Serialize)]
    struct PreTerrainOverridesData {
        version: u32,
        nature: Nature,
        actors: Actors,
        sites: Sites,
        factions: Factions,
        reports: Reports,
        architect: Architect,
        quests: Quests,
        banished: Banishments,
        undercompact_gate: UndercompactGateLevers,
        tick: u64,
        time_of_day: TimeOfDay,
        should_purge: bool,
    }

    /// `terrain_overrides` is an additive `#[serde(default)]` field, same
    /// convention as `banished`/`undercompact_gate` above, so
    /// `CURRENT_VERSION` does not move for it either. This pins that a save
    /// missing the key entirely still loads through the real MessagePack
    /// codec, at the unchanged version, with no active overrides.
    #[test]
    fn a_save_written_before_terrain_overrides_existed_still_loads_at_the_same_version() {
        let old = PreTerrainOverridesData {
            version: CURRENT_VERSION,
            nature: Nature {
                chunks: Grid::populate_from(Vec2::new(1, 1), |_| nature::Chunk {
                    res: Default::default(),
                }),
            },
            actors: Default::default(),
            sites: Default::default(),
            factions: Default::default(),
            reports: Default::default(),
            architect: Default::default(),
            quests: Default::default(),
            banished: Default::default(),
            undercompact_gate: Default::default(),
            tick: 13,
            time_of_day: TimeOfDay(2222.0),
            should_purge: false,
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old).expect("serialise the old save");

        let data = Data::from_reader(&encoded[..]).expect("an old save must still load");
        assert_eq!(data.tick, 13);
        assert!(data.terrain_overrides.active.is_empty());
    }

    /// `TerrainOverridePayload::Damage` is a brand new enum variant added
    /// alongside terrain-damage healing -- `TerrainOverrides` itself already
    /// loads fine on an old save (see the test above), but adding a variant
    /// to something already serialized deserves its own dedicated check.
    ///
    /// This encodes with a LOCAL, deliberately old-shaped payload enum that
    /// has ONLY the `Climate` variant `TerrainOverridePayload` had before
    /// `Damage` was added -- not the current (already-`Damage`-aware) enum
    /// -- then decodes those bytes with the REAL, current
    /// `TerrainOverrides`/`TerrainOverridePayload` type. Encoding and
    /// decoding with the same (current) enum would only prove "this
    /// round-trips today", which would pass identically even if `Damage`
    /// had been inserted BEFORE `Climate` (which, given `rmp_serde`'s
    /// index-based externally-tagged enum encoding, would have silently
    /// broken every pre-existing save's `Climate` overrides) -- so this
    /// mirrors `PreTerrainOverridesData`'s own old-wire-shape approach
    /// above, applied to an enum variant rather than a struct field.
    #[test]
    fn a_climate_only_override_list_written_before_the_damage_payload_existed_still_loads() {
        use common::terrain::{ClimateOverride, ClimateValue, OverrideRegion, TerrainOverrideId};

        /// The exact wire shape of `TerrainOverridePayload` before `Damage`
        /// was added -- `Climate` only, at the same variant position (index
        /// 0) it holds in the real, current enum.
        #[derive(Serialize)]
        enum OldTerrainOverridePayload {
            Climate(ClimateOverride),
        }

        #[derive(Serialize)]
        struct OldRegionalTerrainOverride {
            id: TerrainOverrideId,
            region: OverrideRegion,
            payload: OldTerrainOverridePayload,
            priority: i32,
            activated_at: f64,
            wipe_player_edits: bool,
            ephemeral: bool,
        }

        #[derive(Serialize)]
        struct OldTerrainOverrides {
            version: u64,
            active: Vec<OldRegionalTerrainOverride>,
        }

        let old = OldTerrainOverrides {
            version: 3,
            active: vec![OldRegionalTerrainOverride {
                id: TerrainOverrideId(7),
                region: OverrideRegion::Circle {
                    center: Vec2::new(10, 20),
                    radius: 50.0,
                    edge: 8.0,
                },
                payload: OldTerrainOverridePayload::Climate(ClimateOverride {
                    temp: Some(ClimateValue::Set(-5.0)),
                    humidity: None,
                    tree_density_mul: None,
                }),
                priority: 5,
                activated_at: 0.0,
                wipe_player_edits: false,
                ephemeral: false,
            }],
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old)
            .expect("serialise a climate-only override list using the OLD (pre-Damage) enum");

        let decoded: TerrainOverrides = rmp_serde::decode::from_read(&encoded[..])
            .expect("a climate-only override list written before Damage existed must still load");
        assert_eq!(decoded.version, old.version);
        assert_eq!(decoded.active.len(), 1);
        let climate = decoded.active[0]
            .climate()
            .expect("must decode back to a Climate payload, not silently become something else");
        assert_eq!(climate.temp, Some(ClimateValue::Set(-5.0)));
        assert!(climate.humidity.is_none());
    }

    /// A save containing a `Damage`-payload override (the new payload kind
    /// added alongside terrain-damage healing) must round-trip through the
    /// same MessagePack codec real rtsim saves use.
    #[test]
    fn a_damage_payload_override_round_trips_through_the_real_codec() {
        use common::terrain::{
            DamageOverride, DamageShape, OverrideRegion, RegionalTerrainOverride,
            TerrainOverrideId, TerrainOverridePayload,
        };

        let overrides = TerrainOverrides {
            version: 9,
            active: vec![RegionalTerrainOverride {
                id: TerrainOverrideId(42),
                region: OverrideRegion::Circle {
                    center: Vec2::new(-30, 40),
                    radius: 24.0,
                    edge: 6.0,
                },
                payload: TerrainOverridePayload::Damage(DamageOverride {
                    shapes: vec![
                        DamageShape::Crater {
                            max_depth: 12.0,
                            rim_height: 2.0,
                        },
                        DamageShape::Debris {
                            rubble_density: 0.2,
                            felled_tree_chance: 0.05,
                        },
                    ],
                    scorch: 0.8,
                    vegetation_mul: 0.1,
                    heal_progress: 0.25,
                    heal_stages: 4,
                    heal_interval: 600.0,
                    next_heal_at: 1234.5,
                }),
                priority: 50,
                activated_at: 10.0,
                wipe_player_edits: true,
                ephemeral: false,
                transition: Default::default(),
            }],
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &overrides)
            .expect("serialise a damage override");

        let decoded: TerrainOverrides = rmp_serde::decode::from_read(&encoded[..])
            .expect("a damage-payload override must load through the real codec");
        assert_eq!(decoded, overrides);
    }

    /// `TerrainOverridePayload::BiomeProfile` is a brand new enum variant
    /// (a third payload kind, alongside `Climate` and `Damage`) -- same
    /// risk class the test above already pinned for `Damage`, applied to
    /// this variant too.
    ///
    /// This encodes with a LOCAL, deliberately old-shaped payload enum that
    /// has ONLY the `Climate`/`Damage` variants `TerrainOverridePayload` had
    /// before `BiomeProfile` was added, AT THE SAME VARIANT POSITIONS (index
    /// 0/1) they hold in the real, current enum -- not the current
    /// (already-`BiomeProfile`-aware) enum -- then decodes those bytes with
    /// the REAL, current `TerrainOverrides`/`TerrainOverridePayload` type.
    /// Encoding and decoding with the same (current) enum would only prove
    /// "this round-trips today", which would pass identically even if
    /// `BiomeProfile` had been inserted BEFORE `Climate`/`Damage` (which,
    /// given `rmp_serde`'s index-based externally-tagged enum encoding,
    /// would have silently broken every pre-existing save's `Climate`/
    /// `Damage` overrides) -- mirrors
    /// `a_climate_only_override_list_written_before_the_damage_payload_existed_still_loads`'s
    /// own old-wire-shape approach above.
    #[test]
    fn a_climate_and_damage_only_override_list_written_before_the_biome_profile_payload_existed_still_loads()
     {
        use common::terrain::{
            ClimateOverride, ClimateValue, DamageOverride, DamageShape, OverrideRegion,
            TerrainOverrideId,
        };

        /// The exact wire shape of `TerrainOverridePayload` before
        /// `BiomeProfile` was added -- `Climate`/`Damage` only, at the same
        /// variant positions (index 0/1) they hold in the real, current
        /// enum.
        #[derive(Serialize)]
        enum OldTerrainOverridePayload {
            Climate(ClimateOverride),
            Damage(DamageOverride),
        }

        #[derive(Serialize)]
        struct OldRegionalTerrainOverride {
            id: TerrainOverrideId,
            region: OverrideRegion,
            payload: OldTerrainOverridePayload,
            priority: i32,
            activated_at: f64,
            wipe_player_edits: bool,
            ephemeral: bool,
        }

        #[derive(Serialize)]
        struct OldTerrainOverrides {
            version: u64,
            active: Vec<OldRegionalTerrainOverride>,
        }

        let old = OldTerrainOverrides {
            version: 4,
            active: vec![
                OldRegionalTerrainOverride {
                    id: TerrainOverrideId(7),
                    region: OverrideRegion::Circle {
                        center: Vec2::new(10, 20),
                        radius: 50.0,
                        edge: 8.0,
                    },
                    payload: OldTerrainOverridePayload::Climate(ClimateOverride {
                        temp: Some(ClimateValue::Set(-5.0)),
                        humidity: None,
                        tree_density_mul: None,
                    }),
                    priority: 5,
                    activated_at: 0.0,
                    wipe_player_edits: false,
                    ephemeral: false,
                },
                OldRegionalTerrainOverride {
                    id: TerrainOverrideId(8),
                    region: OverrideRegion::Circle {
                        center: Vec2::new(-30, 40),
                        radius: 24.0,
                        edge: 6.0,
                    },
                    payload: OldTerrainOverridePayload::Damage(DamageOverride {
                        shapes: vec![DamageShape::Crater {
                            max_depth: 12.0,
                            rim_height: 2.0,
                        }],
                        scorch: 0.8,
                        vegetation_mul: 0.1,
                        heal_progress: 0.25,
                        heal_stages: 4,
                        heal_interval: 600.0,
                        next_heal_at: 1234.5,
                    }),
                    priority: 50,
                    activated_at: 10.0,
                    wipe_player_edits: true,
                    ephemeral: false,
                },
            ],
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old).expect(
            "serialise a climate+damage override list using the OLD (pre-BiomeProfile) enum",
        );

        let decoded: TerrainOverrides = rmp_serde::decode::from_read(&encoded[..]).expect(
            "a climate+damage override list written before BiomeProfile existed must still load",
        );
        assert_eq!(decoded.version, old.version);
        assert_eq!(decoded.active.len(), 2);
        let climate = decoded.active[0]
            .climate()
            .expect("must decode back to a Climate payload, not silently become something else");
        assert_eq!(climate.temp, Some(ClimateValue::Set(-5.0)));
        assert!(climate.humidity.is_none());
        let damage = decoded.active[1]
            .damage()
            .expect("must decode back to a Damage payload, not silently become something else");
        assert_eq!(damage.scorch, 0.8);
    }

    /// A save containing a `BiomeProfile`-payload override (the new payload
    /// kind this test module's own sibling above pins the save-compat story
    /// for) must round-trip through the same MessagePack codec real rtsim
    /// saves use.
    #[test]
    fn a_biome_profile_payload_override_round_trips_through_the_real_codec() {
        use common::terrain::{
            BiomeProfileOverride, OverrideRegion, RegionalTerrainOverride, TerrainOverrideId,
            TerrainOverridePayload,
        };

        let overrides = TerrainOverrides {
            version: 11,
            active: vec![RegionalTerrainOverride {
                id: TerrainOverrideId(99),
                region: OverrideRegion::Circle {
                    center: Vec2::new(15, -25),
                    radius: 40.0,
                    edge: 10.0,
                },
                payload: TerrainOverridePayload::BiomeProfile(BiomeProfileOverride {
                    profile: "swamp_dark_01".to_string(),
                    intensity: 0.75,
                    flood_to: Some(123.5),
                }),
                priority: 60,
                activated_at: 20.0,
                wipe_player_edits: false,
                ephemeral: false,
                transition: Default::default(),
            }],
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &overrides)
            .expect("serialise a biome-profile override");

        let decoded: TerrainOverrides = rmp_serde::decode::from_read(&encoded[..])
            .expect("a biome-profile-payload override must load through the real codec");
        assert_eq!(decoded, overrides);
    }

    /// `transition` is an additive `#[serde(default)]` field on
    /// `RegionalTerrainOverride` -- a save written before it existed must
    /// still load, falling back to an empty `TransitionNarrative` (both
    /// fields `None`, the deterministic-fallback-text path).
    #[test]
    fn an_override_written_before_transition_existed_still_loads() {
        use common::terrain::{
            ClimateOverride, ClimateValue, OverrideRegion, TerrainOverrideId,
            TerrainOverridePayload,
        };

        #[derive(Serialize)]
        struct OldRegionalTerrainOverride {
            id: TerrainOverrideId,
            region: OverrideRegion,
            payload: TerrainOverridePayload,
            priority: i32,
            activated_at: f64,
            wipe_player_edits: bool,
            ephemeral: bool,
        }

        #[derive(Serialize)]
        struct OldTerrainOverrides {
            version: u64,
            active: Vec<OldRegionalTerrainOverride>,
        }

        let old = OldTerrainOverrides {
            version: 12,
            active: vec![OldRegionalTerrainOverride {
                id: TerrainOverrideId(123),
                region: OverrideRegion::Circle {
                    center: Vec2::new(5, -5),
                    radius: 16.0,
                    edge: 4.0,
                },
                payload: TerrainOverridePayload::Climate(ClimateOverride {
                    temp: Some(ClimateValue::Set(2.0)),
                    humidity: None,
                    tree_density_mul: None,
                }),
                priority: 10,
                activated_at: 0.0,
                wipe_player_edits: false,
                ephemeral: false,
            }],
        };

        let mut encoded = Vec::new();
        rmp_serde::encode::write_named(&mut encoded, &old)
            .expect("serialise an override list using the OLD (pre-transition) shape");

        let decoded: TerrainOverrides = rmp_serde::decode::from_read(&encoded[..])
            .expect("an override list written before `transition` existed must still load");
        assert_eq!(decoded.version, old.version);
        assert_eq!(decoded.active.len(), 1);
        assert_eq!(decoded.active[0].transition, Default::default());
    }
}
