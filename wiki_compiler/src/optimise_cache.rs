use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::render::{OXIPNG_PRESET, optimise_png};

/// The name of the cache file.
///
/// It is stored next to the compiler binary, so it stays with the compiler
/// whose measurements it holds. It is the only thing kept between runs.
const CACHE_FILE_NAME: &str = "optimise_cache.csv";

/// The first line of the file: the column names of both row layouts.
///
/// A `png` row has six columns. A `task` row has four. The columns are
/// documented on `OptimiseCache`.
const HEADER: &str = "png,hash,original_bytes,optimised_bytes,millis,runs | task,hash,size,millis";

// The four constants below decide whether oxipng is worth running on a PNG.
// They are applied when the file is read, so changing one takes effect on the
// next run without re-measuring anything.

/// A PNG is not worth optimising again if oxipng saved less than this
/// percentage of its size last time, however long it took.
///
/// This catches PNGs that were already optimised before they reached the wiki.
const SKIP_IF_SAVED_UNDER_PERCENT: u64 = 10;

/// Otherwise, oxipng must save at least this many bytes for every millisecond
/// it takes, or it is not worth running again.
///
/// This scales with image size. A big PNG has to save many bytes to justify its
/// time, while a small one costs almost nothing either way.
///
/// In the run this was tuned on, PNGs worth optimising saved roughly 300 to
/// 1,300 bytes per millisecond, and the two that were not saved about 30 and
/// 40. 150 sits between them. The size and time of every PNG are in the CSV, so
/// it can be tuned from real numbers.
const MIN_BYTES_SAVED_PER_MS: u64 = 150;

/// The bytes-per-millisecond test only applies once oxipng's fastest time on a
/// PNG is at least this long.
///
/// Below it, skipping saves almost no time, so the PNG is just optimised.
const SLOW_ENOUGH_TO_MATTER_MS: u64 = 300;

/// How many times oxipng must have run on a PNG before its time is trusted for
/// the bytes-per-millisecond test.
///
/// oxipng runs on the compiler's own thread pool. A worker waiting on it can
/// pick up other files' work, and that time is then counted against the PNG it
/// was waiting on. For example, a 23 KB PNG was recorded at 467 ms while a
/// similar 17 KB one took 13 ms.
///
/// This noise only makes a time longer, never shorter. The fastest time over
/// several runs therefore approaches the real cost, which is why the file keeps
/// the fastest time and waits for several runs.
///
/// The percentage test does not use time, so it applies from the second run.
const RUNS_BEFORE_TRUSTING_TIME: u64 = 10;

/// Hashes some bytes. Used to identify PNGs and file paths.
///
/// `DefaultHasher` is not guaranteed to give the same result across Rust
/// versions. After a toolchain upgrade nothing in the cache matches and it is
/// rebuilt, which only costs the time the skipped work would have taken.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// Identifies one exact PNG: a hash of its contents, plus its length.
///
/// It is made from the PNG's own bytes, not from the file it came from. An
/// image rendered from an SVGZ can then be tracked by itself, because the
/// SVGZ's size says nothing about how big or compressible the PNG is.
///
/// The answer also follows the image around. Renaming or moving a PNG does not
/// lose it, and a `git checkout` that resets modified times does not discard
/// the cache.
///
/// The length is included to make an accidental hash collision between two
/// different images extremely unlikely.
#[derive(Clone, Copy)]
struct Fingerprint {
    hash: u64,
    len: u64,
}

impl Fingerprint {
    fn of(bytes: &[u8]) -> Self {
        // The preset is part of the hash, so records made under a different
        // preset never match and are measured again.
        let mut hasher = DefaultHasher::new();
        hasher.write(bytes);
        hasher.write_u8(OXIPNG_PRESET);
        Self {
            hash: hasher.finish(),
            len: bytes.len() as u64,
        }
    }

    fn key(self) -> (u64, u64) {
        (self.hash, self.len)
    }
}

