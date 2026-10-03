//! Writer for authored raster manifests and tiles.
//!
//! The production writer is the open-world exporter; this one exists so the
//! engine side can be tested end to end without it: tests and the
//! `terrain-probe aw-write` tool paint synthetic water (straight rivers,
//! slot canyons, lakes, straits, thin strips) or import a raw raster, and get
//! byte-exact tiles plus a manifest with their sha256.
//!
//! Painting is in block-column space: a column `(x, y)` belongs to a shape
//! when its centre `(x + 0.5, y + 0.5)` does.

use super::{
    Manifest, RegionManifest, TileManifest,
    format::{self, LayerKind, TILE_CELLS, TILE_SIZE},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vek::*;

/// One cell being authored: absolute centimetres in the block-z frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellCm {
    pub surface: Option<i32>,
    pub bed: Option<i32>,
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
    /// Import a raw raster: `i32` little-endian centimetres, rows from the
    /// south, `i32::MIN` = not authored; applied where either layer is set.
    Raster {
        origin: (i32, i32),
        size: (i32, i32),
        surface_file: PathBuf,
        bed_file: PathBuf,
    },
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
        for y in 0..self.size.y {
            for x in 0..self.size.x {
                let w = self.min + Vec2::new(x, y);
                if shape.contains(w.x, w.y) {
                    self.cells[(y * self.size.x + x) as usize] = cell;
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
            }),
            PaintOp::Bank { shape, bed_cm } => self.paint(shape, CellCm {
                surface: None,
                bed: Some(*bed_cm),
            }),
            PaintOp::Clear { shape } => self.paint(shape, CellCm::default()),
            PaintOp::BankRing { width_m, bed_cm } => self.bank_ring(*width_m, *bed_cm),
            PaintOp::Raster {
                origin,
                size,
                surface_file,
                bed_file,
            } => {
                let read = |p: &Path| -> Result<Vec<i32>, String> {
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
                };
                let s = read(surface_file)?;
                let b = read(bed_file)?;
                for j in 0..size.1 {
                    for i in 0..size.0 {
                        let k = (j * size.0 + i) as usize;
                        let opt = |v: i32| (v != i32::MIN).then_some(v);
                        let cell = CellCm {
                            surface: opt(s[k]),
                            bed: opt(b[k]),
                        };
                        if cell != CellCm::default() {
                            self.set(Vec2::new(origin.0 + i, origin.1 + j), cell);
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
                    out[k] = CellCm {
                        surface: None,
                        bed: Some(bed_cm.unwrap_or(s)),
                    };
                }
            }
        }
        self.cells = out;
    }

    /// Encode every tile that holds any authored cell.
    pub fn build(&self) -> Result<BuiltRegion, String> {
        let tiles = self.size.map(|e| (e + TILE_SIZE - 1) / TILE_SIZE);
        let mut out = Vec::new();
        let mut entries = Vec::new();
        for ty in 0..tiles.y {
            for tx in 0..tiles.x {
                let origin = self.min + Vec2::new(tx, ty) * TILE_SIZE;
                let mut surface = vec![None; TILE_CELLS];
                let mut bed = vec![None; TILE_CELLS];
                let mut any = false;
                for j in 0..TILE_SIZE {
                    for i in 0..TILE_SIZE {
                        let c = self.get(origin + Vec2::new(i, j));
                        let k = format::RawTile::idx(i, j);
                        surface[k] = c.surface;
                        bed[k] = c.bed;
                        any |= c != CellCm::default();
                    }
                }
                if !any {
                    continue;
                }
                let bytes = format::encode_water(origin, &surface, &bed)?;
                entries.push(TileManifest {
                    layer: LayerKind::Water,
                    tx,
                    ty,
                    sha256: format::sha256_hex(&bytes),
                });
                out.push(((tx, ty), bytes));
            }
        }
        Ok(BuiltRegion {
            manifest: RegionManifest {
                id: self.spec.id.clone(),
                min: self.spec.min,
                max: self.spec.max,
                feather_m: self.spec.feather_m,
                tile_size_m: TILE_SIZE,
                cell_size_m: 1,
                layers: vec![LayerKind::Water],
                tiles: entries,
            },
            tiles: out,
        })
    }
}

/// An encoded region: its manifest entry and its tiles' bytes.
pub struct BuiltRegion {
    pub manifest: RegionManifest,
    pub tiles: Vec<((i32, i32), Vec<u8>)>,
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
        for ((tx, ty), bytes) in &r.tiles {
            let name = format!(
                "{stem}_ar_{}_{}_{tx}_{ty}.bin",
                r.manifest.id,
                LayerKind::Water.asset_name()
            );
            let p = map_dir.join(name);
            std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            written.push(p);
        }
    }
    Ok(written)
}
