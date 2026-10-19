use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Warns when an image is not referenced in its folder's `data.json`.
///
/// Compilation does not fail in this case. The image is still copied into the
/// output Wiki, but nothing in the page layout points at it, so it is never
/// displayed. This check reports that at build time.
///
/// An SVG or SVGZ is checked against the PNG name it renders to, because that
/// is the name `data.json` has to contain. The source SVG is never referenced
/// directly.
///
/// For example, `Main.svg` is checked by searching `data.json` for `Main.png`.
pub fn check_and_warn_if_unreferenced(
    file_path: &Path,
    warnings: &Arc<Mutex<Vec<PathBuf>>>,
) -> Result<(), std::io::Error> {
    if let Some(dir) = file_path.parent() {
        let data_json_path = dir.join("data.json");

        // A folder without a `data.json` has nothing to check against.
        if data_json_path.exists() {
            let data = fs::read_to_string(&data_json_path)?;

            // The JSON is parsed generically because the check only searches
            // it for a filename. It does not need the page layout structure.
            if let Ok(json) = serde_json::from_str::<Value>(&data) {
                let extension = file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                let expected_name = match extension.as_str() {
                    // A PNG's own filename is what `data.json` references.
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

                    // An SVG or SVGZ renders to a PNG with the same stem.
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

                    // Other file types are not checked.
                    _ => return Ok(()),
                };

                // Searching the serialized JSON as text is simpler than
                // walking its structure, and finds the filename wherever it
                // appears.
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
