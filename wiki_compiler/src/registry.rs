use serde::Serialize;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::VERSION;

/// The contents of `Registry.json`.
#[derive(Serialize)]
struct Registry {
    pub version: &'static str,
}

impl Registry {
    fn default() -> Self {
        Self { version: VERSION }
    }
}

/// Writes `Registry.json` into the built Wiki, recording which compiler
/// version produced it.
///
/// The RhythmiRust app compares this version with its own bundled compiler
/// version. If they differ, the Wiki may use a layout or file format that has
/// changed, so the app deletes the Wiki directory and forces a rebuild.
///
/// For example, a Wiki built by compiler `0.3.0` produces:
///
/// ```json
/// {
///   "version": "0.3.0"
/// }
/// ```
pub fn write_registry_to_json(target_dir: &Arc<PathBuf>) -> Result<(), Box<dyn Error>> {
    let registry = Registry::default();

    // Pretty-printed because a person may open this file to check the version.
    let json_content = serde_json::to_string_pretty(&registry)?;
    let json_file_path = target_dir.join("Registry.json");
    fs::write(json_file_path, json_content)?;

    Ok(())
}
