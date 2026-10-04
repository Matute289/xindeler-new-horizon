//! SCRATCH (nh161-footprint-prototype, measure only): per-city generation
//! counters. Disabled (one thread-local check) unless `start()` was called.
use super::tile::{HazardKind, Tile, TileKind};
use std::{cell::RefCell, collections::BTreeMap};

thread_local! {
    static PROBE: RefCell<Option<BTreeMap<&'static str, u64>>> = const { RefCell::new(None) };
    /// Prototype footprint: tile predicate (`true` = allowed) for the city
    /// currently being generated, and the quota-fill target.
    pub(crate) static FOOTPRINT: RefCell<Option<Footprint>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct Footprint {
    /// Allowed tiles, as a set of half-planes in tile space (all must hold):
    /// `dot(tile, n) <= c`.
    pub half_planes: Vec<((f32, f32), f32)>,
    /// Max distance from origin in tiles.
    pub radius_tiles: f32,
    /// Quota: keep drawing until this many buildings exist (bounded).
    pub target_buildings: usize,
    /// Restrict plot placement (find_aabr candidate centres) to the footprint.
    pub restrict: bool,
    /// Authored main-plaza anchor (tile coords relative to origin).
    pub anchor: Option<vek::Vec2<i32>>,
    /// When the random plaza search fails, place the next plaza
    /// deterministically at the footprint frontier.
    pub frontier: bool,
}

impl Footprint {
    pub fn allows(&self, t: vek::Vec2<i32>) -> bool {
        let (x, y) = (t.x as f32, t.y as f32);
        (x * x + y * y).sqrt() <= self.radius_tiles
            && self
                .half_planes
                .iter()
                .all(|((nx, ny), c)| x * nx + y * ny <= *c)
    }
}

pub fn footprint_allows(t: vek::Vec2<i32>) -> bool {
    FOOTPRINT.with(|f| {
        f.borrow()
            .as_ref()
            .is_none_or(|f| !f.restrict || f.allows(t))
    })
}

pub fn start() { PROBE.with(|p| *p.borrow_mut() = Some(BTreeMap::new())); }

pub fn take() -> BTreeMap<&'static str, u64> {
    PROBE.with(|p| p.borrow_mut().take().unwrap_or_default())
}

#[inline]
pub fn bump(k: &'static str) {
    PROBE.with(|p| {
        if let Some(m) = p.borrow_mut().as_mut() {
            *m.entry(k).or_default() += 1;
        }
    })
}

#[inline]
pub fn add(k: &'static str, n: u64) {
    PROBE.with(|p| {
        if let Some(m) = p.borrow_mut().as_mut() {
            *m.entry(k).or_default() += n;
        }
    })
}

pub fn enabled() -> bool { PROBE.with(|p| p.borrow().is_some()) }

pub fn tile_class(t: &Tile) -> &'static str {
    match &t.kind {
        TileKind::Empty => "empty",
        TileKind::Hazard(HazardKind::Water) => "water",
        TileKind::Hazard(HazardKind::Hill { .. }) => "hill",
        TileKind::Path { .. } => "path",
        TileKind::Road { .. } | TileKind::Plaza => "road",
        TileKind::Field => "field",
        TileKind::Pier => "pier",
        TileKind::Building => "building",
        _ => "other",
    }
}

/// (buildings, plots, houses, plazas, fields)
pub fn summarize(site: &super::Site) -> (usize, usize, usize, usize, usize) {
    use super::PlotKind;
    let mut b = 0;
    let mut h = 0;
    let mut pz = 0;
    let mut f = 0;
    for p in site.plots() {
        match p.kind() {
            PlotKind::Plaza(_) => pz += 1,
            PlotKind::Road(_) => {},
            PlotKind::FarmField(_) => f += 1,
            PlotKind::House(_) => {
                h += 1;
                b += 1;
            },
            _ => b += 1,
        }
    }
    (b, site.plots().len(), h, pz, f)
}

/// Text dump of the tile grid (radius `r` tiles) and plots, for images.
pub fn dump(site: &super::Site, r: i32) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "ORIGIN\t{}\t{}", site.origin.x, site.origin.y);
    for y in -r..=r {
        let mut row = String::new();
        for x in -r..=r {
            let t = site.tiles.get(vek::Vec2::new(x, y));
            row.push(match tile_class(t) {
                "empty" => '.',
                "water" => 'w',
                "hill" => 'h',
                "path" => 'p',
                "road" => 'r',
                "field" => 'f',
                "pier" => 'P',
                "building" => 'B',
                _ => 'o',
            });
        }
        let _ = writeln!(out, "ROW\t{y}\t{row}");
    }
    for p in site.plots() {
        let b = p.find_bounds();
        let _ = writeln!(
            out,
            "PLOT\t{}\t{}\t{}\t{}\t{}",
            p.kind(),
            b.min.x,
            b.min.y,
            b.max.x,
            b.max.y
        );
    }
    out
}

pub fn footprint() -> Option<Footprint> { FOOTPRINT.with(|f| f.borrow().clone()) }
