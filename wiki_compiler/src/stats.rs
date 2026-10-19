use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

/// One failure recorded during a run.
///
/// Errors are collected and printed together at the end of the build.
/// Printed immediately, they would scroll away among the status lines of the
/// other files.
pub struct ErrorRecord {
    /// The file to fix, relative to the wiki root.
    pub location: String,
    pub message: String,
}

/// Counts of what the compiler did during a run, printed as a summary at the
/// end of the build.
///
/// The counters are atomics because many threads on the rayon pool increment
/// them at the same time. Each counter is independent, so a single `Mutex`
/// around the struct would only make threads wait for no reason.
///
/// The error list is the exception. Each entry holds strings, which cannot be
/// atomic, so it uses a `Mutex`. Errors are rare, so the lock is never
/// contended.
#[derive(Default)]
pub struct Stats {
    pub data_json: AtomicUsize,
    pub genlists: AtomicUsize,
    pub colors_exported: AtomicUsize,
    pub svgs_rendered: AtomicUsize,
    pub hidden_svgs: AtomicUsize,
    pub pngs_copied: AtomicUsize,
    pub optimise_skipped: AtomicUsize,
    pub conflicts: AtomicUsize,
    errors: Mutex<Vec<ErrorRecord>>,
}

impl Stats {
    /// Records a failure against `location`, the file that needs fixing.
    ///
    /// For example, a genlist with a missing comma is recorded as:
    ///
    /// ```text
    /// location: "Main Window/Settings/Image/GenList.json"
    /// message:  "expected `,` or `}` at line 5 column 3"
    /// ```
    pub fn record_error(&self, location: impl Into<String>, message: impl ToString) {
        self.errors.lock().unwrap().push(ErrorRecord {
            location: location.into(),
            message: message.to_string(),
        });
    }

    /// Takes every recorded error, sorted by location.
    ///
    /// Files finish in whatever order the thread pool runs them. Sorting keeps
    /// the report in the same order on every run.
    pub fn take_errors(&self) -> Vec<ErrorRecord> {
        let mut errors = std::mem::take(&mut *self.errors.lock().unwrap());
        errors.sort_by(|a, b| a.location.cmp(&b.location));
        errors
    }
}
