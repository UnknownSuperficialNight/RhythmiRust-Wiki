use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use usvg::{Color, Options, Paint, Tree};

use crate::render::render_svg_to_png;

/// Converts a SVG Color struct to a hexadecimal string representation
/// Format: #RRGGBB (uppercase for consistent filename matching)
///
/// This is used for generating consistent filenames from SVG color values
fn hex_color(color: &Color) -> String {
    format!("#{:02X}{:02X}{:02X}", color.red, color.green, color.blue)
}

/// Loads a generation list from JSON file with case-insensitive key lookup
///
/// Converts all keys to uppercase for consistent lookup across different
/// case variations in filenames. Handles error propagation for file I/O.
fn load_genlist<P: AsRef<Path>>(
    path: P,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(path)?;
    let raw_map: HashMap<String, String> = serde_json::from_str(&data)?;
    // Convert all keys to uppercase for consistent lookup
    let map = raw_map
        .into_iter()
        .map(|(k, v)| (k.to_uppercase(), v))
        .collect();
    Ok(map)
}

/// Recursively collects all path elements from an SVG group
///
/// Handles nested groups and subroots to ensure complete path collection
///
/// Note: Cloning is required to take ownership of the path for storage
fn collect_paths(group: &usvg::Group, paths: &mut Vec<usvg::Path>) {
    for node in group.children() {
        if let usvg::Node::Path(ref path) = *node {
            // Clone path to take ownership and avoid borrowing issues
            paths.push(*path.clone());
        }
        if let usvg::Node::Group(ref g) = *node {
            collect_paths(g, paths);
        }
        node.subroots(|subroot| collect_paths(subroot, paths));
    }
}

/// Main processing function for SVG with generation list
///
/// 1. Loads color-to-filename mapping from JSON
/// 2. Parses SVG file
/// 3. Collects all path elements
/// 4. Processes each path to export colored versions
///
/// Ensures each color is only exported once per SVG
///
/// Relies on render module's `render_svg_to_png` function for PNG generation
pub fn process_svg_with_genlist(
    svg_path: &Path,
    genlist_path: &Path,
    target_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load color-to-filename mapping
    let color_map = load_genlist(genlist_path)?;

    // Read and parse SVG
    let svg_data = fs::read(svg_path)?;
    let options = Options::default();
    let tree = Tree::from_data(&svg_data, &options)?;

    // Track which colours we've already exported
    let mut exported = HashMap::new();

    let mut paths = Vec::new();
    collect_paths(tree.root(), &mut paths);

    // Iterate over SVG nodes
    for path in &paths {
        if let Some(stroke) = path.stroke()
            && let Paint::Color(color) = stroke.paint()
        {
            // Convert to uppercase for consistent filename matching
            let hex = hex_color(color).to_uppercase();
            if let Some(filename) = color_map.get(&hex) {
                if exported.contains_key(&hex) {
                    continue;
                }
                exported.insert(hex.clone(), true);

                let output_path = target_dir.join(filename).with_extension("png");
                render_svg_to_png(svg_path, &output_path, Some(path.abs_bounding_box()))?;
                println!(
                    "Exported {} as {}",
                    hex.truecolor(color.red, color.green, color.blue),
                    filename
                );
            }
        }
    }
    Ok(())
}
