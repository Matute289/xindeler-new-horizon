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

    fn door_tile(&self) -> Option<Vec2<i32>> { foreach_plot!(&self.kind, plot => plot.door_tile()) }

    fn render_ordering(&self) -> u32 { foreach_plot!(&self.kind, plot => plot.render_ordering()) }
}

pub struct AirshipDockInfo<'plot> {
    pub door_tile: Vec2<i32>,
    pub center: Vec2<i32>,
    pub docking_positions: &'plot [Vec3<i32>],
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
