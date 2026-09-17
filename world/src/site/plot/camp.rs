use super::*;
use crate::{Land, assets::AssetHandle, site::generation::PrimitiveTransform};
use common::{
    generation::EntityInfo,
    terrain::{Structure as PrefabStructure, StructuresGroup},
};
use lazy_static::lazy_static;

use rand::prelude::*;
use vek::*;

pub struct Camp {
    bounds: Aabr<i32>,
    pub(crate) alt: i32,
    temp: f32,
}

#[derive(Copy, Clone)]
enum CampType {
    Pirate,
    Snow,
    Forest,
}

impl Camp {
    pub fn generate(
        land: &Land,
        _rng: &mut impl Rng,
        site: &Site,
        tile_aabr: Aabr<i32>,
        site_temp: f32,
    ) -> Self {
        let bounds = Aabr {
            min: site.tile_wpos(tile_aabr.min),
            max: site.tile_wpos(tile_aabr.max),
        };
        let temp = site_temp;
        Self {
            bounds,
            alt: land.get_alt_approx(site.tile_center_wpos(tile_aabr.center())) as i32 + 2,
            temp,
        }
    }
}

impl Structure for Camp {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_camp")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_camp"))
    }

    fn render_inner(&self, site: &Site, land: &Land, painter: &Painter) {
        let center = self.bounds.center();
        let base = land.get_alt_approx(center) as i32;
        let mut rng = rand::rng();
        let model_pos = center.with_z(base);
        let temp = self.temp;
        let camp_type = if temp >= CONFIG.tropical_temp {
            CampType::Pirate
        } else if temp <= (CONFIG.snow_temp) {
            CampType::Snow
        } else {
            CampType::Forest
        };
        // models
        lazy_static! {
            pub static ref MODEL_PIRATE: AssetHandle<StructuresGroup> =
                PrefabStructure::load_group("site_structures.camp.camp_pirate");
            pub static ref MODEL_SNOW: AssetHandle<StructuresGroup> =
                PrefabStructure::load_group("site_structures.camp.camp_snow");
            pub static ref MODEL_FOREST: AssetHandle<StructuresGroup> =
                PrefabStructure::load_group("site_structures.camp.camp_forest");
        }
        let prefab_structure = match camp_type {
            CampType::Pirate => MODEL_PIRATE.read(),
            CampType::Snow => MODEL_SNOW.read(),
            CampType::Forest => MODEL_FOREST.read(),
        }[0]
        .clone();

        painter
            .prim(Primitive::Prefab(Box::new(prefab_structure.clone())))
            .translate(model_pos)
            .fill(Fill::Prefab(Box::new(prefab_structure), model_pos, 0));

        // npcs
        let npc_rng = rng.random_range(1..=5);
        // A genuine wild procedural `Camp` spawns `Alignment::Enemy` pirate
        // NPCs in a tropical biome. An authored settlement (inn/post)
        // reusing this generator as a physical stand-in -- see
        // `site::Site::is_authored_settlement` -- must never spawn hostile
        // NPCs, regardless of local temperature, so it always falls back to
        // the peaceful village-aligned NPC set instead. Read directly off
        // `site` (not cached on `Camp` at generation time): `Site` is only
        // tagged `is_authored_settlement` by `civ::Site::generate` *after*
        // this plot has already been built (see
        // `establish_authored_cromatolis_settlements`), so a value captured
        // during `Camp::generate` would always observe the pre-tag default
        // of `false`.
        match camp_type {
            CampType::Pirate if !site.is_authored_settlement => {
                for p in 0..npc_rng {
                    painter.spawn(
                        EntityInfo::at((center + p).with_z(base + 2).as_()).with_asset_expect(
                            "common.entity.spot.pirate",
                            &mut rng,
                            None,
                        ),
                    )
                }
                let pet = if npc_rng < 3 {
                    "common.entity.wild.peaceful.parrot"
                } else {
                    "common.entity.wild.peaceful.rat"
                };
                painter.spawn(
                    EntityInfo::at(center.with_z(base + 2).as_())
                        .with_asset_expect(pet, &mut rng, None),
                )
            },
            _ => {
                if npc_rng > 2 {
                    painter.spawn(
                        EntityInfo::at((center - 1).with_z(base + 2).as_()).with_asset_expect(
                            "common.entity.village.bowman",
                            &mut rng,
                            None,
                        ),
                    );
                }
                if npc_rng < 4 {
                    painter.spawn(
                        EntityInfo::at((center + 1).with_z(base + 2).as_()).with_asset_expect(
                            "common.entity.village.skinner",
                            &mut rng,
                            None,
                        ),
                    )
                }
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::generation::Painter;
    use common::comp::agent::Alignment;

    /// Regression for the bug fixed alongside this test: `Camp` used to
    /// cache `is_authored_settlement` on itself at `generate()` time, but
    /// `civ::Site::generate` only tags a `Site` as an authored settlement
    /// *after* its plots (this one included) have already been built --
    /// see `establish_authored_cromatolis_settlements` in `civ/mod.rs`. A
    /// value cached that early always observed the pre-tag default of
    /// `false`, so an authored Cromatolis inn/post sitting in a tropical
    /// chunk still spawned real `Alignment::Enemy` pirate NPCs even after
    /// rtsim's separate faction-classification fix landed. `render_inner`
    /// must read `is_authored_settlement` off the `&Site` parameter it is
    /// given at render time (by which point the real tag is set), not off
    /// `self`.
    ///
    /// Deliberately mirrors the real production ordering rather than just
    /// exercising the fixed code's final contract: `Camp::generate` always
    /// runs against an *untagged* `Site` (matching `generate_camp`'s local
    /// `Site::default()`), and the authored tag is only applied afterward,
    /// to the separate `Site` value passed to `render_inner` (matching
    /// `with_authored_settlement` being called on the already-built site).
    /// Tagging before `generate()` instead would make this pass even
    /// against the old, buggy cached-field code, since that code only ever
    /// went wrong when the tag arrived *after* generation.
    fn render_camp(is_authored_settlement: bool, tropical: bool) -> Vec<EntityInfo> {
        let land = Land::empty();
        let untagged_site = Site::default();
        let aabr = Aabr {
            min: Vec2::new(-8, -8),
            max: Vec2::new(8, 8),
        };
        let site_temp = if tropical { CONFIG.tropical_temp } else { 0.0 };
        let camp = Camp::generate(&land, &mut rand::rng(), &untagged_site, aabr, site_temp);
        let render_site = Site {
            is_authored_settlement,
            ..Site::default()
        };
        let painter = Painter::new_for_test(Aabr {
            min: Vec2::new(-64, -64),
            max: Vec2::new(64, 64),
        });
        camp.render_inner(&render_site, &land, &painter);
        painter.spawned_entities_for_test()
    }

    #[test]
    fn authored_settlement_never_spawns_hostile_npcs_even_in_a_tropical_chunk() {
        let entities = render_camp(true, true);
        assert!(
            !entities.is_empty(),
            "an authored Camp stand-in must still spawn its peaceful NPC set"
        );
        assert!(
            entities.iter().all(|e| e.alignment != Alignment::Enemy),
            "an authored settlement (inn/post) reusing the Camp generator must never spawn an \
             Alignment::Enemy NPC, even in a tropical chunk where a genuine wild camp would -- \
             got: {:?}",
            entities.iter().map(|e| e.alignment).collect::<Vec<_>>()
        );
    }

    #[test]
    fn genuine_wild_camp_still_spawns_hostile_pirates_in_a_tropical_chunk() {
        let entities = render_camp(false, true);
        assert!(
            entities.iter().any(|e| e.alignment == Alignment::Enemy),
            "a genuine wild procedural camp in a tropical chunk must be unaffected by this fix \
             and still spawn its hostile pirate NPC set"
        );
    }
}
