use crate::{
    CONFIG, Canvas, ColumnSample,
    layer::{
        rock_traversal,
        traversal::{Accommodation, SolidVolume},
    },
    util::{
        NEIGHBORS, NEIGHBORS3, RandomField, Sampler, StructureGen2d, UnitChooser,
        gen_cache::StructureGenCache, seed_expan,
    },
};
use common::terrain::{Block, BlockKind};
use ordered_float::NotNan;
use rand::prelude::*;
use rand_chacha::ChaChaRng;
use vek::*;

pub(crate) struct Rock {
    pub(crate) wpos: Vec3<i32>,
    seed: u32,
    units: Vec2<Vec2<i32>>,
    kind: RockKind,
    /// Set when this rock would seal a passage it lands in: the per-column
    /// air to open so the passage stays traversable. `None` for the
    /// overwhelming majority of rocks, which land nowhere near a void.
    pub(crate) accommodation: Option<Accommodation>,
}

/// The rock, if any, the global structure lattice places at `wpos`.
///
/// The RNG draws and their order are what decide every rock in the world, so
/// this is the one place that makes them: a second copy of the cascade
/// elsewhere would silently describe a different world the day someone tunes
/// the density or adds a kind.
pub(crate) fn rock_at(wpos: Vec2<i32>, seed: u32, col: &ColumnSample) -> Option<Rock> {
    let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(seed));

    const BASE_ROCK_DENSITY: f64 = 0.15;
    if !(rng.random_bool((BASE_ROCK_DENSITY * col.rock_density as f64).clamped(0.0, 1.0))
        && col.path.is_none_or(|(d, _, _, _)| d > 6.0))
    {
        return None;
    }

    let kind = match (
        (col.alt - CONFIG.sea_level) as i32,
        (col.alt - col.water_level) as i32,
        col.water_dist.map_or(i32::MAX, |d| d as i32),
    ) {
        (-3..=2, _, _) => {
            if rng.random_bool(0.3) {
                Some(RockKind::Rauk(Pillar::generate(&mut rng)))
            } else {
                Some(RockKind::Rock(VoronoiCell::generate(
                    rng.random_range(1.0..3.0),
                    &mut rng,
                )))
            }
        },
        (_, -15..=3, _) => Some(RockKind::Rock(VoronoiCell::generate(
            rng.random_range(1.0..4.0),
            &mut rng,
        ))),
        (5..=i32::MAX, _, 0..=i32::MAX) => {
            if col.temp > CONFIG.desert_temp - 0.1 && col.humidity < CONFIG.desert_hum + 0.1 {
                Some(RockKind::Sandstone(VoronoiCell::generate(
                    rng.random_range(2.0..20.0 - 10.0 * col.tree_density),
                    &mut rng,
                )))
            } else {
                Some(RockKind::Rock(VoronoiCell::generate(
                    rng.random_range(2.0..20.0 - 10.0 * col.tree_density),
                    &mut rng,
                )))
            }
        },
        _ => None,
    }?;

    Some(Rock {
        wpos: wpos.with_z(col.alt as i32),
        seed,
        units: UnitChooser::new(seed).get(seed).into(),
        kind,
        accommodation: None,
    })
}

