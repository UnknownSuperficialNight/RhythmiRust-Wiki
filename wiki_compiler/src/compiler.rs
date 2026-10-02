use owo_colors::OwoColorize;
use rayon::ThreadPoolBuilder;
use serde_json::{Value, to_string};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use walkdir::WalkDir;

use crate::crop_generation::{
    claim_output, find_main_svg, process_svg_with_genlist, resolve_named_genlist,
};
use crate::output::{FileKind, format_duration, log_genlist, log_processed, relative_label};
use crate::reference_check::check_and_warn_if_unreferenced;
use crate::registry::write_registry_to_json;
use crate::render::{optimise_png, render_svg_to_png};
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
    let relevant_files: Vec<PathBuf> = WalkDir::new(source_dir.as_path())
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

            // Only files (not directories) with an extension this compiler
            // actually does something with
            if path.is_file()
                && let Some(file_name) = path.file_name().and_then(|f| f.to_str())
                && (file_name.ends_with(".json")
                    || file_name.ends_with(".png")
                    || file_name.ends_with(".svg")
                    || file_name.ends_with(".svgz"))
            {
                return Some(path.to_path_buf());
            }

            None
        })
        .collect();

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

    let start = Instant::now();
    // scope() blocks until every task spawned inside this closure has
    // finished, so none of the summary/reporting code after this call can
    // ever run ahead of the files it's about to report on
    pool.scope(|scope| {
        for file_path in &relevant_files {
            // Cloned so each spawned closure owns its own handle, rather
            // than trying to share a borrow of these across threads - Arc's
            // clone is cheap (just a refcount bump), not a deep copy
            let source_dir = source_dir.clone();
            let target_dir = target_dir.clone();
            let warnings = Arc::clone(&warnings);
            let stats = Arc::clone(&stats);
            let claims = Arc::clone(&claims);

            scope.spawn(move |_| {
                // Where this file sits under source_dir, e.g. "Settings/Main.svgz"
                let relative_path = match file_path.strip_prefix(source_dir.as_path()) {
                    Ok(path) => path,
                    Err(e) => {
                        eprintln!("Error computing relative path: {}", e);
                        return;
                    }
                };
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

                            // Read the page's data.json as text, then parse it as JSON
                            let json_content =
                                fs::read_to_string(file_path).expect("Failed to read JSON");
                            let json_value: Value =
                                serde_json::from_str(&json_content).expect("Failed to parse JSON");

                            // Re-serialised rather than just copied byte-for-byte, so the
                            // shipped data.json is minified - a smaller download for
                            // whoever's viewing the Wiki, and any whitespace/formatting
                            // quirks left over from hand-editing don't leak into the output
                            let minified_json =
                                to_string(&json_value).expect("Failed to minify JSON");

                            // data.json is always relevant, so its directory can be created
                            // unconditionally - unlike a genlist, there's no "might turn out
                            // not to apply" case to gate this behind
                            if let Err(e) = fs::create_dir_all(target_parent) {
                                eprintln!("Error creating directory: {}", e);
                                return;
                            }

                            // Write the minified JSON out to the mirrored output path
                            if let Err(e) = fs::write(&target_path, minified_json) {
                                eprintln!("Error writing minified JSON: {}", e);
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                            } else {
                                stats.data_json.fetch_add(1, Ordering::Relaxed);
                            }

                            log_processed(FileKind::Data, &location, start.elapsed());
                        }
                        "GenList.json" => {
                            let start = Instant::now();

                            // Figure out which image this folder's default genlist applies to
                            match find_main_svg(file_path) {
                                Ok(svg_path) => {
                                    // Only make the directory once it's confirmed there's
                                    // actually a Main.svg/Main.svgz to render - creating it
                                    // unconditionally would leave an empty output folder
                                    // behind for any GenList.json with no matching Main image
                                    if let Err(e) = fs::create_dir_all(target_parent) {
                                        eprintln!("Error creating directory: {}", e);
                                        stats.errors.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        // Relative, not absolute, so it reads cleanly in log
                                        // lines and conflict messages further down
                                        let svg_label =
                                            relative_label(&svg_path, source_dir.as_path());

                                        match process_svg_with_genlist(
                                            &svg_path,
                                            file_path,
                                            target_parent,
                                            &claims,
                                            &svg_label,
                                        ) {
                                            Ok((count, crop_lines)) => {
                                                stats.genlists.fetch_add(1, Ordering::Relaxed);
                                                stats
                                                    .colors_exported
                                                    .fetch_add(count, Ordering::Relaxed);
                                                log_genlist(
                                                    FileKind::Genlist,
                                                    &genlist_folder(relative_path),
                                                    count,
                                                    start.elapsed(),
                                                    &crop_lines,
                                                );
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "Error processing SVG/SVGZ with genlist: {}",
                                                    e
                                                );
                                                stats.errors.fetch_add(1, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Error finding Main.svg or Main.svgz: {}", e);
                                    stats.errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        file if file.ends_with(".json") => {
                            // A genlist paired by filename with a specific SVG/SVGZ (e.g.
                            // "Cache.json" pairs with "Cache.svgz"), rather than the
                            // folder's Main.svg/Main.svgz. resolve_named_genlist only ever
                            // stats the filesystem for a matching sibling before this point
                            // - it hasn't read or parsed this file's contents yet, so
                            // anything without a matching sibling costs almost nothing.
                            let start = Instant::now();

                            match resolve_named_genlist(file_path) {
                                // A matching svg/svgz sibling exists - this really is a genlist
                                Ok(Some(target_svg_path)) => {
                                    if let Err(e) = fs::create_dir_all(target_parent) {
                                        eprintln!("Error creating directory: {}", e);
                                        stats.errors.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        let svg_label =
                                            relative_label(&target_svg_path, source_dir.as_path());
                                        match process_svg_with_genlist(
                                            &target_svg_path,
                                            file_path,
                                            target_parent,
                                            &claims,
                                            &svg_label,
                                        ) {
                                            Ok((count, crop_lines)) => {
                                                stats.genlists.fetch_add(1, Ordering::Relaxed);
                                                stats
                                                    .colors_exported
                                                    .fetch_add(count, Ordering::Relaxed);
                                                // Labelled by the image this genlist is about,
                                                // not by its own .json filename - the
                                                // [GENLIST CUSTOM] tag already implies "and its
                                                // .json partner", so naming the image is the
                                                // more useful of the two names to show
                                                log_genlist(
                                                    FileKind::GenlistCustom,
                                                    &svg_label,
                                                    count,
                                                    start.elapsed(),
                                                    &crop_lines,
                                                );
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "Error processing genlist '{}': {}",
                                                    file_path.display(),
                                                    e
                                                );
                                                stats.errors.fetch_add(1, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                }
                                // No matching sibling - just some other json file that
                                // happens to live here, leave it alone entirely
                                Ok(None) => {}
                                Err(e) => {
                                    eprintln!(
                                        "Error resolving genlist '{}': {}",
                                        file_path.display(),
                                        e
                                    );
                                    stats.errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        file if file.ends_with(".png") => {
                            if let Err(e) = check_and_warn_if_unreferenced(file_path, &warnings) {
                                eprintln!("Error checking file {}: {}", file_path.display(), e);
                            }

                            let start = Instant::now();

                            if let Err(e) = fs::create_dir_all(target_parent) {
                                eprintln!("Error creating directory: {}", e);
                                return;
                            }

                            claim_output(
                                &claims,
                                &target_path,
                                format!("direct copy of '{}'", relative_path.display()),
                            );

                            // Copy the PNG through as-is first, then shrink the copy in place
                            match fs::copy(file_path, &target_path) {
                                Ok(_) => {
                                    if let Err(e) = optimise_png(&target_path) {
                                        eprintln!("Error optimising PNG file: {}", e);
                                        stats.errors.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        stats.pngs_copied.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Error copying PNG file: {}", e);
                                    stats.errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }

                            log_processed(FileKind::Png, &location, start.elapsed());
                        }
                        file if file.ends_with(".svg") || file.ends_with(".svgz") => {
                            // A leading underscore marks the source file as hidden - it
                            // exists purely to be sliced up by a paired genlist (the *.json
                            // arm above), and was never meant to appear as its own
                            // standalone image on any page. Skip both rendering it as a full
                            // image and the unreferenced-image check, since neither applies
                            // to a file that isn't supposed to stand on its own.
                            if file.starts_with('_') {
                                stats.hidden_svgs.fetch_add(1, Ordering::Relaxed);
                                return;
                            }

                            if let Err(e) = check_and_warn_if_unreferenced(file_path, &warnings) {
                                eprintln!("Error checking file {}: {}", file_path.display(), e);
                            }

                            let start = Instant::now();

                            if let Err(e) = fs::create_dir_all(target_parent) {
                                eprintln!("Error creating directory: {}", e);
                                return;
                            }

                            // The SVG renders to a PNG with the same stem, e.g. Main.svgz -> Main.png
                            claim_output(
                                &claims,
                                &target_path.with_extension("png"),
                                format!("rendered from '{}'", relative_path.display()),
                            );

                            if let Err(e) = render_svg_to_png(
                                file_path,
                                &target_path.with_extension("png"),
                                None,
                            ) {
                                eprintln!("Error rendering SVG/SVGZ to PNG: {}", e);
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                            } else {
                                stats.svgs_rendered.fetch_add(1, Ordering::Relaxed);
                            }

                            log_processed(FileKind::Svg, &location, start.elapsed());
                        }
                        // Some other extension matched the earlier .json/.png/.svg/.svgz
                        // filter but doesn't have a specific handler above - nothing to do
                        _ => {}
                    }
                }
            })
        }
    });

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

    println!();
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
    let error_count = stats.errors.load(Ordering::Relaxed);
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
