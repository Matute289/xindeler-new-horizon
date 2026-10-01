mod adlet;
mod airship_dock;
mod barn;
mod bridge;
mod building;
mod camp;
mod castle;
mod citadel;
mod cliff_tower;
mod cliff_town_airship_dock;
mod coastal_airship_dock;
mod coastal_house;
mod coastal_workshop;
mod cultist;
mod desert_city_airship_dock;
mod desert_city_arena;
mod desert_city_multiplot;
mod desert_city_temple;
mod dwarven_mine;
mod farm_field;
mod fortification;
mod giant_tree;
mod glider_finish;
mod glider_platform;
mod glider_ring;
mod gnarling;
mod haniwa;
mod house;
mod jungle_ruin;
mod myrmidon_arena;
mod myrmidon_house;
mod naval_port;
mod pirate_hideout;
mod plaza;
mod road;
mod rock_circle;
mod sahagin;
mod savannah_airship_dock;
mod savannah_guard_hut;
mod savannah_hut;
mod savannah_workshop;
mod sea_chapel;
mod shipyard;
pub mod tavern;
mod terracotta_house;
mod terracotta_palace;
mod terracotta_yard;
mod troll_cave;
mod vampire_castle;
mod workshop;

pub use self::{
    adlet::AdletStronghold,
    airship_dock::AirshipDock,
    barn::Barn,
    bridge::Bridge,
    building::Building,
    camp::Camp,
    castle::Castle,
    citadel::Citadel,
    cliff_tower::CliffTower,
    cliff_town_airship_dock::CliffTownAirshipDock,
    coastal_airship_dock::CoastalAirshipDock,
    coastal_house::CoastalHouse,
    coastal_workshop::CoastalWorkshop,
    cultist::Cultist,
    desert_city_airship_dock::DesertCityAirshipDock,
    desert_city_arena::DesertCityArena,
    desert_city_multiplot::DesertCityMultiPlot,
    desert_city_temple::DesertCityTemple,
    dwarven_mine::DwarvenMine,
    farm_field::FarmField,
    fortification::Fortification,
    giant_tree::GiantTree,
    glider_finish::GliderFinish,
    glider_platform::GliderPlatform,
    glider_ring::GliderRing,
    gnarling::GnarlingFortification,
    haniwa::Haniwa,
    house::House,
    jungle_ruin::JungleRuin,
    myrmidon_arena::MyrmidonArena,
    myrmidon_house::MyrmidonHouse,
    naval_port::{NavalPort, PortDressing},
    pirate_hideout::PirateHideout,
    plaza::Plaza,
    road::{Road, RoadKind, RoadLights, RoadMaterial},
    rock_circle::RockCircle,
    sahagin::Sahagin,
    savannah_airship_dock::SavannahAirshipDock,
    savannah_guard_hut::SavannahGuardHut,
    savannah_hut::SavannahHut,
    savannah_workshop::SavannahWorkshop,
    sea_chapel::SeaChapel,
    shipyard::Shipyard,
    tavern::Tavern,
    terracotta_house::TerracottaHouse,
    terracotta_palace::TerracottaPalace,
    terracotta_yard::TerracottaYard,
    troll_cave::TrollCave,
    vampire_castle::VampireCastle,
    workshop::Workshop,
};

use super::*;
use crate::{ColumnSample, util::DHashSet};
use common::{match_some, path::Path};
use rand_chacha::ChaCha8Rng;
use vek::*;

pub struct Plot {
    pub(crate) kind: PlotKind,
    pub(crate) root_tile: Vec2<i32>,
    pub(crate) tiles: DHashSet<Vec2<i32>>,
}

impl Plot {
    pub fn find_bounds(&self) -> Aabr<i32> {
        self.tiles
            .iter()
            .fold(Aabr::new_empty(self.root_tile), |b, t| {
                b.expanded_to_contain_point(*t)
            })
    }

    pub fn z_range(&self) -> Option<Range<i32>> {
        match_some!(&self.kind, PlotKind::House(house) => house.z_range())
    }

    pub fn kind(&self) -> &PlotKind { &self.kind }

    pub fn root_tile(&self) -> Vec2<i32> { self.root_tile }

