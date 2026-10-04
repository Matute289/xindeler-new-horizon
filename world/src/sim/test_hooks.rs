//! Test-only seams into `WorldSim::generate`.
//!
//! Lives in `sim` (the code that calls it) rather than in any one test
//! module, so the dependency points from tests into `sim` and never the
//! other way. Compiled only under `cfg(test)`.

use super::ModernMap;
use common::terrain::MapSizeLg;
use std::{cell::RefCell, sync::Arc};

/// Edits a loaded map's `(alt, basement)` chunk arrays, in blocks.
pub(crate) type MapPerturbation = Arc<dyn Fn(MapSizeLg, &mut [f64], &mut [f64]) + Send + Sync>;

thread_local! {
    /// Per thread, so a test installs it only on the workers of its own
    /// rayon pool (via `ThreadPoolBuilder::start_handler`) and tests running
    /// concurrently on other pools never see it.
    static LOADED_MAP_PERTURBATION: RefCell<Option<MapPerturbation>> =
        const { RefCell::new(None) };
}

/// Install (or clear) the perturbation on the calling thread.
pub(crate) fn set_loaded_map_perturbation(perturbation: Option<MapPerturbation>) {
    LOADED_MAP_PERTURBATION.with(|slot| *slot.borrow_mut() = perturbation);
}

/// Called by `WorldSim::generate` right after the world file is loaded and
/// before anything is derived from it. A no-op unless this thread has a
/// perturbation installed.
pub(super) fn perturb_loaded_map(map_size_lg: MapSizeLg, mut map: ModernMap) -> ModernMap {
    LOADED_MAP_PERTURBATION.with(|slot| {
        if let Some(perturb) = slot.borrow().as_ref() {
            perturb(map_size_lg, &mut map.alt, &mut map.basement);
        }
    });
    map
}
