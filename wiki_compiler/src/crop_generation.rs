use owo_colors::OwoColorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
};
use usvg::{Color, Paint, Rect};

use crate::optimise_cache::Outcome;
use crate::output::detail_line;
use crate::render::parse_svg;

/// Records that `output_path` is about to be written, with a description of
/// who is writing it.
///
/// This is how two sources writing the same output filename are caught. For
/// example, a genlist crop can have the same name as a plain PNG in the same
/// folder.
///
/// Every place that writes a file calls this first. After the build,
/// `recreate_directory_structure` reports any output path with more than one
/// claim. This function never blocks a write. Checking once at the end avoids
/// coordinating conflicts across many worker threads while they run.
///
/// For example, a plain `Cache.png` and a genlist crop that target the same
/// output path leave that path with two claims:
///
/// ```text
/// "direct copy of 'Settings/Cache.png'"
/// "genlist crop (#B7E4C7) from 'Settings/_Advanced Settings Dropdowns.svgz'"
/// ```
pub fn claim_output(
    claims: &Mutex<HashMap<PathBuf, Vec<String>>>,
    output_path: &Path,
    description: String,
) {
    let mut claims = claims.lock().unwrap();

    // The first claim on a path creates its `Vec`. Later claims append to it.
    claims
        .entry(output_path.to_path_buf())
        .or_default()
        .push(description);
}

/// Formats a colour as an uppercase `"#RRGGBB"` hex string.
///
/// The case matters. `load_genlist` uppercases every key it reads, so a colour
/// from the SVG only matches a genlist entry if both sides use the same case.
/// Without this, a genlist key written as `"#d1c4e9"` would not match
/// `#D1C4E9`, and that colour would silently export nothing.
///
/// For example, `Color { red: 209, green: 196, blue: 233 }` becomes
/// `"#D1C4E9"`.
fn hex_color(color: &Color) -> String {
    // `:02X` pads each channel to 2 uppercase hex digits, e.g. 10 becomes "0A".
    format!("#{:02X}{:02X}{:02X}", color.red, color.green, color.blue)
}

/// Loads a colour-to-filename map from a genlist JSON file.
///
/// Every key is uppercased, for the reason given on `hex_color`. A genlist can
/// write its keys in any case (`"#d1c4e9"`, `"#D1C4E9"`) and still match the
/// uppercase colours parsed from the SVG.
///
/// The error is the raw `serde_json` error on purpose. For malformed JSON it
/// includes the line and column of the problem.
pub fn load_genlist<P: AsRef<Path>>(path: P) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let data = fs::read_to_string(path)?;
    let raw_map: HashMap<String, String> = serde_json::from_str(&data)?;

    let map = raw_map
        .into_iter()
        .map(|(k, v)| (k.to_uppercase(), v))
        .collect();
    Ok(map)
}

/// Recursively collects every path in an SVG group.
///
/// Looping over `group.children()` alone would miss two places a genlist colour
/// can be:
///
/// - A path inside a nested group. Groups can nest to any depth, so this
///   recurses into every `usvg::Node::Group`.
/// - Content usvg stores as a subroot instead of a child node: clip-paths,
///   masks and patterns.
///
/// Without both, a stroke colour used only inside a clip-path or a deeply
/// nested group would never be found, and its genlist entry would silently
/// export nothing.
fn collect_paths(group: &usvg::Group, paths: &mut Vec<usvg::Path>) {
    for node in group.children() {
        if let usvg::Node::Path(ref path) = *node {
            // The node only lends a borrow, so the path is cloned to be kept.
            paths.push(*path.clone());
        }

        if let usvg::Node::Group(ref g) = *node {
            collect_paths(g, paths);
        }

        // Clip-paths, masks and patterns carried by this node.
        node.subroots(|subroot| collect_paths(subroot, paths));
    }
}

/// One PNG to produce from an SVG: either the full image or one genlist colour
/// crop.
pub struct RenderJob {
    /// `None` renders the whole image. `Some` renders only that region.
    pub crop: Option<Rect>,
    pub output_path: PathBuf,
    /// The styled label for its dropdown line, at least 7 characters wide.
    /// See `detail_line`.
    pub label: String,
    /// The output's name, shown after the arrow on its dropdown line.
    ///
    /// It also tells the PNGs of one SVG apart in the optimise cache.
    pub name: String,
}

impl RenderJob {
    /// How many pixels this job draws.
    ///
    /// It is used to estimate how long a PNG that has never been timed will
    /// take.
    pub fn area(&self, tree: &usvg::Tree) -> u64 {
        match self.crop {
            Some(rect) => (rect.width() * rect.height()) as u64,
            None => {
                let size = tree.size();
                (size.width() * size.height()) as u64
            }
        }
    }
}