pub fn apply_rocks_to(canvas: &mut Canvas, _dynamic_rng: &mut impl Rng) {
    let mut rock_gen = StructureGenCache::new(StructureGen2d::new(canvas.index().seed, 24, 10));

    let info = canvas.info();
    let repair_traversal = rock_traversal::repair_enabled(&info);
    canvas.foreach_col(|canvas, wpos2d, col| {
        let rocks = rock_gen.get(wpos2d, |wpos, seed| {
            let rock = rock_at(wpos, seed, info.col_or_gen(wpos)?.as_ref())?;
            // Keeping a rock out of a passage it would seal, and working out
            // what to open around one it merely crowds, happens *after* the
            // rock itself is decided -- so the RNG stream is untouched and
            // every rock that lands nowhere near a void is bit-identical to
            // before.
            if repair_traversal {
                rock_traversal::accommodate(rock, &info)
            } else {
                Some(rock)
            }
        });

        for rock in rocks {
            let bounds = rock.kind.get_bounds();

            let rpos2d = (wpos2d - rock.wpos.xy())
                .map2(rock.units, |p, unit| unit * p)
                .sum();

            if Aabr::from(bounds).contains_point(rpos2d) {
                let mut is_top = true;
                let mut last_block = Block::empty();
                for z in (bounds.min.z..bounds.max.z).rev() {
                    let wpos = Vec3::new(wpos2d.x, wpos2d.y, rock.wpos.z + z);
                    let model_pos = (wpos - rock.wpos)
                        .xy()
                        .map2(rock.units, |rpos, unit| unit * rpos)
                        .sum()
                        .with_z(wpos.z - rock.wpos.z);

                    rock.kind
                        .take_sample(model_pos, rock.seed, last_block, col)
                        .map(|block| {
                            if col.snow_cover && is_top && block.is_filled() {
                                canvas.set(
                                    wpos + Vec3::unit_z(),
                                    Block::new(BlockKind::Snow, Rgb::new(210, 210, 255)),
                                );
                            }
                            canvas.set(wpos, block);
                            is_top = false;
                            last_block = block;
                        });
                }
            }

            // Post-stamp accommodation: open this column's slice of the
            // repair, if the rock needed one. Deliberately outside the
            // footprint test above --- the repair reaches a few blocks past
            // the rock's own outline, so the columns that need it most are
            // exactly the ones the stamp skips.
            //
            // `apply_rocks_to` runs after every carve, so this is the only
            // place the repair can go. The write guards on `is_filled()` so
            // that sprites -- authored minerals, most of all -- survive: a
            // sprite is not a filled block. That guard is also why plain
            // `map` is right here rather than `map_resource`: this only ever
            // clears solid rock, and never writes a block that could carry a
            // resource.
            if let Some(acc) = &rock.accommodation {
                for &(lo, hi) in acc.carve_at(wpos2d) {
                    for z in lo..=hi {
                        canvas.map(wpos2d.with_z(z), |b| {
                            if b.is_filled() { Block::empty() } else { b }
                        });
                    }
                }
            }
        }
    });
}

impl Rock {
    /// This rock's nominal size parameter, for reporting only.
    #[cfg(test)]
    pub(crate) fn nominal_size(&self) -> f32 { self.kind.nominal_size() }

    /// This rock's bounding box in world space, in the same half-open sense
    /// the stamping loop uses.
    pub(crate) fn world_bounds(&self) -> Aabb<i32> {
        let bounds = self.kind.get_bounds();
        // The stamp walks model space and maps each column through `units`,
        // a signed axis permutation, so the world-space footprint is the
        // model box with its x/y extents possibly swapped. Taking the
        // element-wise max of both axes covers either orientation without
        // having to invert the permutation.
        let extent = Vec3::new(
            bounds.max.x.max(bounds.max.y),
            bounds.max.x.max(bounds.max.y),
            bounds.max.z,
        );
        let min_extent = Vec3::new(
            bounds.min.x.min(bounds.min.y),
            bounds.min.x.min(bounds.min.y),
            bounds.min.z,
        );
        Aabb {
            min: self.wpos + min_extent,
            max: self.wpos + extent,
        }
    }
}

impl SolidVolume for Rock {
    fn bounds(&self) -> Aabb<i32> { self.world_bounds() }

