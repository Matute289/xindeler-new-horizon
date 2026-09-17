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
    /// See `site::Site::is_authored_settlement`. A genuine wild procedural
    /// `Camp` spawns `Alignment::Enemy` pirate NPCs in a tropical biome
    /// (see `render_inner` below); an authored settlement (inn/post) reusing
    /// this generator as a physical stand-in must never spawn hostile NPCs,
    /// regardless of local temperature, so it always falls back to the
    /// peaceful village-aligned NPC set instead.
    is_authored_settlement: bool,
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
            is_authored_settlement: site.is_authored_settlement,
        }
    }
}

impl Structure for Camp {
    #[cfg(feature = "dyn-lib")]
    #[unsafe(export_name = "as_dyn_structure_camp")]
    fn as_dyn_outer(&self) -> Option<(&dyn Structure, &'static str)> {
        Some((Self::as_dyn_impl(self), "as_dyn_structure_camp"))
    }

    fn render_inner(&self, _site: &Site, land: &Land, painter: &Painter) {
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
        // An authored settlement (inn/post) reusing this generator never
        // spawns the hostile pirate NPC set, even in a tropical biome where
        // a genuine wild camp would -- see the `is_authored_settlement`
        // field doc.
        match camp_type {
            CampType::Pirate if !self.is_authored_settlement => {
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
