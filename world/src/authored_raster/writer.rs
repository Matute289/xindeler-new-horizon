//! Writer for authored raster manifests and tiles (test builds and the
//! `tools` feature only: the game never writes tiles).
//!
//! The production writer is the open-world exporter; this one exists so the
//! engine side can be tested end to end without it: tests and the
//! `terrain-probe aw-write` tool paint synthetic water (straight rivers,
//! slot canyons, lakes, straits, thin strips) and ground (plateaus, trenches,
//! ramps, cliffs), or import a raw raster, and get byte-exact tiles plus a
//! manifest with their sha256. A region's layers follow from what it paints:
//! `[Water]` (also for an empty region), `[Ground]` or `[Water, Ground]`.
//!
//! Painting is in block-column space: a column `(x, y)` belongs to a shape
//! when its centre `(x + 0.5, y + 0.5)` does.

use super::{
    ConsistencyBudget, Manifest, RegionManifest, SeaFill, TileManifest,
    format::{self, GROUND_EXACT, LayerKind, TILE_CELLS, TILE_SIZE},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vek::*;

/// One cell being authored: absolute centimetres in the block-z frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellCm {
    pub surface: Option<i32>,
    pub bed: Option<i32>,
    /// Ground layer: centimetres and weight.
    pub ground: Option<(i32, u8)>,
}

impl CellCm {
    fn has_water(&self) -> bool { self.surface.is_some() || self.bed.is_some() }
}

/// A region to write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionSpec {
    pub id: String,
    /// Inclusive south-west corner (wpos, multiple of 32).
    pub min: (i32, i32),
    /// Exclusive north-east corner (wpos, multiple of 32).
    pub max: (i32, i32),
    pub feather_m: i32,
    #[serde(default)]
    pub ops: Vec<PaintOp>,
    /// Passed through to [`RegionManifest::suppress_procedural_in_water`].
    #[serde(default)]
    pub suppress_procedural_in_water: bool,
    /// Passed through to [`RegionManifest::exclude_procedural_margin_m`].
    #[serde(default)]
    pub exclude_procedural_margin_m: i32,
    /// Passed through to [`RegionManifest::allow_partial`].
    #[serde(default)]
    pub allow_partial: bool,
    /// Passed through to [`RegionManifest::allow_partial_chunks`].
    #[serde(default)]
    pub allow_partial_chunks: Vec<(i32, i32)>,
    /// Passed through to [`RegionManifest::aquatic_ecology_profile`].
    #[serde(default)]
    pub aquatic_ecology_profile: Option<String>,
    /// Passed through to [`RegionManifest::consistency`].
    #[serde(default)]
    pub consistency: ConsistencyBudget,
    /// Passed through to [`RegionManifest::sea_fill`].
    #[serde(default)]
    pub sea_fill: SeaFill,
    /// Passed through to [`RegionManifest::sites_on_patch`].
    #[serde(default)]
    pub sites_on_patch: Vec<String>,
    /// Passed through to [`RegionManifest::site_levelling`].
    #[serde(default = "super::default_true")]
    pub site_levelling: bool,
    /// Passed through to [`RegionManifest::max_exposed_void_columns`].
    #[serde(default)]
    pub max_exposed_void_columns: u32,
}

impl RegionSpec {
    /// A region with the default settings (natural decorations kept, no
    /// partial chunks, no aquatic profile, default consistency budget).
    pub fn new(
        id: impl Into<String>,
        min: (i32, i32),
        max: (i32, i32),
        feather_m: i32,
        ops: Vec<PaintOp>,
    ) -> Self {
        Self {
            id: id.into(),
            min,
            max,
            feather_m,
            ops,
            suppress_procedural_in_water: false,
            exclude_procedural_margin_m: 0,
            allow_partial: false,
            allow_partial_chunks: Vec::new(),
            aquatic_ecology_profile: None,
            consistency: ConsistencyBudget::default(),
            sea_fill: SeaFill::Auto,
            sites_on_patch: Vec::new(),
            site_levelling: true,
            max_exposed_void_columns: 0,
        }
    }
}

/// A shape in wpos metres.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Shape {
    /// Columns whose centre lies in `[x0, x1) x [y0, y1)`.
    Rect { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// Columns whose centre lies in the ellipse.
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    /// Columns whose centre lies in `r_in <= r < r_out` around the centre.
    Annulus {
        cx: f32,
        cy: f32,
        r_in: f32,
        r_out: f32,
    },
}