    pub fn tiles(&self) -> impl ExactSizeIterator<Item = Vec2<i32>> + '_ {
        self.tiles.iter().copied()
    }

    pub fn is_house(&self) -> bool {
        // TODO: Better than this
        self.door_tile().is_some()
    }

    pub fn is_workshop(&self) -> bool {
        // TODO: Better than this
        matches!(
            &self.kind,
            PlotKind::Workshop(_) | PlotKind::CoastalWorkshop(_) | PlotKind::SavannahWorkshop(_)
        )
    }
}

#[derive(strum::Display)]
pub enum PlotKind {
    House(House),
    AirshipDock(AirshipDock),
    GliderRing(GliderRing),
    GliderPlatform(GliderPlatform),
    GliderFinish(GliderFinish),
    Tavern(Tavern),
    CoastalAirshipDock(CoastalAirshipDock),
    CoastalHouse(CoastalHouse),
    CoastalWorkshop(CoastalWorkshop),
    Workshop(Workshop),
    DesertCityMultiPlot(DesertCityMultiPlot),
    DesertCityTemple(DesertCityTemple),
    DesertCityArena(DesertCityArena),
    DesertCityAirshipDock(DesertCityAirshipDock),
    SeaChapel(SeaChapel),
    JungleRuin(JungleRuin),
    Plaza(Plaza),
    Castle(Castle),
    Cultist(Cultist),
    Road(Road),
    Gnarling(GnarlingFortification),
    Adlet(AdletStronghold),
    Haniwa(Haniwa),
    GiantTree(GiantTree),
    CliffTower(CliffTower),
    CliffTownAirshipDock(CliffTownAirshipDock),
    Sahagin(Sahagin),
    Citadel(Citadel),
    SavannahAirshipDock(SavannahAirshipDock),
    SavannahGuardHut(SavannahGuardHut),
    SavannahHut(SavannahHut),
    SavannahWorkshop(SavannahWorkshop),
    Barn(Barn),
    Bridge(Bridge),
    Fortification(Fortification),
    PirateHideout(PirateHideout),
    RockCircle(RockCircle),
    TrollCave(TrollCave),
    Camp(Camp),
    DwarvenMine(DwarvenMine),
    TerracottaPalace(TerracottaPalace),
    TerracottaHouse(TerracottaHouse),
    TerracottaYard(TerracottaYard),
    FarmField(FarmField),
    VampireCastle(VampireCastle),
    MyrmidonArena(MyrmidonArena),
    MyrmidonHouse(MyrmidonHouse),
    Building(Building),
    NavalPort(NavalPort),
    Shipyard(Shipyard),
}

/// # Syntax
/// ```ignore
/// foreach_plot!(expr, plot => plot.something())
/// ```
#[macro_export]
macro_rules! foreach_plot {
    ($p:expr, $x:ident => $y:expr $(,)?) => {
        match $p {
            PlotKind::House($x) => $y,
            PlotKind::AirshipDock($x) => $y,
            PlotKind::CoastalAirshipDock($x) => $y,
            PlotKind::CoastalHouse($x) => $y,
            PlotKind::CoastalWorkshop($x) => $y,
            PlotKind::Workshop($x) => $y,
            PlotKind::DesertCityAirshipDock($x) => $y,
            PlotKind::DesertCityMultiPlot($x) => $y,
            PlotKind::DesertCityTemple($x) => $y,
            PlotKind::DesertCityArena($x) => $y,
            PlotKind::SeaChapel($x) => $y,
            PlotKind::JungleRuin($x) => $y,
            PlotKind::Plaza($x) => $y,
            PlotKind::Castle($x) => $y,
            PlotKind::Road($x) => $y,
            PlotKind::Gnarling($x) => $y,
            PlotKind::Adlet($x) => $y,
            PlotKind::GiantTree($x) => $y,
            PlotKind::CliffTower($x) => $y,
            PlotKind::CliffTownAirshipDock($x) => $y,
            PlotKind::Citadel($x) => $y,
            PlotKind::SavannahAirshipDock($x) => $y,
            PlotKind::SavannahGuardHut($x) => $y,
            PlotKind::SavannahHut($x) => $y,
            PlotKind::SavannahWorkshop($x) => $y,
            PlotKind::Barn($x) => $y,
            PlotKind::Bridge($x) => $y,
            PlotKind::Fortification($x) => $y,
            PlotKind::PirateHideout($x) => $y,
            PlotKind::Tavern($x) => $y,
            PlotKind::Cultist($x) => $y,
            PlotKind::Haniwa($x) => $y,
            PlotKind::Sahagin($x) => $y,
            PlotKind::RockCircle($x) => $y,
            PlotKind::TrollCave($x) => $y,
            PlotKind::Camp($x) => $y,
            PlotKind::DwarvenMine($x) => $y,
            PlotKind::TerracottaPalace($x) => $y,
            PlotKind::TerracottaHouse($x) => $y,
            PlotKind::TerracottaYard($x) => $y,
            PlotKind::FarmField($x) => $y,
            PlotKind::VampireCastle($x) => $y,
            PlotKind::GliderRing($x) => $y,
            PlotKind::GliderPlatform($x) => $y,
            PlotKind::GliderFinish($x) => $y,
            PlotKind::MyrmidonArena($x) => $y,
            PlotKind::MyrmidonHouse($x) => $y,
            PlotKind::Building($x) => $y,
            PlotKind::NavalPort($x) => $y,
            PlotKind::Shipyard($x) => $y,
        }
    };
}

