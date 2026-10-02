use owo_colors::OwoColorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use std::{collections::HashMap, error::Error};
use usvg::{Color, Options, Paint, Tree};

use crate::output::format_duration;
use crate::render::{load_svg_data, render_svg_to_png};

/// Records that `output_path` is about to be written, tagged with a
/// human-readable description of who's writing it.
///
/// This is the entire mechanism behind catching two different sources
/// fighting over the same output filename - for instance a genlist crop
/// happening to be named the same as a plain copied PNG sitting right next
/// to it. Every place in the compiler that's about to write a file calls
/// this first, and afterwards `recreate_directory_structure` checks
/// whether any single output path ended up with more than one claim
/// attached to it. This function itself never stops a write from
/// happening; it just keeps a paper trail so that check can happen once,
/// after every file has finished, rather than trying to coordinate
/// conflict detection live across dozens of worker threads.
///
/// For example, if both a plain `Cache.png` and a genlist crop from
/// `_Advanced Settings Dropdowns.svgz` end up targeting the same output
/// path, that path accumulates two claims:
///
/// - `"direct copy of 'Settings/Cache.png'"`
/// - `"genlist crop (#B7E4C7) from 'Settings/_Advanced Settings Dropdowns.svgz'"`
pub fn claim_output(
    claims: &Mutex<HashMap<PathBuf, Vec<String>>>,
    output_path: &Path,
    description: String,
) {
    let mut claims = claims.lock().unwrap();
    // entry().or_default() means the first claim on a path creates its Vec,
    // every claim after that just appends to the same one
    claims
        .entry(output_path.to_path_buf())
        .or_default()
        .push(description);
}

/// Formats a colour as an uppercase `"#RRGGBB"` hex string.
///
/// Uppercase specifically matters here, not just for a tidy appearance:
/// `load_genlist` uppercases every hex key it reads out of a genlist JSON
/// file, so a colour parsed from the SVG only matches a genlist entry if
/// both sides agree on case. Without this, a wiki author who typed
/// `"#d1c4e9"` in their genlist JSON could silently fail to match a colour
/// usvg reports as `#D1C4E9`, and that colour just wouldn't export -
/// with no error, since as far as the code is concerned it's simply a
/// colour the genlist never mentioned.
///
/// For example, `Color { red: 209, green: 196, blue: 233 }` becomes
/// `"#D1C4E9"`.
fn hex_color(color: &Color) -> String {
    // :02X pads each channel to 2 hex digits and uppercases them, e.g. 10 -> "0A"
    format!("#{:02X}{:02X}{:02X}", color.red, color.green, color.blue)
}

/// Loads a colour-to-filename map from a genlist JSON file.
///
/// Every key is uppercased on the way in, for the same reason described on
/// `hex_color`: a wiki author can write a genlist's hex keys in whatever
/// case is comfortable (`"#d1c4e9"`, `"#D1C4E9"`, even `"#d1C4e9"`), and
/// lookups against colours parsed out of the SVG - which are always
/// uppercase - will still succeed.
fn load_genlist<P: AsRef<Path>>(path: P) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let data = fs::read_to_string(path)?;
    let raw_map: HashMap<String, String> = serde_json::from_str(&data)?;
    // Rebuild the map with every key uppercased
    let map = raw_map
        .into_iter()
        .map(|(k, v)| (k.to_uppercase(), v))
        .collect();
    Ok(map)
}

/// Recursively collects every drawable path in an SVG group.
///
/// A plain `for child in group.children()` loop would miss two kinds of
/// content a genlist colour might be hiding in:
///
/// - a path nested inside a nested group (groups can nest arbitrarily
///   deep), which is why this recurses into every `usvg::Node::Group`
/// - content usvg represents as a "subroot" rather than an ordinary child
///   node - specifically clip-paths, masks, and patterns, which usvg
///   gives their own independent root transform rather than treating as
///   part of the tree that references them
///
/// Without walking both of those, a stroke colour used only inside a
/// clip-path or a deeply nested group would never be found, and a genlist
/// entry for that colour would silently export nothing.
fn collect_paths(group: &usvg::Group, paths: &mut Vec<usvg::Path>) {
    for node in group.children() {
        // A plain path element - keep it
        if let usvg::Node::Path(ref path) = *node {
            // Cloned to take ownership - node only lends us a borrow, and
            // paths needs to outlive this recursive call
            paths.push(*path.clone());
        }
        // A nested group - recurse into its children too
        if let usvg::Node::Group(ref g) = *node {
            collect_paths(g, paths);
        }
        // Subroots this node carries (clip-paths, masks, patterns) - see
        // the doc comment above for why these need walking too
        node.subroots(|subroot| collect_paths(subroot, paths));
    }
}

