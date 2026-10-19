use owo_colors::OwoColorize;
use rayon::{ScopeFifo, ThreadPoolBuilder};
use serde_json::{Value, to_string};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

use crate::crop_generation::{
    RenderJob, SvgPlan, build_report, claim_output, find_genlist_for_svg, find_main_svg,
    load_genlist, plan_svg,
};
use crate::optimise_cache::{FileState, OptimiseCache, Outcome};
use crate::output::{FileKind, format_duration, log_genlist, log_processed, relative_label};
use crate::reference_check::check_and_warn_if_unreferenced;
use crate::registry::write_registry_to_json;
use crate::render::render_tree_to_png;
use crate::scheduler::{Priority, Scheduler};
use crate::stats::Stats;

/// The folder a `GenList.json` sits in, as a path relative to the wiki
/// root ("." when it's sitting in the root itself).
///
/// Shown on its log line specifically because every single folder's
/// `GenList.json` shares that exact same filename - a log line that just
/// said `"GenList.json"` would be completely indistinguishable from every
/// other folder's `GenList.json` line, so its folder is what actually
/// identifies which one this is.
fn genlist_folder(relative_path: &Path) -> String {
    match relative_path.parent() {
        // Has a parent and it's not empty - use it
        Some(p) if !p.as_os_str().is_empty() => p.display().to_string(),
        // No parent, or an empty one - GenList.json is sitting at the wiki root itself
        _ => ".".to_string(),
    }
}

/// What every job needs, bundled so a job takes one handle instead of six.
struct Shared {
    source_dir: Arc<PathBuf>,
    target_dir: Arc<PathBuf>,
    warnings: Arc<Mutex<Vec<PathBuf>>>,
    stats: Arc<Stats>,
    claims: Arc<Mutex<HashMap<PathBuf, Vec<String>>>>,
    optimise_cache: Arc<OptimiseCache>,
}

/// How long one PNG job took, and whether it succeeded or failed with a message.
type JobReport = (Duration, Result<Outcome, String>);

/// One SVG while its PNGs are being rendered. Each PNG is a job of its own,
/// so they're finished one at a time, by whichever threads get to them; this
/// holds what they report until the last one is in.
struct SvgBatch {
    plan: SvgPlan,
    /// What each job reported, in the same order as `plan.jobs`
    results: Mutex<Vec<Option<JobReport>>>,
    /// How many jobs haven't reported yet. Whoever brings it to zero logs
    /// the SVG
    remaining: AtomicUsize,
    /// The slowest PNG's time on record, which becomes the SVG's own row
    slowest_millis: AtomicU64,
    /// When the SVG's task started, for the time on its log line
    start: Instant,
    /// Shown on its log line, and used to say which file any error is about
    location: String,
    relative_path: PathBuf,
    /// The SVG file's size, which its timings are recorded against
    size: u64,
    /// `None` if it has no genlist; otherwise whether it's the folder
    /// default (`GenList.json`) rather than a named pair
    genlist: Option<bool>,
}

/// Handles one SVG/SVGZ source file: works out its PNGs - the full image,
/// and its genlist's colour crops if it has one - and queues each one as a
/// job of its own.
///
/// The PNGs are separate jobs, rather than being rendered together inside
/// the SVG's task, so that each one is queued by how long it takes. In a
/// folder where one crop takes 600 ms and the rest take 20, the slow one
/// starts at the very front, alongside the other slowest files in the wiki,
/// instead of being held up behind its SVG's cheap ones or the SVG's task as
/// a whole. They still all come from one parse.
///
/// A leading underscore on the file name marks it as hidden: it exists
/// purely to be sliced up by its genlist, and was never meant to appear as
/// its own standalone image on any page. So the full image is skipped, and
/// so is the unreferenced-image check, since that only applies to images
/// that are meant to be on a page. A hidden SVG with no genlist has nothing
/// to produce at all.
///
/// A genlist that won't load (malformed JSON, say) is reported against the
/// JSON file - that's the one to go and fix - but doesn't stop the full
/// image from being rendered.
fn start_svg<'scope>(
    file_path: &Path,
    relative_path: &Path,
    target_path: &Path,
    size: u64,
    shared: &Arc<Shared>,
    scheduler: &Arc<Scheduler<'scope>>,
    scope: &ScopeFifo<'scope>,
) {
    let Some(batch) = plan_svg_batch(file_path, relative_path, target_path, size, shared) else {
        // Nothing to render, so nothing could be slow. Still recorded, so
        // that the file isn't treated as unknown on every run
        shared.optimise_cache.set_task(relative_path, size, 0);
        return;
    };
    let batch = Arc::new(batch);
    let count = batch.plan.jobs.len();

    // A genlist with no matching colours: nothing to wait for
    if count == 0 {
        finish_svg(&batch, shared);
        return;
    }

    for index in 0..count {
        let job = &batch.plan.jobs[index];
        let area = job.area(&batch.plan.tree);
        // By the fastest this PNG has taken, if it has been timed at this
        // SVG size; otherwise by its pixel area, ahead of the timed ones
        let priority = match shared
            .optimise_cache
            .last_job_millis(relative_path, &job.name, size)
        {
            Some(millis) => Priority {
                unknown: false,
                cost: millis,
                size: area,
            },
            None => Priority {
                unknown: true,
                cost: area,
                size: area,
            },
        };

        let batch = Arc::clone(&batch);
        let shared = Arc::clone(shared);
        scheduler.submit(scope, priority, move |_| render_one(&batch, index, &shared));
    }
}

