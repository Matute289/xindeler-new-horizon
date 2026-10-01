//! The world-wide naval-berth/anchorage discovery pass (spec §5.3, §11.2).
//!
//! By analogy with `airship_travel::all_airshipdock_positions`: one mutable
//! counter, advanced by each port's berth *and* anchorage count together,
//! not `.enumerate()`, so ids come out monotonic, non-overlapping and
//! stable per port for an unchanged world seed.
//!
//! # Why ids are reassigned here rather than trusted from the plot
//!
//! `NavalPort` (`world/src/site/plot/naval_port.rs`) builds its own
//! `Berth`/`Anchorage` list with ids local to that one port (berths first,
//! then anchorages, both continuing the same count from zero), because
//! nothing at generation time knows how many berths every *other* port in
//! the world already claimed. This is the same shape
//! `AirshipDockInfo::docking_positions` uses -- raw positions, no id at all
//! -- except `Berth`/`Anchorage` carry a provisional id instead of none,
//! because `Anchorage::tender_berth` has to reference another slot on the
//! *same* port before this global pass ever runs. [`all_naval_berths`] is
//! the only producer of the id a consumer should actually address a berth
//! or anchorage by -- every id it hands back is `local_id + that port's
//! starting offset`, which is exactly why `tender_berth` is remapped by the
//! same offset as the berths and anchorages around it.

use crate::site::{self, Site, Structure as _};
use common::store::{Id, Store};

pub use site::plot::{Anchorage, Berth};

/// Every berth and anchorage one site's `NavalPort` plot (if any) built,
/// with world-wide-unique ids.
#[derive(Debug, Clone)]
pub struct SiteBerths {
    pub site: Id<Site>,
    pub berths: Vec<Berth>,
    pub anchorages: Vec<Anchorage>,
}

/// Collect every naval berth and anchorage in the world, grouped by the
/// site that built them.
///
/// This is the deliverable COW-24's maritime-traffic itinerary code depends
/// on, and it retires that code's placeholder (snapping to the nearest
/// navigable water chunk near a settlement's centre), which should survive
/// only as the `warn!`-logged fallback for a stop whose settlement produced
/// no port. It is also the fix for the `desert_city_airship_dock.rs`
/// failure mode (spec §2.5): a port with no entry here is invisible to
/// every consumer no matter how finished its geometry looks, because this
/// function -- not the plot's own fields -- is the only path a consumer has
/// to a berth's or anchorage's real, addressable id.
pub fn all_naval_berths(sites: &Store<Site>) -> Vec<SiteBerths> {
    let mut next_id = 0u32;
    sites
        .iter()
        .filter_map(|(site_id, site)| {
            let info = site.plots().find_map(|plot| plot.naval_dock_info())?;
            let base_id = next_id;
            next_id += (info.berths.len() + info.anchorages.len()) as u32;

            let berths: Vec<Berth> = info
                .berths
                .iter()
                .map(|berth| Berth {
                    id: base_id + berth.id,
                    ..*berth
                })
                .collect();
            let anchorages: Vec<Anchorage> = info
                .anchorages
                .iter()
                .map(|anchorage| Anchorage {
                    id: base_id + anchorage.id,
                    tender_berth: base_id + anchorage.tender_berth,
                    ..*anchorage
                })
                .collect();

            Some(SiteBerths {
                site: site_id,
                berths,
                anchorages,
            })
        })
        .collect()
}