/// An SVG that has been parsed and had its PNGs worked out, ready to render.
pub struct SvgPlan {
    /// Parsed once and shared by every render.
    pub tree: usvg::Tree,
    /// The full image first (unless the SVG is hidden), then each crop in the
    /// order the SVG has them.
    pub jobs: Vec<RenderJob>,
}

/// What an SVG's PNGs came to, for the caller to count and log.
pub struct ExportReport {
    /// Whether the full image was rendered.
    ///
    /// This is false for a hidden SVG, and when the render failed.
    pub full_image: bool,
    /// How many genlist colour crops were rendered.
    pub crops: usize,
    /// One formatted dropdown line per rendered PNG, ready for `log_genlist`.
    pub detail_lines: String,
    /// How many PNGs reused an earlier optimisation result instead of running
    /// oxipng. See `OptimiseCache`.
    pub reused: usize,
    /// The time of every rendered PNG added together.
    ///
    /// This is more than the time on the clock, because the PNGs are rendered
    /// at the same time.
    pub total_time: Duration,
    /// One message per PNG that failed to render.
    ///
    /// They are returned here instead of as an error, so one failed PNG does
    /// not throw away the ones that succeeded.
    pub failures: Vec<String>,
}

/// Parses an SVG and works out every PNG it produces: the full image, plus one
/// crop per colour if it has a genlist.
///
/// Nothing is rendered here. Each PNG is its own job, so the slowest ones can
/// be started first. See `Scheduler`.
///
/// The image and its crops are planned together because they come from one
/// parse. Planned separately, an SVG with a 10-colour genlist would be parsed
/// 11 times.
///
/// A leading underscore on the file name marks the SVG as hidden. It exists
/// only to be cropped by its genlist, so the full image is skipped.
///
/// If a hidden SVG, or one without a `color_map`, has nothing to render, the
/// output folder is not created either. An empty folder would be clutter.
///
/// `source_label` is a short name for `svg_path`, usually its path relative to
/// the wiki root. It appears in claim descriptions and terminal output.
///
/// This returns an error only if the SVG cannot be read or parsed, or the
/// output folder cannot be created.
pub fn plan_svg(
    svg_path: &Path,
    color_map: Option<&HashMap<String, String>>,
    target_dir: &Path,
    claims: &Mutex<HashMap<PathBuf, Vec<String>>>,
    source_label: &str,
) -> Result<SvgPlan, Box<dyn Error>> {
    let tree = parse_svg(svg_path)?;

    let file_name = svg_path.file_name().ok_or("SVG path has no file name")?;
    let hidden = file_name.to_str().is_some_and(|name| name.starts_with('_'));

    let mut jobs: Vec<RenderJob> = Vec::new();

    if !hidden {
        // The SVG renders to a PNG with the same name, e.g. `Main.svgz` becomes
        // `Main.png`.
        let output_path = target_dir.join(file_name).with_extension("png");
        claim_output(
            claims,
            &output_path,
            format!("rendered from '{source_label}'"),
        );
        let name = output_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();

        // The label is the source's own name, so the line reads like a crop's:
        // what it came from, then what it became.
        //
        // For example, `Main → Main` instead of `#D1C4E9 → Save Theme`.
        //
        // It is padded to 7 characters to line up with a hex swatch. A longer
        // name pushes its own arrow out.
        let source_name = svg_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        jobs.push(RenderJob {
            crop: None,
            output_path,
            label: format!("{source_name:<7}").dimmed().to_string(),
            name,
        });
    }

    if let Some(color_map) = color_map {
        let mut paths = Vec::new();
        collect_paths(tree.root(), &mut paths);

        // The hex colours that already have a crop planned.
        //
        // A colour used by several paths produces one PNG, not one per path.
        let mut planned = HashSet::new();

        for path in &paths {
            // `stroke()` is `None` for a fill-only path. `Paint::Color`
            // excludes gradients and patterns, which a genlist hex key can
            // never match.
            if let Some(stroke) = path.stroke()
                && let Paint::Color(color) = stroke.paint()
            {
                let hex = hex_color(color).to_uppercase();

                // Only colours the genlist asked for get a crop.
                if let Some(filename) = color_map.get(&hex) {
                    // `insert()` returns false if an earlier path already
                    // planned this colour.
                    if !planned.insert(hex.clone()) {
                        continue;
                    }

                    let output_path = target_dir.join(filename).with_extension("png");
                    let swatch = hex.truecolor(color.red, color.green, color.blue);
                    claim_output(
                        claims,
                        &output_path,
                        format!("genlist crop ({swatch}) from '{source_label}'"),
                    );

                    jobs.push(RenderJob {
                        // The bounding box of this one path is the region to
                        // crop.
                        crop: Some(path.abs_bounding_box()),
                        output_path,
                        label: swatch.to_string(),
                        name: filename.clone(),
                    });
                }
            }
        }
    }

    if !jobs.is_empty() {
        fs::create_dir_all(target_dir)
            .map_err(|e| format!("couldn't create output folder: {e}"))?;
    }

    Ok(SvgPlan { tree, jobs })
}