/// What the cache knows about one PNG from running oxipng on it.
#[derive(Clone, Copy)]
struct PngRecord {
    /// How big oxipng got the PNG, in bytes.
    optimised: u64,
    /// The fastest oxipng has taken on it, in milliseconds.
    ///
    /// It is the fastest, not the latest. See `RUNS_BEFORE_TRUSTING_TIME`.
    millis: u64,
    /// How many times oxipng has run on it.
    runs: u64,
}

/// Whether `write_optimised` skipped oxipng or ran it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Reused,
    Optimised,
}

/// What `save` did with the file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Nothing changed, so the file was left alone.
    Unchanged,
    Rewritten,
    /// The compiler's location could not be found, so there is nowhere to save.
    NotSaved,
}

/// Counters for this run, updated from every worker thread.
#[derive(Default)]
struct Counters {
    png_skipped: AtomicUsize,
    /// The time oxipng took, when last measured, on the PNGs it was skipped
    /// for.
    skipped_ms: AtomicU64,
    png_optimised: AtomicUsize,
    /// PNGs oxipng ran on that had no record.
    png_new: AtomicUsize,
    /// PNGs whose record got a new fastest time.
    png_faster: AtomicUsize,
    /// Time spent inside oxipng this run, added up across threads.
    oxipng_ms: AtomicU64,
    /// Files and renders whose queue position came from a recorded time.
    timings_known: AtomicUsize,
    /// Files and renders with no recorded time (new, renamed or edited).
    timings_new: AtomicUsize,
    timings_faster: AtomicUsize,
}

/// What the cache did this run, for the summary.
pub struct CacheSummary {
    pub png_skipped: usize,
    pub skipped_ms: u64,
    pub png_optimised: usize,
    pub png_new: usize,
    pub png_faster: usize,
    pub oxipng_ms: u64,
    pub timings_known: usize,
    pub timings_new: usize,
    pub timings_faster: usize,
    /// Rows in the file as it now stands.
    pub rows: usize,
    pub file: FileState,
}

/// What the compiler remembers between runs, loaded from one CSV file at the
/// start and written back at the end.
///
/// It keeps two things.
///
/// Which PNGs oxipng is not worth running on. Every PNG goes through
/// `write_optimised`, whether it was copied from the wiki or rendered from an
/// SVG. A PNG is written as it is, without oxipng, when oxipng did too little
/// for it last time: it saved almost nothing, or too few bytes for the time it
/// took. See the constants at the top of this file. Only sizes and times are
/// stored, never images, so a PNG worth optimising is optimised on every build.
///
/// How long each file takes. This is used to start the slowest files first, so
/// a big file is not left at the end of the queue running alone.
///
/// The file is a CSV. Its first line names the columns of both row layouts,
/// `png` rows (six columns) and `task` rows (four):
///
/// ```csv
/// png,hash,original_bytes,optimised_bytes,millis,runs | task,hash,size,millis
/// png,3f9c2a7e41b8d605,184230,178100,310,3
/// task,b7e10c44a9d2f351,48213,1090
/// ```
///
/// - The first column is the row kind: `png` or `task`.
/// - `hash` identifies a PNG (a hash of its bytes and the oxipng preset), or a
///   task (a hash of the file's path relative to the wiki). An SVG has a task
///   row for itself and one for each PNG it produces.
/// - `original_bytes` and `optimised_bytes` are the PNG's size before and after
///   oxipng.
/// - `size` (task rows) is the source file's size. A different size means the
///   file changed, so its timing is dropped.
/// - `millis` is the fastest time seen: oxipng on a PNG, the whole task for a
///   file, or render plus optimise for one PNG of an SVG. An SVG's own row holds
///   its slowest PNG's time. Sort by this column to find the slow images.
/// - `runs` is how many times oxipng has run on the PNG, up to
///   `RUNS_BEFORE_TRUSTING_TIME`. It only exists on `png` rows.
///
/// A row that does not parse is ignored, so that file is measured again. The
/// bad row is dropped when the file is rewritten.
pub struct OptimiseCache {
    /// `None` if the compiler's own location could not be found.
    ///
    /// The cache still works for this run but is not saved.
    path: Option<PathBuf>,
    /// What the file said about PNGs when this run started. Only read.
    previous_pngs: HashMap<(u64, u64), PngRecord>,
    /// What the file should say about PNGs after this run.
    ///
    /// Records for images that were edited or deleted are never added, so the
    /// file does not grow forever.
    current_pngs: Mutex<HashMap<(u64, u64), PngRecord>>,
    /// How long each file took last run, by `task_id`. Only read.
    previous_tasks: HashMap<u64, TaskRecord>,
    /// How long each file took this run.
    current_tasks: Mutex<HashMap<u64, TaskRecord>>,
    counters: Counters,
}

