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

pub use self::{
    actor::{Actor, Actors},
    banished::{BanishedCreature, BanishedKind, Banishments},
    faction::{Faction, FactionId, Factions},
    nature::Nature,
    quest::Quests,
    report::{Report, ReportId, ReportKind, Reports},
    sentiment::{Sentiment, Sentiments},
    site::{Site, SiteId, Sites},
};
use airship::AirshipSim;
use architect::Architect;
use common::{resources::TimeOfDay, rtsim::ActorId};
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
}