/// Parses an SVG, loads its genlist and works out its jobs - everything
/// `start_svg` does before queueing anything. `None` means there's nothing
/// to render, whether because there never was or because of an error, which
/// is recorded.
fn plan_svg_batch(
    file_path: &Path,
    relative_path: &Path,
    target_path: &Path,
    size: u64,
    shared: &Shared,
) -> Option<SvgBatch> {
    let stats = &shared.stats;
    let start = Instant::now();
    let location = relative_path.display().to_string();

    let hidden = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('_'));
    if hidden {
        stats.hidden_svgs.fetch_add(1, Ordering::Relaxed);
    }

    let genlist_path = find_genlist_for_svg(file_path);

    // A hidden SVG with no genlist has nothing to produce
    if hidden && genlist_path.is_none() {
        return None;
    }

    if !hidden && let Err(e) = check_and_warn_if_unreferenced(file_path, &shared.warnings) {
        stats.record_error(
            &location,
            format!("couldn't check its data.json reference: {e}"),
        );
    }

    // The genlist's colour map, if this SVG has one and it loads. A broken
    // genlist is recorded against the JSON file itself and treated as if
    // there were no genlist, so the full image still gets rendered
    let color_map = genlist_path
        .as_ref()
        .and_then(|path| match load_genlist(path) {
            Ok(color_map) => Some(color_map),
            Err(e) => {
                // The genlist sits in the same folder as its SVG, so its path
                // relative to the wiki root is the SVG's with the file name
                // swapped
                let genlist_location = path
                    .file_name()
                    .map(|name| relative_path.with_file_name(name).display().to_string())
                    .unwrap_or_else(|| location.clone());
                stats.record_error(genlist_location, e);
                None
            }
        });

    // Hidden, and its genlist didn't load: nothing left to produce
    if hidden && color_map.is_none() {
        return None;
    }

    let Some(target_parent) = target_path.parent() else {
        stats.record_error(&location, "output path has no parent folder");
        return None;
    };

    let plan = match plan_svg(
        file_path,
        color_map.as_ref(),
        target_parent,
        &shared.claims,
        &location,
    ) {
        Ok(plan) => plan,
        Err(e) => {
            stats.record_error(&location, e);
            return None;
        }
    };

    // The colour map is only used for a genlist's log line, so what's left of
    // it is whether there is one and where it lives
    let genlist = color_map.as_ref().map(|_| {
        genlist_path
            .as_deref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "GenList.json")
    });

    let count = plan.jobs.len();
    Some(SvgBatch {
        plan,
        results: Mutex::new((0..count).map(|_| None).collect()),
        remaining: AtomicUsize::new(count),
        slowest_millis: AtomicU64::new(0),
        start,
        location,
        relative_path: relative_path.to_path_buf(),
        size,
        genlist,
    })
}