impl Shape {
    fn contains(&self, x: i32, y: i32) -> bool {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        match *self {
            Shape::Rect { x0, y0, x1, y1 } => px >= x0 && px < x1 && py >= y0 && py < y1,
            Shape::Ellipse { cx, cy, rx, ry } => {
                let (dx, dy) = ((px - cx) / rx, (py - cy) / ry);
                dx * dx + dy * dy <= 1.0
            },
            Shape::Annulus {
                cx,
                cy,
                r_in,
                r_out,
            } => {
                let r = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                r >= r_in && r < r_out
            },
        }
    }
}

/// One painting operation, applied in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "op")]
pub enum PaintOp {
    /// Water with an exact surface over an exact bed.
    Water {
        shape: Shape,
        surface_cm: i32,
        bed_cm: i32,
    },
    /// Dry authored ground (overwrites water too).
    Bank { shape: Shape, bed_cm: i32 },
    /// Clear cells back to "not authored".
    Clear { shape: Shape },
    /// Every not-authored cell within `width_m` (Chebyshev) of a wet cell
    /// becomes a bank: at `bed_cm` if given, else at the highest neighbouring
    /// wet surface (so the water is always contained).
    BankRing {
        width_m: i32,
        #[serde(default)]
        bed_cm: Option<i32>,
    },
    /// Ground-layer cells at a constant altitude (water cells keep
    /// precedence where both are painted).
    Ground {
        shape: Shape,
        ground_cm: i32,
        /// [`GROUND_EXACT`] unless given (1..=254 is a blend weight).
        #[serde(default = "exact_weight")]
        weight: u8,
    },
    /// Ground-layer cells on a plane (a ramp): at a column centre `(px, py)`
    /// the ground is `round(origin_cm + (px - origin.0) * cm_per_m.0 + (py -
    /// origin.1) * cm_per_m.1)`, exact.
    GroundPlane {
        shape: Shape,
        origin: (f32, f32),
        origin_cm: i32,
        cm_per_m: (f32, f32),
    },
    /// Clear the ground layer only.
    ClearGround { shape: Shape },
    /// The blend ring around the exact ground cells (weight
    /// [`GROUND_EXACT`]): every cell no layer authors whose chamfer (3-4)
    /// distance to the nearest exact cell is below `width_m` becomes a blend
    /// cell of weight [`ring_weight`], at `ground_cm` if given, else at the
    /// ground of that nearest exact cell (a master that continues the patch's
    /// edge flat). The exporter's ring follows the same weight rule.
    GroundRing {
        width_m: i32,
        #[serde(default)]
        ground_cm: Option<i32>,
    },
    /// Import a raw ground raster: `i32` little-endian centimetres, rows from
    /// the south, `i32::MIN` = not authored; exact cells.
    GroundRaster {
        origin: (i32, i32),
        size: (i32, i32),
        ground_file: PathBuf,
    },
    /// Import a raw raster: `i32` little-endian centimetres, rows from the
    /// south, `i32::MIN` = not authored; applied where either layer is set.
    Raster {
        origin: (i32, i32),
        size: (i32, i32),
        surface_file: PathBuf,
        bed_file: PathBuf,
    },
}

fn exact_weight() -> u8 { GROUND_EXACT }

/// The blend weight of a ring cell at chamfer (3-4) distance `d3` (in thirds
/// of a metre) from the nearest footprint cell, for a ring `blend_m` wide:
/// `round(255 * smoothstep(1 - d / blend_m))` in integer arithmetic (so the
/// exporter's mirror gives the same bytes), at most 254 (a ring cell is never
/// exact); `None` at the footprint itself (`d3 == 0`), at or beyond the
/// ring's outer edge, or where the weight rounds to 0.
pub fn ring_weight(d3: u32, blend_m: u32) -> Option<u8> {
    let d = 3 * u64::from(blend_m);
    let d3 = u64::from(d3);
    if d3 == 0 || d3 >= d {
        return None;
    }
    let u = d - d3;
    // smoothstep(u / d) = u^2 (3d - 2u) / d^3, rounded half up.
    let num = 255 * u * u * (3 * d - 2 * u);
    let den = d * d * d;
    let w = (2 * num + den) / (2 * den);
    (w > 0).then(|| w.min(u64::from(GROUND_EXACT) - 1) as u8)
}

fn read_i32_raster(p: &Path, size: (i32, i32)) -> Result<Vec<i32>, String> {
    let b = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let (c, rest) = b.as_chunks::<4>();
    if !rest.is_empty() || c.len() != (size.0 * size.1) as usize {
        return Err(format!(
            "{}: {} bytes is not {} x {} i32",
            p.display(),
            b.len(),
            size.0,
            size.1
        ));
    }
    Ok(c.iter().map(|c| i32::from_le_bytes(*c)).collect())
}