/// Combines what every job of an SVG reported into one `ExportReport`.
///
/// `results` is in the same order as `jobs`. Each entry holds how long that PNG
/// took and whether it worked.
pub fn build_report(
    jobs: &[RenderJob],
    results: Vec<(Duration, Result<Outcome, String>)>,
) -> ExportReport {
    let mut report = ExportReport {
        full_image: false,
        crops: 0,
        detail_lines: String::new(),
        reused: 0,
        total_time: Duration::ZERO,
        failures: Vec::new(),
    };

    for (job, (elapsed, result)) in jobs.iter().zip(results) {
        match result {
            Ok(outcome) => {
                let skipped = outcome == Outcome::Reused;
                if skipped {
                    report.reused += 1;
                }
                report
                    .detail_lines
                    .push_str(&detail_line(elapsed, &job.label, &job.name, skipped));
                report.total_time += elapsed;

                // A job with a crop is a genlist colour. Without one, it is
                // the full image.
                if job.crop.is_some() {
                    report.crops += 1;
                } else {
                    report.full_image = true;
                }
            }
            Err(message) => {
                report
                    .failures
                    .push(format!("couldn't render '{}': {message}", job.name));
            }
        }
    }

    report
}

/// Finds the genlist, if any, that belongs to an SVG or SVGZ.
///
/// A genlist is attached to an image by filename, in one of two ways:
///
/// - `GenList.json` is the folder default and belongs to that folder's
///   `Main.svg` or `Main.svgz`.
/// - A named pair, such as `Cache.json` for `Cache.svg` or `Cache.svgz`,
///   matches on the file stem.
///
/// The match is decided from the SVG's side because the SVG is the file that
/// gets rendered, so the full image and its crops are produced together. A JSON
/// file with no matching SVG is never opened.
///
/// `data.json` and `GenList.json` already have jobs of their own, so an image
/// with one of those stems is never paired with them as a named genlist.
///
/// For example, take a folder holding `Main.svgz`, `GenList.json`, `Cache.svgz`
/// and `Cache.json`:
///
/// ```text
/// Main.svgz  -> GenList.json
/// Cache.svgz -> Cache.json
/// ```
///
/// A `notes.json` with no `notes.svg` or `notes.svgz` belongs to nothing and is
/// never read.
pub fn find_genlist_for_svg(svg_path: &Path) -> Option<PathBuf> {
    let parent = svg_path.parent()?;
    let stem = svg_path.file_stem()?.to_str()?;

    // For `Main`, the folder default wins.
    if stem == "Main" {
        let default = parent.join("GenList.json");
        if default.is_file() {
            return Some(default);
        }
    }

    // A named pair, unless the stem would pair the image with the page's own
    // `data.json` or the folder default.
    if stem != "data" && stem != "GenList" {
        let named = parent.join(format!("{stem}.json"));
        if named.is_file() {
            return Some(named);
        }
    }

    None
}

/// Locates the SVG or SVGZ that a folder's `GenList.json` applies to.
///
/// `GenList.json` has no image name in its own filename. It always means that
/// folder's `Main.svg` or `Main.svgz`.
///
/// Genlists are applied from the SVG's side, so a `GenList.json` with no Main
/// image next to it would never be used and nothing would say so. The caller
/// uses this function to report that as an error.
pub fn find_main_svg(genlist_path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let parent = genlist_path
        .parent()
        .ok_or("GenList.json has no parent directory")?;

    let svg_path = parent.join("Main.svg");
    if svg_path.is_file() {
        return Ok(svg_path);
    }

    let svgz_path = parent.join("Main.svgz");
    if svgz_path.is_file() {
        return Ok(svgz_path);
    }

    // The message has no paths because the error report already names the
    // `GenList.json` it is about.
    Err("needs a Main.svg or Main.svgz in the same folder to apply to".into())
}