/// Whether oxipng did so little for this PNG that running it again is not worth
/// the time. It uses whole numbers to avoid floating point.
///
/// The PNG is skipped if it saved less than `SKIP_IF_SAVED_UNDER_PERCENT`, or,
/// once its time is trusted and long enough to matter, if it saved fewer than
/// `MIN_BYTES_SAVED_PER_MS` bytes for every millisecond it took.
///
/// For example, a 336,309 byte PNG that oxipng reduced to 300,537 bytes in a
/// trusted 846 ms saved 35,772 bytes. That is about 42 bytes per millisecond,
/// below 150, so it is skipped.
///
/// A 23,397 byte PNG reduced to 1,666 bytes saved 93%, so it is never skipped.
fn worth_skipping(original: u64, record: PngRecord) -> bool {
    let saved = original.saturating_sub(record.optimised);

    // This is `saved / original < percent / 100` rearranged to avoid division.
    // For example, with a 10% threshold, a 1,000 byte PNG is skipped if it saved
    // fewer than 100 bytes.
    if saved * 100 < original * SKIP_IF_SAVED_UNDER_PERCENT {
        return true;
    }

    // The time is only used once it can be trusted and is long enough for
    // skipping to matter.
    record.runs >= RUNS_BEFORE_TRUSTING_TIME
        && record.millis >= SLOW_ENOUGH_TO_MATTER_MS
        && saved < MIN_BYTES_SAVED_PER_MS * record.millis
}

/// Identifies a file by its path relative to the wiki root, for the timing
/// rows.
///
/// The path is hashed, so a comma in a path cannot break the CSV.
fn task_id(relative_path: &Path) -> u64 {
    hash_bytes(relative_path.to_string_lossy().as_bytes())
}

/// Identifies one PNG of an SVG, for the timing rows: the SVG's path, then the
/// PNG's name.
///
/// The separator is a NUL character, which cannot appear in a path or name. The
/// id therefore cannot equal `task_id` of a path, or the id of a different pair.
fn job_id(svg_path: &Path, name: &str) -> u64 {
    hash_bytes(format!("{}\0{name}", svg_path.to_string_lossy()).as_bytes())
}

/// What the cache knows about how long a file's task, or one PNG of an SVG,
/// takes.
#[derive(Clone, Copy)]
struct TaskRecord {
    /// The source file's size when it was measured.
    ///
    /// A different size now means the file was edited, so the old timing is
    /// discarded.
    size: u64,
    /// The fastest the task has run for a file of this size.
    millis: u64,
}

/// One valid row from the file.
enum Row {
    Png { key: (u64, u64), record: PngRecord },
    Task { id: u64, record: TaskRecord },
}