/// Renders one PNG of an SVG, and if it's the last one to finish, logs the SVG.
fn render_one(batch: &SvgBatch, index: usize, shared: &Shared) {
    let job: &RenderJob = &batch.plan.jobs[index];
    let started = Instant::now();
    // The error is turned into a String because `Box<dyn Error>` can't be
    // sent between threads
    let result = render_tree_to_png(
        &batch.plan.tree,
        &job.output_path,
        job.crop,
        &shared.optimise_cache,
    )
    .map_err(|e| e.to_string());
    let elapsed = started.elapsed();

    // Recorded against this one PNG, so the next run queues it by its own
    // time and not by its SVG's
    if result.is_ok() {
        let recorded =
            shared
                .optimise_cache
                .record_job(&batch.relative_path, &job.name, batch.size, elapsed);
        batch.slowest_millis.fetch_max(recorded, Ordering::Relaxed);
    }

    batch.results.lock().unwrap()[index] = Some((elapsed, result));

    // AcqRel so that whoever sees zero also sees every other job's result
    if batch.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
        finish_svg(batch, shared);
    }
}

/// Counts and logs an SVG once every one of its PNGs has reported.
fn finish_svg(batch: &SvgBatch, shared: &Shared) {
    let stats = &shared.stats;
    let results = std::mem::take(&mut *batch.results.lock().unwrap())
        .into_iter()
        .map(|result| result.expect("every job has reported before the last one finishes"))
        .collect();
    let report = build_report(&batch.plan.jobs, results);

    // The SVG's own row is its slowest PNG: that's how early it has to be
    // started for that PNG to be able to start early
    shared.optimise_cache.set_task(
        &batch.relative_path,
        batch.size,
        batch.slowest_millis.load(Ordering::Relaxed),
    );

    // Recorded against the SVG, since a PNG that failed to render has no
    // JSON to blame
    for failure in &report.failures {
        stats.record_error(&batch.location, failure);
    }
    if report.full_image {
        stats.svgs_rendered.fetch_add(1, Ordering::Relaxed);
    }
    stats
        .optimise_skipped
        .fetch_add(report.reused, Ordering::Relaxed);

    if let Some(is_default) = batch.genlist {
        stats.genlists.fetch_add(1, Ordering::Relaxed);
        stats
            .colors_exported
            .fetch_add(report.crops, Ordering::Relaxed);

        // The folder default is labelled by its folder, a named pair by the
        // image it belongs to - the [GENLIST CUSTOM] tag already implies
        // "and its .json partner", so the image is the more useful name
        let (kind, label) = if is_default {
            (FileKind::Genlist, genlist_folder(&batch.relative_path))
        } else {
            (FileKind::GenlistCustom, batch.location.clone())
        };
        // The header is the total of every line under it, not the time on
        // the clock - they're rendered at the same time, so the clock time
        // is just the slowest one
        log_genlist(
            kind,
            &label,
            report.crops,
            report.total_time,
            &report.detail_lines,
        );
    } else if report.full_image {
        // A plain SVG is one PNG, so it's skipped if that one was
        log_processed(
            FileKind::Svg,
            &batch.location,
            batch.start.elapsed(),
            report.reused > 0,
        );
    }
}