    fn column_solid(&self, wpos2d: Vec2<i32>, z_lo: i32, z_hi: i32, out: &mut Vec<bool>) {
        let model_bounds = self.kind.get_bounds();
        let rpos2d = (wpos2d - self.wpos.xy())
            .map2(self.units, |p, unit| unit * p)
            .sum();
        if !Aabr::from(model_bounds).contains_point(rpos2d) {
            out.extend(std::iter::repeat_n(
                false,
                (z_hi - z_lo + 1).max(0) as usize,
            ));
            return;
        }
        let base = out.len();
        out.extend(std::iter::repeat_n(
            false,
            (z_hi - z_lo + 1).max(0) as usize,
        ));
        // Where solidity depends on a top-down scan of the whole column ---
        // a pillar's de-floating rule reads the block above --- the scan has
        // to start at the model's own top, or the answer would depend on
        // which slice was asked for. Where it does not, sampling only the
        // asked-for slice is the same answer for a fraction of the work, and
        // a band is usually a small part of a rock's z extent.
        let from_z = if self.kind.needs_column_scan() {
            model_bounds.max.z - 1
        } else {
            (z_hi - self.wpos.z).min(model_bounds.max.z - 1)
        };
        let to_z = if self.kind.needs_column_scan() {
            model_bounds.min.z
        } else {
            (z_lo - self.wpos.z).max(model_bounds.min.z)
        };
        let mut last_filled = false;
        for z in (to_z..=from_z).rev() {
            let model_pos = rpos2d.with_z(z);
            let solid = self.kind.solid_at(model_pos, self.seed, last_filled);
            if solid {
                let wz = self.wpos.z + z;
                if wz >= z_lo && wz <= z_hi {
                    out[base + (wz - z_lo) as usize] = true;
                }
                last_filled = true;
            }
        }
    }
}

struct VoronoiCell {
    size: f32,
    points: [Vec3<f32>; 26],
}

impl VoronoiCell {
    fn generate(size: f32, rng: &mut impl Rng) -> Self {
        let mut points = [Vec3::zero(); 26];
        for (i, p) in NEIGHBORS3.iter().enumerate() {
            points[i] = p.as_() * size
                + Vec3::new(
                    rng.random_range(-0.5..=0.5) * size,
                    rng.random_range(-0.5..=0.5) * size,
                    rng.random_range(-0.5..=0.5) * size,
                );
        }
        Self { size, points }
    }

    fn sample_at(&self, rpos: Vec3<i32>) -> bool {
        let rposf = rpos.as_();
        // Would theoretically only need to compare with 7 other points rather than 26,
        // by checking all the points in the cells touching the closest corner of this
        // point.
        rposf.magnitude_squared()
            <= *(0..26)
                .map(|i| self.points[i].distance_squared(rposf))
                .map(|d| NotNan::new(d).unwrap())
                .min()
                .unwrap()
    }
}

struct Pillar {
    height: f32,
    max_extent: Vec2<f32>,
    extents: [Vec2<f32>; 3],
}

impl Pillar {
    fn generate(rng: &mut impl Rng) -> Self {
        let extents = [
            Vec2::new(rng.random_range(0.5..1.5), rng.random_range(0.5..1.5)),
            Vec2::new(rng.random_range(0.8..2.8), rng.random_range(0.8..2.8)),
            Vec2::new(rng.random_range(0.5..1.5), rng.random_range(0.5..3.5)),
        ];
        Self {
            height: rng.random_range(6.0..16.0),
            extents,
            max_extent: extents
                .iter()
                .cloned()
                .reduce(|accum, item| accum.map2(item, |a, b| a.max(b)))
                .unwrap(),
        }
    }

    fn sample_at(&self, rpos: Vec3<i32>) -> bool {
        let h = rpos.z as f32 / self.height;
        let extent = if h < 0.0 {
            self.extents[0] * (-h).max(1.0)
        } else if h < 0.5 {
            self.extents[0].map2(self.extents[1], |l, m| f32::lerp(l, m, h * 2.0))
        } else if h < 1.0 {
            self.extents[1].map2(self.extents[2], |m, t| f32::lerp(m, t, (h - 0.5) * 2.0))
        } else {
            self.extents[2]
        };
        h < 1.0
            && extent
                .map2(rpos.xy(), |e, p| p.abs() < e.ceil() as i32)
                .reduce_and()
    }
}