/// Reads one row, or returns `None` if it cannot be understood.
///
/// The header also returns `None`, since it has nothing to keep.
fn parse_row(line: &str) -> Option<Row> {
    let fields: Vec<&str> = line.split(',').collect();

    // The second column is the hash in both row layouts, written in hex.
    let hash = u64::from_str_radix(fields.get(1)?, 16).ok()?;

    // The row kind and the column count must both match, or the row is treated
    // as damaged.
    match (fields[0], fields.len()) {
        // png,hash,original_bytes,optimised_bytes,millis,runs
        ("png", 6) => {
            let original: u64 = fields[2].parse().ok()?;
            let record = PngRecord {
                optimised: fields[3].parse().ok()?,
                millis: fields[4].parse().ok()?,
                runs: fields[5].parse().ok()?,
            };

            // oxipng never makes a file bigger, so a larger "optimised" size
            // means the row is damaged.
            if record.optimised > original {
                return None;
            }

            // A PNG is identified by its hash and original length together.
            // See `Fingerprint`.
            Some(Row::Png {
                key: (hash, original),
                record,
            })
        }
        // task,hash,size,millis
        ("task", 4) => Some(Row::Task {
            id: hash,
            record: TaskRecord {
                size: fields[2].parse().ok()?,
                millis: fields[3].parse().ok()?,
            },
        }),
        _ => None,
    }
}

/// Times one file's task from creation until it is dropped, then records the
/// result. See `OptimiseCache::time_task`.
pub struct TaskTimer<'a> {
    cache: &'a OptimiseCache,
    id: u64,
    size: u64,
    start: Instant,
}

impl Drop for TaskTimer<'_> {
    fn drop(&mut self) {
        self.cache
            .record_task(self.id, self.size, self.start.elapsed());
    }
}