/// A dense raster over one region box.
pub struct RegionRaster {
    pub spec: RegionSpec,
    min: Vec2<i32>,
    size: Vec2<i32>,
    cells: Vec<CellCm>,
}

impl RegionRaster {
    pub fn new(spec: RegionSpec) -> Self {
        let min: Vec2<i32> = Vec2::from(spec.min);
        let size: Vec2<i32> = Vec2::from(spec.max) - min;
        let n = (size.x.max(0) as usize) * (size.y.max(0) as usize);
        Self {
            spec,
            min,
            size,
            cells: vec![CellCm::default(); n],
        }
    }

    #[inline]
    fn idx(&self, wpos: Vec2<i32>) -> Option<usize> {
        let l = wpos - self.min;
        (l.x >= 0 && l.y >= 0 && l.x < self.size.x && l.y < self.size.y)
            .then(|| (l.y * self.size.x + l.x) as usize)
    }

    pub fn get(&self, wpos: Vec2<i32>) -> CellCm {
        self.idx(wpos).map(|i| self.cells[i]).unwrap_or_default()
    }

    pub fn set(&mut self, wpos: Vec2<i32>, cell: CellCm) {
        if let Some(i) = self.idx(wpos) {
            self.cells[i] = cell;
        }
    }

    fn paint(&mut self, shape: &Shape, cell: CellCm) {
        self.paint_with(shape, |c, _| {
            c.surface = cell.surface;
            c.bed = cell.bed;
        });
    }

    fn paint_with(&mut self, shape: &Shape, mut f: impl FnMut(&mut CellCm, Vec2<i32>)) {
        for y in 0..self.size.y {
            for x in 0..self.size.x {
                let w = self.min + Vec2::new(x, y);
                if shape.contains(w.x, w.y) {
                    f(&mut self.cells[(y * self.size.x + x) as usize], w);
                }
            }
        }
    }

    /// Apply one operation.
    pub fn apply(&mut self, op: &PaintOp) -> Result<(), String> {
        match op {
            PaintOp::Water {
                shape,
                surface_cm,
                bed_cm,
            } => self.paint(shape, CellCm {
                surface: Some(*surface_cm),
                bed: Some(*bed_cm),
                ground: None,
            }),
            PaintOp::Bank { shape, bed_cm } => self.paint(shape, CellCm {
                surface: None,
                bed: Some(*bed_cm),
                ground: None,
            }),
            PaintOp::Clear { shape } => self.paint_with(shape, |c, _| *c = CellCm::default()),
            PaintOp::Ground {
                shape,
                ground_cm,
                weight,
            } => self.paint_with(shape, |c, _| c.ground = Some((*ground_cm, *weight))),
            PaintOp::GroundPlane {
                shape,
                origin,
                origin_cm,
                cm_per_m,
            } => self.paint_with(shape, |c, w| {
                let (px, py) = (w.x as f64 + 0.5, w.y as f64 + 0.5);
                let cm = *origin_cm as f64
                    + (px - origin.0 as f64) * cm_per_m.0 as f64
                    + (py - origin.1 as f64) * cm_per_m.1 as f64;
                c.ground = Some((cm.round() as i32, GROUND_EXACT));
            }),
            PaintOp::ClearGround { shape } => self.paint_with(shape, |c, _| c.ground = None),
            PaintOp::GroundRaster {
                origin,
                size,
                ground_file,
            } => {
                let g = read_i32_raster(ground_file, *size)?;
                for j in 0..size.1 {
                    for i in 0..size.0 {
                        let v = g[(j * size.0 + i) as usize];
                        if v != i32::MIN
                            && let Some(k) = self.idx(Vec2::new(origin.0 + i, origin.1 + j))
                        {
                            self.cells[k].ground = Some((v, GROUND_EXACT));
                        }
                    }
                }
            },
            PaintOp::BankRing { width_m, bed_cm } => self.bank_ring(*width_m, *bed_cm),
            PaintOp::GroundRing { width_m, ground_cm } => self.ground_ring(*width_m, *ground_cm)?,
            PaintOp::Raster {
                origin,
                size,
                surface_file,
                bed_file,
            } => {
                let s = read_i32_raster(surface_file, *size)?;
                let b = read_i32_raster(bed_file, *size)?;
                for j in 0..size.1 {
                    for i in 0..size.0 {
                        let k = (j * size.0 + i) as usize;
                        let opt = |v: i32| (v != i32::MIN).then_some(v);
                        let (surface, bed) = (opt(s[k]), opt(b[k]));
                        if (surface, bed) != (None, None)
                            && let Some(at) = self.idx(Vec2::new(origin.0 + i, origin.1 + j))
                        {
                            self.cells[at].surface = surface;
                            self.cells[at].bed = bed;
                        }
                    }
                }
            },
        }
        Ok(())
    }