pub use foreach_plot;

impl Structure for Plot {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_plot")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_plot"))
    }

    fn render_inner(&self, site: &Site, land: &Land, painter: &Painter) {
        foreach_plot!(&self.kind, plot => plot.render(site, land, painter))
    }

    fn spawn_rules_inner(
        &self,
        spawn_rules: &mut SpawnRules,
        land: &Land,
        wpos: Vec2<i32>,
        weight: f32,
    ) {
        foreach_plot!(&self.kind, plot => plot.spawn_rules(spawn_rules, land, wpos, weight))
    }

    fn rel_terrain_offset(&self, col: &ColumnSample) -> i32 {
        foreach_plot!(&self.kind, plot => plot.rel_terrain_offset(col))
    }

    fn terrain_surface_at_inner(
        &self,
        wpos: Vec2<i32>,
        old: Block,
        rng: &mut ChaCha8Rng,
        col: &ColumnSample,
        z_off: i32,
        site: &Site,
    ) -> Option<Block> {
        foreach_plot!(&self.kind, plot => plot.terrain_surface_at(wpos, old, rng, col, z_off, site))
    }

    fn airship_dock_info(&self) -> Option<AirshipDockInfo<'_>> {
        foreach_plot!(&self.kind, plot => plot.airship_dock_info())
    }

    fn naval_dock_info(&self) -> Option<NavalDockInfo<'_>> {
        foreach_plot!(&self.kind, plot => plot.naval_dock_info())
    }

    fn door_tile(&self) -> Option<Vec2<i32>> { foreach_plot!(&self.kind, plot => plot.door_tile()) }

    fn render_ordering(&self) -> u32 { foreach_plot!(&self.kind, plot => plot.render_ordering()) }
}

pub struct AirshipDockInfo<'plot> {
    pub door_tile: Vec2<i32>,
    pub center: Vec2<i32>,
    pub docking_positions: &'plot [Vec3<i32>],
}

/// Which hull size a [`Berth`] or [`Anchorage`] is built for (spec §4.3's two
/// berth classes, derived from the two merchant hulls COW-24 `[Q4]` confirmed
/// -- `SailBoat` 12×32×6, `Galleon` 14×48×10, `common/src/comp/body/ship.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BerthClass {
    /// `SailBoat`-sized: 6-tile quay face, 3 blocks minimum depth.
    Small,
    /// `Galleon`-sized: 9-tile quay face, 6 blocks minimum depth.
    Large,
}

/// Which side of the hull faces the quay/pier/finger face when moored --
/// decides which rail the gangway hangs off. The mapping of `Port`/
/// `Starboard` onto a tier's two deck edges is
/// [`NavalPort`](super::plot::NavalPort)'s own implementation detail; a
/// consumer should treat the two variants as opaque, stable-per-berth
/// labels rather than a compass direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BerthSide {
    Port,
    Starboard,
}

