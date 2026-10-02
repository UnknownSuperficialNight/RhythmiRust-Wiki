use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Catches the easy mistake of dropping in (or renaming) an image and
/// forgetting to reference it from the page's data.json.
///
/// Nothing in the compile step itself would ever fail because of this -
/// the file still renders and copies into the output Wiki just fine. The
/// problem only shows up later, invisibly: the image exists in the
/// output, it just never gets displayed anywhere, because nothing in the
/// page's layout points at it. This check exists purely to surface that
/// mistake at build time instead of a wiki author only noticing months
/// later that an image never showed up.
///
/// An SVG/SVGZ is checked against the PNG filename it renders to, not its
/// own filename, since that PNG name is what actually has to appear in
/// data.json - the source SVG itself is never referenced directly. For
/// example, `Main.svg` in a folder is checked for a `Main.png` reference
/// in that folder's data.json, even though no file named `Main.svg`
/// exists in the output at all.
pub fn check_and_warn_if_unreferenced(
    file_path: &Path,
    warnings: &Arc<Mutex<Vec<PathBuf>>>,
) -> Result<(), std::io::Error> {
    if let Some(dir) = file_path.parent() {
        let data_json_path = dir.join("data.json");

        // Nothing to check against if this folder has no data.json at all
        if data_json_path.exists() {
            let data = fs::read_to_string(&data_json_path)?;

            // Parsed as generic JSON rather than into a fixed struct - all
            // that's needed is to search its text for a filename, not
            // understand or validate its actual page-layout structure
            if let Ok(json) = serde_json::from_str::<Value>(&data) {
                let extension = file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                let expected_name = match extension.as_str() {
                    // A PNG's own filename is exactly what data.json would reference
                    "png" => file_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "Invalid UTF-8 filename",
                            )
                        })?
                        .to_string(),

                    // An SVG/SVGZ renders to a same-stem PNG, so that PNG
                    // name is what to search for, not the source file's own
                    // name - see the doc comment above for why
                    "svg" | "svgz" => {
                        let file_stem = file_path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "Invalid UTF-8 filename",
                                )
                            })?;

                        format!("{file_stem}.png")
                    }

                    // Any other extension isn't something this check applies to
                    _ => return Ok(()),
                };

                // Stringify the whole parsed JSON and substring-search it -
                // simpler than walking data.json's own structure looking
                // for image references, and just as reliable for this purpose
                let json_str = json.to_string();

                if !json_str.contains(&expected_name) {
                    let mut w = warnings.lock().unwrap();
                    w.push(file_path.to_path_buf());
                }
            }
        }
    }

    Ok(())
}