/// Core export routine shared by both genlist formats - the legacy
/// GenList.json format and named/paired genlists alike both end up here.
///
/// Walks every path in the SVG, and for each one whose stroke colour is a
/// key in `color_map`, renders just that path's bounding box out to its
/// own PNG (named after the map's value for that colour). Each colour is
/// only ever exported once per SVG, even if several different paths share
/// that exact stroke colour.
///
/// `source_label` is a short, human-readable name for `svg_path` - the
/// caller decides how, but it's usually a path relative to the wiki root
/// rather than the full absolute path, since that's what ends up in claim
/// descriptions and terminal output where a long absolute path would just
/// be noise.
///
/// Note: `target_dir` must already exist - this function only ever writes
/// into it, it never creates it.
///
/// Returns the number of colours actually exported, plus a pre-formatted
/// block of text containing one log line per colour exported. That block
/// is built up in a `String` here rather than printed line-by-line as
/// each colour is found, and printed as a single write by the caller -
/// see `log_genlist`'s doc comment for why that matters when many files
/// are being processed in parallel.
pub fn export_colors_from_svg(
    svg_path: &Path,
    color_map: &HashMap<String, String>,
    target_dir: &Path,
    claims: &Mutex<HashMap<PathBuf, Vec<String>>>,
    source_label: &str,
) -> Result<(usize, String), Box<dyn Error>> {
    // Read and parse the SVG into a tree of drawable nodes
    let svg_data = load_svg_data(svg_path)?;
    let options = Options::default();
    let tree = Tree::from_data(&svg_data, &options)?;

    // Which hex colours have already been exported this run, so a colour
    // used by several different paths only produces one PNG, not several
    let mut exported = HashMap::new();

    let mut paths = Vec::new();
    collect_paths(tree.root(), &mut paths);

    // Buffered log lines, one per colour exported - see the doc comment
    // above for why this is buffered instead of printed immediately
    let mut crop_lines = String::new();

    for path in &paths {
        // stroke() is None for a fill-only path; Paint::Color excludes
        // gradients and patterns, which a genlist hex-colour key could
        // never match in the first place
        if let Some(stroke) = path.stroke()
            && let Paint::Color(color) = stroke.paint()
        {
            let hex = hex_color(color).to_uppercase();
            // Only export colours the genlist actually asked for
            if let Some(filename) = color_map.get(&hex) {
                // Already exported this exact colour from an earlier path - skip it
                if exported.contains_key(&hex) {
                    continue;
                }
                exported.insert(hex.clone(), true);

                let output_path = target_dir.join(filename).with_extension("png");
                claim_output(
                    claims,
                    &output_path,
                    format!(
                        "genlist crop ({}) from '{}'",
                        hex.truecolor(color.red, color.green, color.blue),
                        source_label
                    ),
                );

                let crop_start = Instant::now();
                // abs_bounding_box() gives just this one path's on-canvas
                // extent, which is exactly the crop region render_svg_to_png needs
                render_svg_to_png(svg_path, &output_path, Some(path.abs_bounding_box()))?;
                let crop_elapsed = format_duration(crop_start.elapsed());

                // Duration in a dimmed bracket, in the style of the main
                // status lines - padded to a fixed width (measured on the
                // plain text, coloured after, same reasoning as
                // FileKind::tag) so the colour swatches after it line up
                let crop_time = format!("[{crop_elapsed}]");
                let crop_pad = " ".repeat(8usize.saturating_sub(crop_time.len()));
                crop_lines.push_str(&format!(
                    "      {} {}{} {} {} {}\n",
                    "↳".dimmed(),
                    crop_time.dimmed(),
                    crop_pad,
                    hex.truecolor(color.red, color.green, color.blue),
                    "→".dimmed(),
                    filename
                ));
            }
        }
    }
    Ok((exported.len(), crop_lines))
}