/// An addressable mooring slot alongside a naval port's quay, pier or jetty
/// face (spec §5.2 -- every field's reasoning lives there).
#[derive(Debug, Clone, Copy)]
pub struct Berth {
    /// Local to the port that built it -- [`crate::civ::naval_berths::
    /// all_naval_berths`] is the only producer of a globally unique id (see
    /// its own doc comment for why this field cannot carry one at plot
    /// construction time: nothing at that point knows how many berths every
    /// *other* port in the world already claimed). Stable per port for an
    /// unchanged world seed; **not** stable across a change to port
    /// generation.
    pub id: u32,
    pub class: BerthClass,
    /// Hull centre when moored: x/y at the water surface, z = water_alt.
    pub mooring_pos: Vec3<i32>,
    /// Unit direction the bow points when moored (along the quay face).
    pub heading: Vec2<f32>,
    /// Which side of the hull faces the deck -- decides which rail gets the
    /// gangway.
    pub side: BerthSide,
    /// A walkable deck block flush with the hull's rail. Guaranteed
    /// adjacent to `mooring_pos` horizontally.
    pub gangway: Vec3<i32>,
    /// Offshore waypoint from which `mooring_pos` is reachable in a straight
    /// line over water only.
    pub approach: Vec2<i32>,
    /// Measured water depth at `mooring_pos`, in blocks. Recorded, not
    /// assumed, so a berth whose real depth disagrees with its tier is a
    /// loud, testable inconsistency instead of a hull on the seabed.
    pub depth: i32,
}

/// An offshore mooring for a hull too large for any berth the port itself
/// offers (spec §11.2) -- today, emitted only where a `Jetty`'s single
/// `Small` berth would otherwise cap a route to `SailBoat`.
///
/// A **separate type from [`Berth`]**, deliberately: it carries no gangway,
/// heading or side, so a consumer cannot mistake it for a quayside slot and
/// try to walk crew off a hull sitting in open water.
#[derive(Debug, Clone, Copy)]
pub struct Anchorage {
    /// See [`Berth::id`] -- same provisional-then-global id scheme, same
    /// producer.
    pub id: u32,
    /// Always `Large`: an anchorage exists so a `Galleon` can call where no
    /// `Large` berth does.
    pub class: BerthClass,
    /// Hull centre at anchor: x/y at the water surface, z = water_alt.
    pub pos: Vec3<i32>,
    /// Blocks of clear water around `pos` the hull swings through on its
    /// cable.
    pub swing_radius: i32,
    /// Measured water depth at `pos`, in blocks.
    pub depth: i32,
    /// Offshore waypoint from which `pos` is reachable over water only.
    pub approach: Vec2<i32>,
    /// The local id (see [`Berth::id`]) of this port's own `Small` berth --
    /// the jetty a tender shuttles cargo/passengers to and from.
    pub tender_berth: u32,
}

/// The berth/anchorage discovery hook (spec §5.1). Mirrors
/// [`AirshipDockInfo`] exactly -- forwarded through every plot kind by the
/// same [`foreach_plot!`] idiom and defaulted to `None` by
/// [`crate::site::generation::Structure::naval_dock_info`] -- because
/// `desert_city_airship_dock.rs` (spec §2.5) is the proof that a structure
/// whose discovery hook is never implemented looks finished and is invisible
/// to everything downstream.
pub struct NavalDockInfo<'plot> {
    /// Landward entry on the apron's road-facing edge -- where an NPC walks
    /// in.
    pub door_tile: Vec2<i32>,
    /// The apron/deck hinge, in the plot's own world-space blocks -- the
    /// port's identity for logging and map marking.
    pub center: Vec2<i32>,
    pub class: PortClass,
    pub berths: &'plot [Berth],
    /// Empty for every tier but `Jetty` (spec §11.2).
    pub anchorages: &'plot [Anchorage],
}

/// Builds the sprite fill for a dungeon's key-gated keyhole: it opts out of
/// ranged/keyless unlocking (e.g. the `knock` spell) via `no_knock`, so a
/// dungeon whose whole point is requiring the matching key item can't be
/// bypassed remotely — melee key-item unlocking is unaffected. See
/// `common::event::RemoteUnlockEvent` / `SpriteCfg::no_knock` for the
/// mechanism this backs, and `Haniwa::render_inner` for the original
/// precedent every dungeon's key gate should share.
pub fn locked_dungeon_keyhole(kind: common::terrain::sprite::SpriteKind) -> Fill {
    Fill::sprite_ori_cfg(kind, 0, common::terrain::sprite::SpriteCfg {
        no_knock: true,
        ..Default::default()
    })
}

#[cfg(test)]
mod key_gate_tests {
    use super::*;
    use common::terrain::sprite::SpriteKind;

