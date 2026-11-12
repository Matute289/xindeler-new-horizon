//! Test-only allocation tracker: a global allocator that, for the threads that
//! opted in through [`measure`], records the peak live bytes and the largest
//! single allocation. Used to prove that reading malformed dumps stays bounded.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    // `const` initialisers with `Copy` types: no lazy init, no destructor, so
    // touching them from inside the allocator is safe.
    static ON: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static BIGGEST: Cell<usize> = const { Cell::new(0) };
}

pub struct Tracking;

fn grow(by: usize) {
    if ON.with(Cell::get) {
        let live = LIVE.with(|l| {
            l.set(l.get() + by as isize);
            l.get()
        });
        PEAK.with(|p| p.set(p.get().max(live)));
        BIGGEST.with(|b| b.set(b.get().max(by)));
    }
}

fn shrink(by: usize) {
    if ON.with(Cell::get) {
        LIVE.with(|l| l.set(l.get() - by as isize));
    }
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        shrink(l.size());
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new >= l.size() {
            grow(new - l.size());
            // A realloc of a huge block is itself a huge allocation.
            BIGGEST.with(|b| {
                if ON.with(Cell::get) {
                    b.set(b.get().max(new));
                }
            });
        } else {
            shrink(l.size() - new);
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Usage {
    /// Peak bytes live at once, above the level when measuring started.
    pub peak: usize,
    /// Largest single allocation (or realloc target).
    pub biggest: usize,
}

/// Run `f` and report this thread's allocation behaviour while it ran.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Usage) {
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    BIGGEST.with(|b| b.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    let u = Usage {
        peak: PEAK.with(Cell::get).max(0) as usize,
        biggest: BIGGEST.with(Cell::get),
    };
    (r, u)
}

#[cfg(test)]
mod tests {
    use super::measure;

    #[test]
    fn tracker_sees_large_allocations_and_ignores_frees() {
        let (_, u) = measure(|| {
            let big = vec![1u8; 9 << 20];
            let small = vec![0u8; 100];
            std::hint::black_box((&big, &small));
        });
        assert!(u.biggest >= 9 << 20 && u.peak >= 9 << 20, "{u:?}");
        let (_, u) = measure(|| std::hint::black_box(vec![0u8; 10]));
        assert!(u.biggest < 1024, "{u:?}");
    }
}