enum RockKind {
    // A normal rock with a size
    Rock(VoronoiCell),
    Sandstone(VoronoiCell),
    Rauk(Pillar),
    // Arch,
    // Hoodoos,
}

impl RockKind {
    fn take_sample(
        &self,
        rpos: Vec3<i32>,
        seed: u32,
        last_block: Block,
        col: &ColumnSample,
    ) -> Option<Block> {
        // Used to debug get_bounds
        /*
        let bounds = self.get_bounds();
        if rpos
            .map3(
                bounds.min,
                bounds.max,
                |e, a, b| if e == a || e == b { 1 } else { 0 },
            )
            .sum()
            >= 2
        {
            return Some(Block::new(BlockKind::Rock, Rgb::red()));
        }
        */

        if !self.solid_at(rpos, seed, last_block.is_filled()) {
            return None;
        }

        Some(match self {
            RockKind::Rock(cell) => {
                let mossiness =
                    0.1 + RandomField::new(seed).get_f32(Vec3::zero()) * 0.3 + col.humidity * 0.9;
                if last_block.is_filled()
                    || (rpos.z as f32 / cell.size + RandomField::new(seed).get_f32(rpos) * 0.3
                        > mossiness)
                {
                    let mut i = 0;
                    Block::new(
                        BlockKind::WeakRock,
                        col.stone_col.map(|c| {
                            i += 1;
                            c + RandomField::new(seed).get(rpos) as u8 % 10
                        }),
                    )
                } else {
                    Block::new(
                        BlockKind::Grass,
                        col.surface_color.map(|e| (e * 255.0) as u8),
                    )
                }
            },
            RockKind::Sandstone(cell) => {
                let sandiness = 0.3 + RandomField::new(seed).get_f32(Vec3::zero()) * 0.4;
                if last_block.is_filled()
                    || (rpos.z as f32 / cell.size + RandomField::new(seed).get_f32(rpos) * 0.3
                        > sandiness)
                {
                    let mut i = 0;
                    Block::new(
                        BlockKind::WeakRock,
                        Rgb::new(220, 160, 100).map(|c| {
                            i += 1;
                            c + RandomField::new(seed + i).get(Vec2::zero().with_z(rpos.z)) as u8
                                % 30
                        }),
                    )
                } else {
                    Block::new(
                        BlockKind::Grass,
                        col.surface_color.map(|e| (e * 255.0) as u8),
                    )
                }
            },
            RockKind::Rauk(_) => Block::new(
                BlockKind::WeakRock,
                Rgb::new(
                    190 + RandomField::new(seed + 1).get(rpos) as u8 % 10,
                    190 + RandomField::new(seed + 2).get(rpos) as u8 % 10,
                    190 + RandomField::new(seed + 3).get(rpos) as u8 % 10,
                ),
            ),
        })
    }

    /// Whether this kind's solidity at one voxel depends on what lies above
    /// it in the same column --- the de-floating rule a pillar applies. When
    /// it does not, a caller may sample any `z` slice on its own.
    fn needs_column_scan(&self) -> bool { matches!(self, RockKind::Rauk(_)) }

    /// Whether this rock occupies the given model-space voxel.
    ///
    /// Split out of [`RockKind::take_sample`] --- which now delegates to it
    /// --- so that solidity can be evaluated without a `ColumnSample` and
    /// without writing anything, as the traversal analysis needs, while the
    /// stamp and the analysis stay provably in agreement.
    ///
    /// `last_filled` is whether the stamp has already produced a block
    /// higher up this same column; a pillar's de-floating rule reads it.
    fn solid_at(&self, rpos: Vec3<i32>, seed: u32, last_filled: bool) -> bool {
        match self {
            RockKind::Rock(cell) | RockKind::Sandstone(cell) => cell.sample_at(rpos),
            RockKind::Rauk(pillar) => {
                let max_extent = *pillar
                    .max_extent
                    .map(|e| NotNan::new(e).unwrap())
                    .reduce_max();
                let is_filled = |rpos: Vec3<i32>| {
                    pillar.sample_at(rpos)
                        && RandomField::new(seed).chance(
                            rpos,
                            1.5 - rpos.z as f32 / pillar.height
                                - rpos.xy().as_::<f32>().magnitude() / max_extent,
                        )
                };
                is_filled(rpos)
                    // Prevent floating blocks
                    || (last_filled
                        && NEIGHBORS
                            .iter()
                            .all(|n| !is_filled(rpos + n.with_z(0))))
            },
        }
    }