    fn bank_ring(&mut self, width: i32, bed_cm: Option<i32>) {
        let mut out = self.cells.clone();
        for y in 0..self.size.y {
            for x in 0..self.size.x {
                let k = (y * self.size.x + x) as usize;
                // Authored ground holds the water by itself (or is refused
                // at load when lower): no bank over it.
                if self.cells[k] != CellCm::default() {
                    continue;
                }
                let mut best: Option<i32> = None;
                for dy in -width..=width {
                    for dx in -width..=width {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= self.size.x || ny >= self.size.y {
                            continue;
                        }
                        let c = self.cells[(ny * self.size.x + nx) as usize];
                        if let (Some(s), Some(_)) = (c.surface, c.bed) {
                            best = Some(best.map_or(s, |b: i32| b.max(s)));
                        }
                    }
                }
                if let Some(s) = best {
                    // A bank level with the water's surface block holds it.
                    out[k].surface = None;
                    out[k].bed = Some(bed_cm.unwrap_or(s));
                }
            }
        }
        self.cells = out;
    }

    fn ground_ring(&mut self, width: i32, ground_cm: Option<i32>) -> Result<(), String> {
        if !(1..=256).contains(&width) {
            return Err(format!("ground ring width {width} m must be 1..=256"));
        }
        let (w, h) = (self.size.x, self.size.y);
        // Chamfer (3-4) distance to the nearest exact cell, carrying that
        // cell's ground; two raster passes, integer only.
        let mut dist = vec![(u32::MAX, 0i32); self.cells.len()];
        for (k, c) in self.cells.iter().enumerate() {
            if let Some((g, GROUND_EXACT)) = c.ground {
                dist[k] = (0, g);
            }
        }
        let relax = |dist: &mut Vec<(u32, i32)>, x: i32, y: i32, nx: i32, ny: i32, step: u32| {
            if nx >= 0 && ny >= 0 && nx < w && ny < h {
                let (nd, ng) = dist[(ny * w + nx) as usize];
                let v = nd.saturating_add(step);
                let k = (y * w + x) as usize;
                if v < dist[k].0 {
                    dist[k] = (v, ng);
                }
            }
        };
        for y in 0..h {
            for x in 0..w {
                relax(&mut dist, x, y, x - 1, y, 3);
                relax(&mut dist, x, y, x, y - 1, 3);
                relax(&mut dist, x, y, x - 1, y - 1, 4);
                relax(&mut dist, x, y, x + 1, y - 1, 4);
            }
        }
        for y in (0..h).rev() {
            for x in (0..w).rev() {
                relax(&mut dist, x, y, x + 1, y, 3);
                relax(&mut dist, x, y, x, y + 1, 3);
                relax(&mut dist, x, y, x + 1, y + 1, 4);
                relax(&mut dist, x, y, x - 1, y + 1, 4);
            }
        }
        for (k, c) in self.cells.iter_mut().enumerate() {
            if *c != CellCm::default() {
                continue;
            }
            let (d3, g) = dist[k];
            if let Some(weight) = ring_weight(d3, width as u32) {
                c.ground = Some((ground_cm.unwrap_or(g), weight));
            }
        }
        Ok(())
    }

