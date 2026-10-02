use std::sync::atomic::AtomicUsize;

/// Tallies of what the compiler actually did during a run, printed as a
/// summary once the whole build finishes.
///
/// Every field is an `AtomicUsize` rather than the struct being wrapped in
/// a single `Mutex`. The compiler processes every file on a shared rayon
/// thread pool, so dozens of threads bump these counters at once; a mutex
/// would force every single increment to wait its turn for a lock it
/// doesn't actually need, since the fields never need to change together
/// atomically - each one only ever needs `fetch_add(1, ...)` applied to
/// itself.
#[derive(Default)]
pub struct Stats {
    pub data_json: AtomicUsize,
    pub genlists: AtomicUsize,
    pub colors_exported: AtomicUsize,
    pub svgs_rendered: AtomicUsize,
    pub hidden_svgs: AtomicUsize,
    pub pngs_copied: AtomicUsize,
    pub errors: AtomicUsize,
    pub conflicts: AtomicUsize,
}