/// Processes the legacy GenList.json format: loads its colour-to-filename
/// map from JSON, then hands off to `export_colors_from_svg` to do the
/// actual per-colour rendering.
///
/// Kept as a thin wrapper rather than folding the JSON-loading into
/// `export_colors_from_svg` directly, since a named/paired genlist (see
/// `resolve_named_genlist`) reaches `export_colors_from_svg` the same way
/// - both formats share the exact same JSON shape, just discovered
///   differently, so this is the one place that shared loading step lives.
///
/// Returns the same `(count, log lines)` pair as `export_colors_from_svg`.
pub fn process_svg_with_genlist(
    svg_path: &Path,
    genlist_path: &Path,
    target_dir: &Path,
    claims: &Mutex<HashMap<PathBuf, Vec<String>>>,
    source_label: &str,
) -> Result<(usize, String), Box<dyn Error>> {
    let color_map = load_genlist(genlist_path)?;
    export_colors_from_svg(svg_path, &color_map, target_dir, claims, source_label)
}

/// Resolves a "named" genlist file - any `*.json` file other than
/// `data.json`/`GenList.json` that's paired by filename with a sibling
/// SVG/SVGZ, e.g. `Cache.json` pairs with `Cache.svg` or `Cache.svgz`.
///
/// This is what lets a single folder have more than one genlist:
/// `GenList.json` still always drives that folder's `Main.svg`/
/// `Main.svgz` (see `find_main_svg`), while e.g. `Cache.json` drives
/// `Cache.svg`/`Cache.svgz` in that same folder. Any other `*.json` file
/// (one that doesn't share a stem with an existing SVG/SVGZ) is left
/// completely alone, so a wiki author can drop unrelated JSON into a
/// folder for whatever their own purposes are without it being
/// misinterpreted as a genlist.
///
/// This deliberately only ever stats the filesystem for a matching
/// sibling - it never reads or parses the JSON file's contents to decide
/// whether it's a genlist. The wiki only gets larger over time, and a
/// file read + JSON parse is real work that most `*.json` files in the
/// tree (the ones with no matching SVG/SVGZ) would be paying for nothing.
/// A single `is_file()` check on two candidate paths is enough to know
/// whether this file is a genlist at all, before spending anything on it.
///
/// For example, given a folder containing `Cache.svgz` and `Cache.json`,
/// calling this with `Cache.json`'s path returns
/// `Ok(Some(".../Cache.svgz"))`. Given `notes.json` with no matching
/// `notes.svg`/`notes.svgz` anywhere nearby, it returns `Ok(None)` and
/// `notes.json` is never opened at all.
pub fn resolve_named_genlist(genlist_path: &Path) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let parent = genlist_path
        .parent()
        .ok_or("Genlist file has no parent directory")?;

    // file_stem strips the extension, e.g. "Cache.json" -> "Cache", so it
    // can be re-joined with the svg/svgz extension instead
    let stem = genlist_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Genlist file has an invalid UTF-8 filename")?;

    let svg_path = parent.join(format!("{stem}.svg"));
    let svgz_path = parent.join(format!("{stem}.svgz"));

    if svg_path.is_file() {
        Ok(Some(svg_path))
    } else if svgz_path.is_file() {
        Ok(Some(svgz_path))
    } else {
        Ok(None)
    }
}

/// Locates the SVG/SVGZ that a folder's default `GenList.json` targets.
///
/// Unlike a named genlist (see `resolve_named_genlist`), which is paired
/// with its target by matching filename, `GenList.json`'s own filename
/// carries no information about which image it's for - it's always the
/// one folder-level default, so it implicitly always means whatever this
/// folder's `Main.svg` or `Main.svgz` happens to be. This function is
/// what turns that implicit convention into an actual path.
pub fn find_main_svg(genlist_path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let parent = genlist_path
        .parent()
        .ok_or("GenList.json has no parent directory")?;

    // Try .svg first, then .svgz - whichever one actually exists wins
    let svg_path = parent.join("Main.svg");
    if svg_path.is_file() {
        return Ok(svg_path);
    }

    let svgz_path = parent.join("Main.svgz");
    if svgz_path.is_file() {
        return Ok(svgz_path);
    }

    Err(format!(
        "Could not find {} or {}",
        svg_path.display(),
        svgz_path.display()
    )
    .into())
}