    /// This kind's nominal size parameter, for reporting only.
    #[cfg(test)]
    fn nominal_size(&self) -> f32 {
        match self {
            RockKind::Rock(cell) | RockKind::Sandstone(cell) => cell.size,
            RockKind::Rauk(pillar) => pillar.height,
        }
    }

    fn get_bounds(&self) -> Aabb<i32> {
        match self {
            RockKind::Rock(VoronoiCell { size, .. })
            | RockKind::Sandstone(VoronoiCell { size, .. }) => {
                // Need to use full size because rock can bleed over into other cells
                let extent = *size as i32;
                Aabb {
                    min: Vec3::broadcast(-extent),
                    max: Vec3::broadcast(extent),
                }
            },
            RockKind::Rauk(Pillar {
                max_extent: extent,
                height,
                ..
            }) => Aabb {
                min: (-extent.as_()).with_z(-2),
                max: extent.as_().with_z(*height as i32),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `column_solid` must report exactly the voxels the stamp writes for the
    /// same world column: the axis permutation applied, and the top-down
    /// de-floating chain walked over the whole model column rather than only
    /// the asked-for slice.
    ///
    /// That the stamp and the analysis agree on *solidity* needs no test ---
    /// `take_sample` returns `None` unless `solid_at` says solid, so they are
    /// the same predicate by construction. What needs pinning is this
    /// coordinate mapping.
    #[test]
    fn column_solid_matches_the_stamped_column() {
        let mut rng = ChaChaRng::from_seed(seed_expan::rng_state(77));
        for kind in [
            RockKind::Rock(VoronoiCell::generate(9.0, &mut rng)),
            RockKind::Rauk(Pillar::generate(&mut rng)),
        ] {
            let rock = Rock {
                wpos: Vec3::new(1000, -2000, 130),
                seed: 77,
                units: UnitChooser::new(77).get(77).into(),
                kind,
                accommodation: None,
            };
            let model = rock.kind.get_bounds();
            let world = rock.world_bounds();
            let mut out = Vec::new();
            for wx in world.min.x..=world.max.x {
                for wy in world.min.y..=world.max.y {
                    let wpos2d = Vec2::new(wx, wy);
                    out.clear();
                    rock.column_solid(wpos2d, world.min.z, world.max.z, &mut out);
                    assert_eq!(out.len(), (world.max.z - world.min.z + 1) as usize);

                    let rpos2d = (wpos2d - rock.wpos.xy())
                        .map2(rock.units, |p, unit| unit * p)
                        .sum();
                    if !Aabr::from(model).contains_point(rpos2d) {
                        assert!(
                            out.iter().all(|s| !s),
                            "reported solid outside the model footprint at {wpos2d:?}"
                        );
                        continue;
                    }
                    let mut last_filled = false;
                    for z in (model.min.z..model.max.z).rev() {
                        let solid = rock.kind.solid_at(rpos2d.with_z(z), rock.seed, last_filled);
                        let wz = rock.wpos.z + z;
                        if wz >= world.min.z && wz <= world.max.z {
                            assert_eq!(
                                solid,
                                out[(wz - world.min.z) as usize],
                                "column_solid disagreed at {wpos2d:?} z={wz}"
                            );
                        }
                        last_filled |= solid;
                    }
                }
            }
        }
    }
}