/// Walks the wiki source tree and rebuilds the compiled Wiki output from
/// scratch.
pub fn recreate_directory_structure(
    source_dir: &Arc<PathBuf>,
    target_dir: &Arc<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    // Wipe any previous build rather than merge new output into it - a
    // merge would leave behind output from a source file that's since been
    // renamed or deleted, since nothing would ever remove output for a
    // source file that's no longer there to trigger its own removal
    if target_dir.exists() {
        fs::remove_dir_all(target_dir.as_path())?;
    }

    // Walked once, up front, into a plain Vec rather than processed file by
    // file as the walk happens. The parallel loop below spawns one rayon
    // task per entry in this list, so it needs a concrete, already-known
    // collection to spawn tasks against - the walker's own iterator is
    // lazy and single-threaded, and isn't something rayon can spread across
    // its thread pool on its own.
    let mut relevant_files: Vec<(PathBuf, u64)> = WalkDir::new(source_dir.as_path())
        .into_iter()
        // filter_entry prunes a whole subtree before ever descending into
        // it. Filtering by name only in filter_map below, as this used to
        // work, would only ever exclude the directory entry itself from
        // the final list - WalkDir would still walk everything inside a
        // ".git" or "target" directory looking for matching files, wasting
        // time on huge folders (a Rust build's target/ directory especially)
        // that could never contain anything relevant anyway.
        .filter_entry(|entry| {
            // Keep everything except a ".git" or "target" directory
            !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name == ".git" || name == "target")
        })
        .filter_map(|entry| {
            // A single unreadable entry (permissions, a broken symlink,
            // etc.) shouldn't be allowed to abort the whole build over one
            // file - just skip that one and keep walking
            let entry = entry.ok()?;
            let path = entry.path();

            // Only files (not directories) this compiler actually does
            // something with. A named genlist (Cache.json) isn't listed on
            // purpose: it's found from the SVG it belongs to (see
            // start_svg), so queueing it as a task of its own would just
            // be a task with nothing to do.
            if path.is_file()
                && let Some(file_name) = path.file_name().and_then(|f| f.to_str())
                && (file_name == "data.json"
                    || file_name == "GenList.json"
                    || file_name.ends_with(".png")
                    || file_name.ends_with(".svg")
                    || file_name.ends_with(".svgz"))
            {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                return Some((path.to_path_buf(), size));
            }

            None
        })
        .collect();

    // What previous runs learned about the wiki - which PNGs oxipng has
    // nothing left to do for, and how long each file took. Loaded here,
    // before the queue is ordered, because the ordering uses it. See
    // OptimiseCache
    let optimise_cache = Arc::new(OptimiseCache::load());

    // One worker thread per CPU core, so the parallel pass below actually
    // uses every core available rather than defaulting to some fixed count
    let pool = ThreadPoolBuilder::new()
        .num_threads(num_cpus::get())
        .build()
        .unwrap();

    // Every image whose PNG never turned up anywhere in its sibling data.json
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let stats = Arc::new(Stats::default());

    // Tracks which source(s) claimed which output path - see claim_output's
    // doc comment for the full explanation of how this catches two
    // different sources fighting over the same output filename
    let claims: Arc<Mutex<HashMap<PathBuf, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));

    let shared = Arc::new(Shared {
        source_dir: Arc::clone(source_dir),
        target_dir: Arc::clone(target_dir),
        warnings: Arc::clone(&warnings),
        stats: Arc::clone(&stats),
        claims: Arc::clone(&claims),
        optimise_cache: Arc::clone(&optimise_cache),
    });

    // Where the slowest-first order is decided. Every job - a PNG, a
    // data.json, an SVG, and each PNG an SVG produces - goes through this
    // one queue, and whenever a thread is free it takes the most urgent job
    // waiting at that moment. The build can't finish before its slowest job
    // does, and a big PNG or SVG crop can take hundreds of milliseconds - if
    // one of those is started last, everything else finishes and it runs on
    // its own at the end. Started first, the many small jobs fill in around
    // it instead. See Scheduler.
    //
    // A job that has been timed before (and is still the same size) goes by
    // the fastest time it has ever taken. One that hasn't (new, renamed or
    // edited) has no timing, so it goes before all of those, biggest first -
    // it could be anything, and its size is a fair stand-in for how long it
    // will take. An SVG is queued by its slowest PNG, since it has to be
    // parsed before that PNG can start. Ties go to the file that sorts
    // first by path, so the order is the same on every run.
    relevant_files.sort();
    let scheduler = Scheduler::new();

    let start = Instant::now();
    // scope_fifo() blocks until every task spawned inside this closure has
    // finished - including ones spawned by other tasks, such as an SVG's
    // PNGs - so none of the summary/reporting code after this call can ever
    // run ahead of the files it's about to report on
    {
        let shared = Arc::clone(&shared);
        let scheduler = Arc::clone(&scheduler);
        pool.scope_fifo(move |scope| {
            for (file_path, size) in relevant_files {
                let shared = Arc::clone(&shared);
                let scheduler_for_job = Arc::clone(&scheduler);

                let relative = file_path
                    .strip_prefix(shared.source_dir.as_path())
                    .unwrap_or(file_path.as_path());
                let priority = match shared.optimise_cache.last_task_millis(relative, size) {
                    Some(millis) => Priority {
                        unknown: false,
                        cost: millis,
                        size,
                    },
                    None => Priority {
                        unknown: true,
                        cost: size,
                        size,
                    },
                };

                scheduler.submit(scope, priority, move |scope| {
                    // Borrowed from the shared handle, under the names the
                    // code below has always used
                    let source_dir = &shared.source_dir;
                    let target_dir = &shared.target_dir;
                    let warnings = &shared.warnings;
                    let stats = &shared.stats;
                    let claims = &shared.claims;
                    let optimise_cache = &shared.optimise_cache;
                    let scheduler = &scheduler_for_job;
                    let file_path = &file_path;

                    // Where this file sits under source_dir, e.g. "Settings/Main.svgz"
                    let relative_path = match file_path.strip_prefix(source_dir.as_path()) {
                        Ok(path) => path,
                        Err(e) => {
                            stats.record_error(file_path.display().to_string(), e);
                            return;
                        }
                    };
                    // Times this file's whole task, however it ends (the many early
                    // returns below included), so the next run knows how long it
                    // took - see OptimiseCache::time_task. An SVG isn't timed as a
                    // whole: its PNGs are jobs of their own, see start_svg
                    let is_svg = file_path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.ends_with(".svg") || f.ends_with(".svgz"));
                    let _timer = (!is_svg).then(|| optimise_cache.time_task(relative_path, size));

                    // Mirror that same relative path onto the output directory,
                    // so the output tree's shape matches the source tree's
                    let target_path = target_dir.join(relative_path);
                    let target_parent = target_path.parent().unwrap();

                    // Where this file lives, shown on its log line so
                    // identically-named files (data.json, Main.svgz) in
                    // different folders can be told apart from one another
                    let location = relative_path.display().to_string();

                    if let Some(file_name) = file_path.file_name().and_then(|f| f.to_str()) {
                        match file_name {
                            "data.json" => {
                                let start = Instant::now();

                                // Read the page's data.json as text, then parse it as JSON.
                                // A failure is recorded against this file rather than
                                // panicking - a panic only says where in the compiler's
                                // source it happened, not which data.json was malformed
                                let json_content = match fs::read_to_string(file_path) {
                                    Ok(content) => content,
                                    Err(e) => {
                                        stats.record_error(
                                            &location,
                                            format!("couldn't read file: {e}"),
                                        );
                                        return;
                                    }
                                };
                                let json_value: Value = match serde_json::from_str(&json_content) {
                                    Ok(value) => value,
                                    Err(e) => {
                                        stats.record_error(&location, format!("invalid JSON: {e}"));
                                        return;
                                    }
                                };

                                // Re-serialised rather than just copied byte-for-byte, so the
                                // shipped data.json is minified - a smaller download for
                                // whoever's viewing the Wiki, and any whitespace/formatting
                                // quirks left over from hand-editing don't leak into the output
                                let minified_json = match to_string(&json_value) {
                                    Ok(json) => json,
                                    Err(e) => {
                                        stats.record_error(
                                            &location,
                                            format!("couldn't minify: {e}"),
                                        );
                                        return;
                                    }
                                };

                                // data.json is always relevant, so its directory can be created
                                // unconditionally - unlike a genlist, there's no "might turn out
                                // not to apply" case to gate this behind
                                if let Err(e) = fs::create_dir_all(target_parent) {
                                    stats.record_error(
                                        &location,
                                        format!("couldn't create output folder: {e}"),
                                    );
                                    return;
                                }

                                // Write the minified JSON out to the mirrored output path
                                if let Err(e) = fs::write(&target_path, minified_json) {
                                    stats.record_error(
                                        &location,
                                        format!("couldn't write output: {e}"),
                                    );
                                } else {
                                    stats.data_json.fetch_add(1, Ordering::Relaxed);
                                }

                                log_processed(FileKind::Data, &location, start.elapsed(), false);
                            }
                            "GenList.json" => {
                                // The genlist itself is applied by the Main.svg/Main.svgz it
                                // belongs to (see start_svg) - all that's left to do here is
                                // make sure there is one, otherwise this GenList.json would
                                // just never get used, with nothing to say so
                                if let Err(e) = find_main_svg(file_path) {
                                    stats.record_error(&location, e);
                                }
                            }
                            file if file.ends_with(".png") => {
                                if let Err(e) = check_and_warn_if_unreferenced(file_path, warnings)
                                {
                                    stats.record_error(
                                        &location,
                                        format!("couldn't check its data.json reference: {e}"),
                                    );
                                }

                                let start = Instant::now();

                                if let Err(e) = fs::create_dir_all(target_parent) {
                                    stats.record_error(
                                        &location,
                                        format!("couldn't create output folder: {e}"),
                                    );
                                    return;
                                }

                                claim_output(
                                    claims,
                                    &target_path,
                                    format!("direct copy of '{}'", relative_path.display()),
                                );

                                // Read the source PNG and write it out optimised. If this
                                // exact PNG has been through oxipng on an earlier run, that
                                // result is reused instead of running oxipng again - see
                                // OptimiseCache
                                let mut skipped = false;
                                match fs::read(file_path) {
                                    Ok(bytes) => {
                                        match optimise_cache.write_optimised(&bytes, &target_path) {
                                            Ok(outcome) => {
                                                stats.pngs_copied.fetch_add(1, Ordering::Relaxed);
                                                if outcome == Outcome::Reused {
                                                    skipped = true;
                                                    stats
                                                        .optimise_skipped
                                                        .fetch_add(1, Ordering::Relaxed);
                                                }
                                            }
                                            Err(e) => {
                                                stats.record_error(
                                                    &location,
                                                    format!("couldn't optimise: {e}"),
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        stats
                                            .record_error(&location, format!("couldn't read: {e}"));
                                    }
                                }

                                log_processed(FileKind::Png, &location, start.elapsed(), skipped);
                            }
                            file if file.ends_with(".svg") || file.ends_with(".svgz") => {
                                // Everything an SVG is responsible for - the full image, and
                                // its genlist's crops if it has one - is handled in one
                                // place: parsed here, each PNG then queued as a job
                                start_svg(
                                    file_path,
                                    relative_path,
                                    &target_path,
                                    size,
                                    &shared,
                                    scheduler,
                                    scope,
                                );
                            }
                            // Some other file matched the earlier filter but doesn't have a
                            // specific handler above - nothing to do
                            _ => {}
                        }
                    }
                });
            }
        });
    }

    // Keep what this run learned about the PNGs for next time. A failure is
    // reported but doesn't affect the build - the cache is only a shortcut
    let cache_file = match optimise_cache.save() {
        Ok(state) => state,
        Err(message) => {
            stats.record_error("PNG optimise cache", message);
            FileState::NotSaved
        }
    };
    let cache = optimise_cache.summary(cache_file);

    // Print every image whose PNG never turned up in its sibling data.json
    let warnings = warnings.lock().unwrap();
    if !warnings.is_empty() {
        println!();
        println!(
            "{}",
            "⚠ Images not referenced in their sibling data.json:"
                .yellow()
                .bold()
        );
        for path in warnings.iter() {
            let relative_path = relative_label(path, source_dir.as_path());
            println!("  {} {}", "•".yellow(), relative_path);
        }
    }

    // Every failure recorded during the run, listed together here - right
    // above the summary, where it can't scroll away - with the file each
    // one is about. take_errors sorts by location so the order is the same
    // on every run, however the thread pool happened to schedule things
    let errors = stats.take_errors();
    if !errors.is_empty() {
        println!();
        eprintln!("{}", "✗ Errors:".red().bold());
        for record in &errors {
            eprintln!("  {} {}", "→".red(), record.location.bold());
            eprintln!("      {} {}", "•".dimmed(), record.message);
        }
    }

    // Every output path more than one source claimed - see claim_output's
    // doc comment for the mechanism behind this
    let claims = claims.lock().unwrap();
    let mut printed_conflict_header = false;
    for (output_path, sources) in claims.iter() {
        // Fewer than 2 claims on this path means no conflict, nothing to report
        if sources.len() < 2 {
            continue;
        }

        stats.conflicts.fetch_add(1, Ordering::Relaxed);

        // Only print the section header once, right before the first conflict found
        if !printed_conflict_header {
            println!();
            eprintln!("{}", "✗ Output conflicts detected:".red().bold());
            printed_conflict_header = true;
        }

        let relative_output = relative_label(output_path, target_dir.as_path());

        eprintln!("  {} {}", "→".red(), relative_output.bold());
        for source in sources {
            eprintln!("      {} {}", "•".dimmed(), source);
        }
    }
    drop(claims); // release the lock now that reporting is done with it

    // Green when something actually happened this run, dimmed grey for a zero count
    let count_str = |n: usize| -> String {
        if n > 0 {
            n.to_string().green().to_string()
        } else {
            n.to_string().dimmed().to_string()
        }
    };

    // What the optimise cache did this run, printed before the summary so the
    // summary stays the last thing on screen. A zero stays dimmed, so what
    // shows up in colour is what actually happened
    println!();
    println!("{}", "── Optimise cache ──────────────────".bold());
    println!(
        "  {:<22}: {}{}",
        "PNG optimise skipped",
        count_str(cache.png_skipped),
        if cache.png_skipped > 0 {
            format!(
                " (~{} of oxipng avoided)",
                format_duration(Duration::from_millis(cache.skipped_ms))
            )
            .dimmed()
            .to_string()
        } else {
            String::new()
        }
    );
    println!(
        "  {:<22}: {}{}",
        "PNG optimised",
        count_str(cache.png_optimised),
        if cache.png_optimised > 0 {
            format!(
                " ({} in oxipng)",
                format_duration(Duration::from_millis(cache.oxipng_ms))
            )
            .dimmed()
            .to_string()
        } else {
            String::new()
        }
    );
    // How many jobs were queued by a time the cache had recorded. The rest
    // (new, renamed or edited) have none, so they were started first
    let queued = cache.timings_known + cache.timings_new;
    println!(
        "  {:<22}: {}{}",
        "Queued by timing",
        format!("{} of {}", cache.timings_known, queued).green(),
        if cache.timings_new > 0 {
            format!(" ({} new or changed, started first)", cache.timings_new)
                .dimmed()
                .to_string()
        } else {
            String::new()
        }
    );
    println!(
        "  {:<22}: {}",
        "Cache rows added",
        count_str(cache.png_new + cache.timings_new)
    );
    println!(
        "  {:<22}: {}{}",
        "Cache rows improved",
        count_str(cache.png_faster + cache.timings_faster),
        if cache.png_faster + cache.timings_faster > 0 {
            " (a run beat the recorded time)".dimmed().to_string()
        } else {
            String::new()
        }
    );
    let file_text = match cache.file {
        FileState::Unchanged => "no changes, not rewritten".dimmed().to_string(),
        FileState::Rewritten => "saved".green().to_string(),
        FileState::NotSaved => "not saved".yellow().to_string(),
    };
    println!("  {:<22}: {} rows, {}", "Cache file", cache.rows, file_text);

    println!("{}", "── Summary ─────────────────────────".bold());
    println!(
        "  {:<22}: {}",
        "data.json copied",
        count_str(stats.data_json.load(Ordering::Relaxed))
    );
    println!(
        "  {:<22}: {}",
        "SVG/SVGZ rendered",
        count_str(stats.svgs_rendered.load(Ordering::Relaxed))
    );
    println!(
        "  {:<22}: {}",
        "Hidden SVGs (crops)",
        count_str(stats.hidden_svgs.load(Ordering::Relaxed))
    );
    println!(
        "  {:<22}: {}",
        "PNG copied",
        count_str(stats.pngs_copied.load(Ordering::Relaxed))
    );
    println!(
        "  {:<22}: {}",
        "Genlists processed",
        count_str(stats.genlists.load(Ordering::Relaxed))
    );
    println!(
        "  {:<22}: {}",
        "Colours exported",
        count_str(stats.colors_exported.load(Ordering::Relaxed))
    );
    let error_count = errors.len();
    if error_count > 0 {
        println!(
            "  {:<22}: {}",
            "Errors",
            error_count.to_string().red().bold()
        );
    }
    let conflict_count = stats.conflicts.load(Ordering::Relaxed);
    if conflict_count > 0 {
        println!(
            "  {:<22}: {}",
            "Output conflicts",
            conflict_count.to_string().red().bold()
        );
    }

    println!("{}", "────────────────────────────────────".bold());
    println!(
        "  {:<22}: {}",
        "Total time",
        format_duration(start.elapsed()).cyan()
    );

    // See write_registry_to_json's own doc comment for why this file is written
    write_registry_to_json(target_dir)?;

    // Two sources fighting over the same output filename is a genuine
    // naming bug in the wiki's own content - a coincidence, not intentional
    // - and not something safe to let a build silently succeed through.
    // The build fails here, after everything else has already run and been
    // reported on, so the conflict gets caught during compilation rather
    // than shipped in a Wiki where one of the two images just silently
    // doesn't exist.
    if conflict_count > 0 {
        return Err(format!(
            "{} output file(s) were written by more than one source, see above",
            conflict_count
        )
        .into());
    }

    println!();
    println!("{}", "✔ All tasks completed.".green().bold());
    Ok(())
}
