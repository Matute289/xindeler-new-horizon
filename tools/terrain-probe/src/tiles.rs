//! Tiling math for the client dump (pure, no engine): which chunks the bot
//! streams from which teleport position.
//!
//! The client drops chunks that are far from its player, so a big box cannot
//! be held in the client at once; the bot visits one tile at a time and the
//! tile's chunks are harvested before it moves on. A tile must fit inside the
//! view distance with the client's own safety rings (it requests chunks inside
//! roughly `view_distance - 3` of the player), hence [`EDGE_MARGIN`].

use crate::probe::CHUNK;

/// Chunks of the view distance kept free around a tile (the client skips about
/// two rings and needs neighbours for meshing; one more as slack).
pub const EDGE_MARGIN: u32 = 4;

/// Inclusive rectangle of chunk keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRect {
    pub cx0: i32,
    pub cy0: i32,
    pub cx1: i32,
    pub cy1: i32,
}

impl ChunkRect {
    pub fn width(&self) -> i32 { self.cx1 - self.cx0 + 1 }

    pub fn height(&self) -> i32 { self.cy1 - self.cy0 + 1 }

    pub fn count(&self) -> usize { self.width() as usize * self.height() as usize }

    pub fn keys(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        (self.cy0..=self.cy1).flat_map(move |cy| (self.cx0..=self.cx1).map(move |cx| (cx, cy)))
    }

    /// World position (metres) of the middle of the rectangle.
    pub fn center_wpos(&self) -> (i32, i32) {
        (
            (self.cx0 + self.cx1 + 1) * CHUNK / 2,
            (self.cy0 + self.cy1 + 1) * CHUNK / 2,
        )
    }

    /// Distance, in chunks, from the rectangle's centre to its farthest chunk
    /// centre.
    pub fn farthest_chunk(&self) -> f64 {
        f64::from(self.width() - 1).hypot(f64::from(self.height() - 1)) / 2.0
    }

    /// Whether every chunk of the rectangle is inside the usable radius of a
    /// player standing at its centre.
    pub fn fits(&self, view_distance: u32) -> bool {
        self.farthest_chunk() <= f64::from(view_distance) - f64::from(EDGE_MARGIN)
    }
}

/// Largest square tile side (in chunks) that [`ChunkRect::fits`].
pub fn max_tile_side(view_distance: u32) -> Option<i32> {
    let fits = |s: i32| {
        ChunkRect {
            cx0: 0,
            cy0: 0,
            cx1: s - 1,
            cy1: s - 1,
        }
        .fits(view_distance)
    };
    if !fits(1) {
        return None;
    }
    let mut s = 1;
    while fits(s + 1) {
        s += 1;
    }
    Some(s)
}

/// Split `total` into `parts` near-equal lengths (larger ones first).
fn split_even(total: i32, parts: i32) -> Vec<i32> {
    let (base, extra) = (total / parts, total % parts);
    (0..parts).map(|i| base + i32::from(i < extra)).collect()
}

/// Partition `chunks` into tiles that each fit `view_distance`, row-major from
/// the south-west. Every chunk belongs to exactly one tile.
pub fn plan_tiles(chunks: ChunkRect, view_distance: u32) -> Result<Vec<ChunkRect>, String> {
    let side = max_tile_side(view_distance).ok_or_else(|| {
        format!(
            "view distance {view_distance} is too small (need at least {})",
            EDGE_MARGIN + 1
        )
    })?;
    let nx = (chunks.width() + side - 1) / side;
    let ny = (chunks.height() + side - 1) / side;
    let (ws, hs) = (
        split_even(chunks.width(), nx),
        split_even(chunks.height(), ny),
    );
    let mut out = Vec::with_capacity((nx * ny) as usize);
    let mut cy = chunks.cy0;
    for &h in &hs {
        let mut cx = chunks.cx0;
        for &w in &ws {
            out.push(ChunkRect {
                cx0: cx,
                cy0: cy,
                cx1: cx + w - 1,
                cy1: cy + h - 1,
            });
            cx += w;
        }
        cy += h;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(cx0: i32, cy0: i32, cx1: i32, cy1: i32) -> ChunkRect {
        ChunkRect { cx0, cy0, cx1, cy1 }
    }

    #[test]
    fn side_grows_with_view_distance_and_always_fits() {
        assert_eq!(max_tile_side(4), Some(1));
        assert_eq!(max_tile_side(3), None);
        let mut last = 0;
        for vd in 5..80 {
            let s = max_tile_side(vd).unwrap();
            assert!(s >= last);
            last = s;
            assert!(rect(0, 0, s - 1, s - 1).fits(vd));
            assert!(!rect(0, 0, s, s).fits(vd));
        }
        // The default: view distance 24 gives 29-chunk (928 m) tiles.
        assert_eq!(max_tile_side(24), Some(29));
    }

    #[test]
    fn tiles_partition_the_box_exactly_and_each_fits() {
        for (w, h, vd) in [
            (1, 1, 24),
            (37, 34, 24),
            (44, 44, 24),
            (45, 7, 12),
            (100, 3, 16),
        ] {
            let all = rect(10, -3, 10 + w - 1, -3 + h - 1);
            let tiles = plan_tiles(all, vd).unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for t in &tiles {
                assert!(t.fits(vd), "{t:?} must fit vd {vd}");
                for k in t.keys() {
                    assert!(seen.insert(k), "{k:?} in two tiles");
                    assert!(
                        (all.cx0..=all.cx1).contains(&k.0) && (all.cy0..=all.cy1).contains(&k.1)
                    );
                }
            }
            assert_eq!(seen.len(), all.count());
        }
    }

    #[test]
    fn a_1_4_km_box_is_four_balanced_tiles_at_the_default_view_distance() {
        // 1400 m = chunks 22750/32 ..: 44 chunks wide.
        let all = rect(0, 0, 43, 43);
        let t = plan_tiles(all, 24).unwrap();
        assert_eq!(t.len(), 4);
        assert!(t.iter().all(|t| t.width() == 22 && t.height() == 22));
        assert_eq!(t[0].center_wpos(), (11 * 32, 11 * 32));
        assert_eq!(t[3].center_wpos(), (33 * 32, 33 * 32));
    }

    #[test]
    fn too_small_view_distance_is_an_error() {
        assert!(plan_tiles(rect(0, 0, 3, 3), 4 - 1).is_err());
    }

    #[test]
    fn center_is_the_middle_of_the_rect_in_world_metres() {
        assert_eq!(rect(2, 4, 3, 5).center_wpos(), (96, 160));
        assert_eq!(rect(-2, -2, -1, -1).center_wpos(), (-32, -32));
    }
}