    /// Encode every tile that holds any authored cell, per layer.
    pub fn build(&self) -> Result<BuiltRegion, String> {
        let tiles = self.size.map(|e| (e + TILE_SIZE - 1) / TILE_SIZE);
        let has_water = self.cells.iter().any(CellCm::has_water);
        let has_ground = self.cells.iter().any(|c| c.ground.is_some());
        let mut water_tiles = Vec::new();
        let mut ground_tiles = Vec::new();
        let mut entries = Vec::new();
        for ty in 0..tiles.y {
            for tx in 0..tiles.x {
                let origin = self.min + Vec2::new(tx, ty) * TILE_SIZE;
                let mut surface = vec![None; TILE_CELLS];
                let mut bed = vec![None; TILE_CELLS];
                let mut ground = vec![None; TILE_CELLS];
                let (mut any_water, mut any_ground) = (false, false);
                for j in 0..TILE_SIZE {
                    for i in 0..TILE_SIZE {
                        let c = self.get(origin + Vec2::new(i, j));
                        let k = format::cell_index(i, j);
                        surface[k] = c.surface;
                        bed[k] = c.bed;
                        ground[k] = c.ground;
                        any_water |= c.has_water();
                        any_ground |= c.ground.is_some();
                    }
                }
                if any_water {
                    let bytes = format::encode_water(origin, &surface, &bed)?;
                    entries.push(TileManifest {
                        layer: LayerKind::Water,
                        tx,
                        ty,
                        sha256: format::sha256_hex(&bytes),
                    });
                    water_tiles.push(((tx, ty), bytes));
                }
                if any_ground {
                    let bytes = format::encode_ground(origin, &ground)?;
                    entries.push(TileManifest {
                        layer: LayerKind::Ground,
                        tx,
                        ty,
                        sha256: format::sha256_hex(&bytes),
                    });
                    ground_tiles.push(((tx, ty), bytes));
                }
            }
        }
        let layers = match (has_water, has_ground) {
            (_, false) => vec![LayerKind::Water],
            (false, true) => vec![LayerKind::Ground],
            (true, true) => vec![LayerKind::Water, LayerKind::Ground],
        };
        Ok(BuiltRegion {
            manifest: RegionManifest {
                id: self.spec.id.clone(),
                min: self.spec.min,
                max: self.spec.max,
                feather_m: self.spec.feather_m,
                tile_size_m: TILE_SIZE,
                cell_size_m: 1,
                layers,
                tiles: entries,
                suppress_procedural_in_water: self.spec.suppress_procedural_in_water,
                exclude_procedural_margin_m: self.spec.exclude_procedural_margin_m,
                allow_partial: self.spec.allow_partial,
                allow_partial_chunks: self.spec.allow_partial_chunks.clone(),
                aquatic_ecology_profile: self.spec.aquatic_ecology_profile.clone(),
                consistency: self.spec.consistency,
                sea_fill: self.spec.sea_fill,
                sites_on_patch: self.spec.sites_on_patch.clone(),
                site_levelling: self.spec.site_levelling,
                max_exposed_void_columns: self.spec.max_exposed_void_columns,
            },
            tiles: water_tiles,
            ground_tiles,
        })
    }
}

/// An encoded region: its manifest entry and its tiles' bytes.
pub struct BuiltRegion {
    pub manifest: RegionManifest,
    /// Water tiles.
    pub tiles: Vec<((i32, i32), Vec<u8>)>,
    /// Ground tiles.
    pub ground_tiles: Vec<((i32, i32), Vec<u8>)>,
}

impl BuiltRegion {
    /// The bytes of one tile, for an in-memory `fetch`.
    pub fn tile(&self, layer: LayerKind, tx: i32, ty: i32) -> Option<&[u8]> {
        let list = match layer {
            LayerKind::Water => &self.tiles,
            LayerKind::Ground => &self.ground_tiles,
        };
        list.iter()
            .find(|(t, _)| *t == (tx, ty))
            .map(|(_, b)| b.as_slice())
    }
}

/// Paint `spec.ops` into a fresh raster and encode it.
pub fn build_region(spec: &RegionSpec) -> Result<BuiltRegion, String> {
    let mut raster = RegionRaster::new(spec.clone());
    for op in &spec.ops {
        raster.apply(op)?;
    }
    raster.build()
}

/// The manifest for `regions`.
pub fn manifest(regions: &[BuiltRegion]) -> Manifest {
    Manifest {
        schema: super::MANIFEST_SCHEMA,
        regions: regions.iter().map(|r| r.manifest.clone()).collect(),
    }
}

/// Write the manifest and tiles of `regions` for the map `world.map.<stem>`
/// into `map_dir` (an asset root's `world/map` directory). Returns the files
/// written.
pub fn write_assets(
    map_dir: &Path,
    stem: &str,
    regions: &[BuiltRegion],
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(map_dir).map_err(|e| format!("{}: {e}", map_dir.display()))?;
    let mut written = Vec::new();
    let ron = ron::ser::to_string_pretty(&manifest(regions), ron::ser::PrettyConfig::default())
        .map_err(|e| format!("manifest serialisation: {e}"))?;
    let p = map_dir.join(format!("{stem}_authored_rasters.ron"));
    std::fs::write(&p, ron).map_err(|e| format!("{}: {e}", p.display()))?;
    written.push(p);
    for r in regions {
        for (layer, list) in [
            (LayerKind::Water, &r.tiles),
            (LayerKind::Ground, &r.ground_tiles),
        ] {
            for ((tx, ty), bytes) in list {
                let name = format!(
                    "{stem}_ar_{}_{}_{tx}_{ty}.bin",
                    r.manifest.id,
                    layer.asset_name()
                );
                let p = map_dir.join(name);
                std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
                written.push(p);
            }
        }
    }
    Ok(written)
}