impl OptimiseCache {
    /// Loads the cache file, or starts empty.
    ///
    /// This never fails. A missing, unreadable or corrupted file means there is
    /// nothing to reuse, and `save` writes a valid file at the end.
    pub fn load() -> Self {
        let path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(CACHE_FILE_NAME)));

        let mut previous_pngs = HashMap::new();
        let mut previous_tasks = HashMap::new();

        if let Some(path) = &path
            && let Ok(text) = fs::read_to_string(path)
        {
            for line in text.lines() {
                match parse_row(line) {
                    Some(Row::Png { key, record }) => {
                        previous_pngs.insert(key, record);
                    }
                    Some(Row::Task { id, record }) => {
                        previous_tasks.insert(id, record);
                    }
                    // The header, blank lines, and rows that cannot be
                    // understood.
                    None => {}
                }
            }
        }

        Self {
            path,
            previous_pngs,
            current_pngs: Mutex::new(HashMap::new()),
            previous_tasks,
            current_tasks: Mutex::new(HashMap::new()),
            counters: Counters::default(),
        }
    }

    /// Writes `png_bytes` to `output_path` as an optimised PNG, skipping oxipng
    /// when an earlier run found it was not worth running on this exact PNG.
    ///
    /// Every PNG the compiler produces goes through here. The result says which
    /// of the two happened, for the summary.
    pub fn write_optimised(
        &self,
        png_bytes: &[u8],
        output_path: &Path,
    ) -> Result<Outcome, Box<dyn Error>> {
        let fingerprint = Fingerprint::of(png_bytes);

        // The bytes are written first in both cases, because oxipng works on
        // the file in place.
        fs::write(output_path, png_bytes)?;

        if let Some(skipped_ms) = self.already_optimised(fingerprint) {
            self.counters.png_skipped.fetch_add(1, Ordering::Relaxed);
            self.counters
                .skipped_ms
                .fetch_add(skipped_ms, Ordering::Relaxed);
            return Ok(Outcome::Reused);
        }

        let oxipng_start = Instant::now();
        optimise_png(output_path)?;
        let millis = oxipng_start.elapsed().as_millis() as u64;
        self.counters.png_optimised.fetch_add(1, Ordering::Relaxed);
        self.counters.oxipng_ms.fetch_add(millis, Ordering::Relaxed);

        // Record the new size and time so the next run can decide whether
        // oxipng is worth running again.
        self.record(fingerprint, fs::metadata(output_path)?.len(), millis);
        Ok(Outcome::Optimised)
    }

    /// Whether an earlier run found oxipng was not worth running on this PNG.
    ///
    /// If so, it returns how long oxipng took on it, which is the time that
    /// skipping avoids.
    fn already_optimised(&self, fingerprint: Fingerprint) -> Option<u64> {
        match self.previous_pngs.get(&fingerprint.key()) {
            Some(&record) if worth_skipping(fingerprint.len, record) => {
                // The record is carried over unchanged, so the file keeps the
                // numbers it was judged on instead of re-measuring them.
                self.current_pngs
                    .lock()
                    .unwrap()
                    .insert(fingerprint.key(), record);
                Some(record.millis)
            }
            _ => None,
        }
    }

    /// Records how small oxipng got this PNG, and how long it took.
    ///
    /// If oxipng has run on this PNG before, the fastest time is kept and the
    /// run count goes up. See `RUNS_BEFORE_TRUSTING_TIME`.
    fn record(&self, fingerprint: Fingerprint, optimised_len: u64, millis: u64) {
        // oxipng never makes a file bigger. If it somehow did, there is nothing
        // sensible to record.
        if optimised_len > fingerprint.len {
            return;
        }

        let key = fingerprint.key();

        // The same bytes always cost the same to optimise, so the fastest run is
        // the best estimate and anything slower was noise. A changed image has
        // different bytes and starts a new record.
        //
        // `runs` stops counting at `RUNS_BEFORE_TRUSTING_TIME`, so a settled row
        // stops changing and the file is not rewritten.
        let (millis, runs) = match self.previous_pngs.get(&key) {
            Some(previous) => {
                if millis < previous.millis {
                    self.counters.png_faster.fetch_add(1, Ordering::Relaxed);
                }
                (
                    millis.min(previous.millis),
                    (previous.runs + 1).min(RUNS_BEFORE_TRUSTING_TIME),
                )
            }
            None => {
                self.counters.png_new.fetch_add(1, Ordering::Relaxed);
                (millis, 1)
            }
        };

        self.current_pngs.lock().unwrap().insert(
            key,
            PngRecord {
                optimised: optimised_len,
                millis,
                runs,
            },
        );
    }

    /// The fastest time recorded for `id` at this size, if there is one.
    ///
    /// It also counts whether the queue position it is used for was known.
    fn lookup(&self, id: u64, size: u64) -> Option<u64> {
        let found = self
            .previous_tasks
            .get(&id)
            // A record at a different size belongs to an older version of the
            // file.
            .filter(|record| record.size == size)
            .map(|record| record.millis);
        let counter = if found.is_some() {
            &self.counters.timings_known
        } else {
            &self.counters.timings_new
        };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    /// The fastest this file's task has run, if it was measured before and is
    /// still the same size.
    ///
    /// For an SVG this is its slowest PNG. See `set_task`.
    pub fn last_task_millis(&self, relative_path: &Path, size: u64) -> Option<u64> {
        self.lookup(task_id(relative_path), size)
    }

    /// The fastest one PNG of an SVG has taken to render and optimise, if it
    /// was measured before and the SVG is still the same size.
    ///
    /// `name` is the PNG's name, which tells the SVG's PNGs apart.
    pub fn last_job_millis(&self, svg_path: &Path, name: &str, svg_size: u64) -> Option<u64> {
        self.lookup(job_id(svg_path, name), svg_size)
    }

    /// Starts timing a file's task.
    ///
    /// The time is recorded when the returned timer is dropped. That happens on
    /// every exit from the task, including early returns, so none can forget
    /// to record it.
    pub fn time_task(&self, relative_path: &Path, size: u64) -> TaskTimer<'_> {
        TaskTimer {
            cache: self,
            id: task_id(relative_path),
            size,
            start: Instant::now(),
        }
    }

    /// Records how long one PNG of an SVG took to render and optimise, and
    /// returns the fastest time now on record for it.
    pub fn record_job(&self, svg_path: &Path, name: &str, svg_size: u64, elapsed: Duration) -> u64 {
        self.record_task(job_id(svg_path, name), svg_size, elapsed)
    }

    /// Sets an SVG's own row, which is the time of its slowest PNG.
    ///
    /// An SVG's PNGs are separate jobs that cannot be known until the SVG is
    /// parsed. For the slowest PNG to start early, the SVG must be parsed
    /// early, so the SVG is queued by that PNG's time.
    pub fn set_task(&self, relative_path: &Path, size: u64, millis: u64) {
        self.current_tasks
            .lock()
            .unwrap()
            .insert(task_id(relative_path), TaskRecord { size, millis });
    }

    /// Records how long a task took, keeping the fastest time seen for a file
    /// of this size, and returns what is now on record.
    ///
    /// A worker waiting on its own sub-work can pick up other files' tasks,
    /// which are then counted against it. One run's time can therefore only be
    /// too long, never too short, so the fastest is closest to the real cost.
    ///
    /// A file whose size changed starts over.
    fn record_task(&self, id: u64, size: u64, elapsed: Duration) -> u64 {
        let observed = elapsed.as_millis() as u64;
        let millis = match self.previous_tasks.get(&id) {
            Some(previous) if previous.size == size => {
                if observed < previous.millis {
                    self.counters.timings_faster.fetch_add(1, Ordering::Relaxed);
                }
                observed.min(previous.millis)
            }
            // No earlier record, or the file changed size, so this run is the
            // only measurement.
            _ => observed,
        };
        self.current_tasks
            .lock()
            .unwrap()
            .insert(id, TaskRecord { size, millis });
        millis
    }

    /// What the cache did this run. `file` is what `save` returned.
    pub fn summary(&self, file: FileState) -> CacheSummary {
        let counters = &self.counters;
        CacheSummary {
            png_skipped: counters.png_skipped.load(Ordering::Relaxed),
            skipped_ms: counters.skipped_ms.load(Ordering::Relaxed),
            png_optimised: counters.png_optimised.load(Ordering::Relaxed),
            png_new: counters.png_new.load(Ordering::Relaxed),
            png_faster: counters.png_faster.load(Ordering::Relaxed),
            oxipng_ms: counters.oxipng_ms.load(Ordering::Relaxed),
            timings_known: counters.timings_known.load(Ordering::Relaxed),
            timings_new: counters.timings_new.load(Ordering::Relaxed),
            timings_faster: counters.timings_faster.load(Ordering::Relaxed),
            rows: self.current_pngs.lock().unwrap().len()
                + self.current_tasks.lock().unwrap().len(),
            file,
        }
    }

    /// Writes the cache back out, if anything in it changed.
    ///
    /// Times only move down, so once they settle the file stops changing.
    ///
    /// The file is written to a temporary file and then renamed over the real
    /// one. A rename replaces the old file in one step, so a crash or a full
    /// disk can only leave the old complete file, never a half-written one.
    pub fn save(&self) -> Result<FileState, String> {
        let Some(path) = &self.path else {
            return Ok(FileState::NotSaved);
        };

        let mut text = String::from(HEADER);
        text.push('\n');

        // Rows are sorted so the file is the same on every run, instead of
        // following the order the thread pool finished in.
        let pngs = self.current_pngs.lock().unwrap();
        let mut png_rows: Vec<_> = pngs.iter().collect();
        png_rows.sort_by_key(|(key, _)| **key);
        for (&(hash, original), &record) in png_rows {
            text.push_str(&format!(
                "png,{hash:016x},{original},{},{},{}\n",
                record.optimised, record.millis, record.runs
            ));
        }

        let tasks = self.current_tasks.lock().unwrap();
        let mut task_rows: Vec<_> = tasks.iter().collect();
        task_rows.sort_by_key(|(id, _)| **id);
        for (&id, &record) in task_rows {
            text.push_str(&format!(
                "task,{id:016x},{},{}\n",
                record.size, record.millis
            ));
        }

        // Leave the file alone if nothing changed since it was read.
        if fs::read_to_string(path).is_ok_and(|existing| existing == text) {
            return Ok(FileState::Unchanged);
        }

        let temp = path.with_extension("csv.tmp");
        fs::write(&temp, text).map_err(|e| format!("couldn't write {}: {e}", temp.display()))?;
        fs::rename(&temp, path).map_err(|e| format!("couldn't replace {}: {e}", path.display()))?;
        Ok(FileState::Rewritten)
    }
}
