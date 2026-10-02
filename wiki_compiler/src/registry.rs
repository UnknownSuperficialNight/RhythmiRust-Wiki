use serde::Serialize;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::VERSION;

#[derive(Serialize)]
struct Registry {
    pub version: &'static str,
}

impl Registry {
    fn default() -> Self {
        Self { version: VERSION }
    }
}

/// Writes Registry.json into the freshly built Wiki, recording which
/// compiler version produced it.
///
/// The main RhythmiRust app reads this file back and compares the version
/// inside it against its own bundled compiler version. If they don't
/// match - for example the app was updated but the installed Wiki was
/// built by an older compiler - the app deletes the Wiki directory and
/// forces a rebuild, rather than risk trusting a layout or file format
/// that might have changed between versions.
///
/// For example, a Wiki built by compiler `0.3.0` produces:
///
/// ```json
/// {
///   "version": "0.3.0"
/// }
/// ```
///
/// If the app is later updated to expect `0.4.0`, that mismatch is what
/// triggers the rebuild.
pub fn write_registry_to_json(target_dir: &Arc<PathBuf>) -> Result<(), Box<dyn Error>> {
    let registry = Registry::default();
    // Pretty-printed, not minified, since a person might open this file
    // directly to check which version built their Wiki
    let json_content = serde_json::to_string_pretty(&registry)?;
    let json_file_path = target_dir.join("Registry.json");
    fs::write(json_file_path, json_content)?;

    Ok(())
}