    /// Every dungeon-type keyhole sprite placed directly by a generator's
    /// own `render_inner` (as opposed to via a prefab, see below) must be
    /// built through `locked_dungeon_keyhole` so its `knock`-immunity can
    /// never silently regress per-dungeon. One `SpriteKind` per Cromatolis
    /// dungeon type that gates progression behind a physical key this way.
    ///
    /// Gnarling has no lockable-door mechanism in this codebase at all.
    /// DwarvenMine's key gates DO exist — its `forgemaster_boss`,
    /// `forgemaster_room`, `entrance`, `hallway`, `hallway2`,
    /// `mining_site`, `excavation_site` and `cleansing_room` prefabs all
    /// place `Keyhole`/`KeyholeBars` — but the whole dungeon is
    /// prefab-driven (`render_prefab`, not a literal `Fill::sprite_ori_cfg`
    /// call here), so its `no_knock` guarantee instead comes from
    /// `block::keyhole_cfg` (see `block::keyhole_cfg_tests`), which also
    /// backs `SpriteKind::Keyhole` here — DwarvenMine's prefabs and
    /// Cultist Sanctum's literal call both resolve to that same sprite
    /// kind, just through two different code paths.
    const DUNGEON_KEY_GATE_SPRITES: &[SpriteKind] = &[
        SpriteKind::HaniwaKeyhole,     // Claybound Ossuary (SiteKind::Haniwa)
        SpriteKind::SahaginKeyhole,    // Sahagin Island (SiteKind::Sahagin)
        SpriteKind::VampireKeyhole,    // Vampire Castle (SiteKind::VampireCastle)
        SpriteKind::TerracottaKeyhole, // Terracotta Palace (SiteKind::Terracotta)
        SpriteKind::MyrmidonKeyhole,   // Myrmidon Arena (SiteKind::Myrmidon)
        SpriteKind::MinotaurKeyhole,   // Myrmidon Arena's Minotaur vault (SiteKind::Myrmidon)
        SpriteKind::Keyhole,           // Cultist Sanctum; also DwarvenMine (prefab)
        SpriteKind::BoneKeyhole,       // Adlet Stronghold (SiteKind::Adlet)
        SpriteKind::GlassKeyhole,      // Sea Chapel (SiteKind::ChapelSite)
    ];

    #[test]
    fn all_dungeon_key_gates_are_no_knock() {
        for &kind in DUNGEON_KEY_GATE_SPRITES {
            match locked_dungeon_keyhole(kind) {
                Fill::CfgSprite(block, cfg) => {
                    assert!(
                        cfg.no_knock,
                        "{kind:?}'s dungeon key gate must set no_knock, so the knock spell can't \
                         bypass it"
                    );
                    assert_eq!(
                        block.get_sprite(),
                        Some(kind),
                        "locked_dungeon_keyhole must place the requested sprite kind"
                    );
                },
                _ => panic!(
                    "{kind:?}: expected locked_dungeon_keyhole to build a Fill::CfgSprite \
                     carrying the keyhole's SpriteCfg"
                ),
            }
        }
    }

    /// Guards against a dungeon's key gate quietly regressing back to a raw
    /// `Fill::Block`/`Fill::sprite_ori_cfg` call that bypasses
    /// `locked_dungeon_keyhole` (and so loses `no_knock`) — every dungeon
    /// type's own generator source must actually call the shared helper for
    /// its keyhole sprite kind.
    #[test]
    fn dungeon_generators_use_the_locked_keyhole_helper() {
        let sources: &[(&str, &str)] = &[
            (include_str!("haniwa.rs"), "HaniwaKeyhole"),
            (include_str!("sahagin.rs"), "SahaginKeyhole"),
            (include_str!("vampire_castle.rs"), "VampireKeyhole"),
            (include_str!("terracotta_palace.rs"), "TerracottaKeyhole"),
            (include_str!("myrmidon_arena.rs"), "MyrmidonKeyhole"),
            (include_str!("myrmidon_arena.rs"), "MinotaurKeyhole"),
            (include_str!("cultist.rs"), "Keyhole"),
            (include_str!("adlet.rs"), "BoneKeyhole"),
            (include_str!("sea_chapel.rs"), "GlassKeyhole"),
        ];

        for (src, variant) in sources {
            let needle = format!("locked_dungeon_keyhole(SpriteKind::{variant})");
            assert!(
                src.contains(&needle),
                "expected to find `{needle}` in the dungeon generator source — every dungeon's \
                 key gate must be built via the shared no_knock-setting helper"
            );
        }
    }
}
