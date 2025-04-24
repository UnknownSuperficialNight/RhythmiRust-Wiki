use serde::Serialize;
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

/// Writes the registry data to a JSON file in the specified directory.
pub fn write_registry_to_json(target_dir: &Arc<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    // Create a new instance of the registry
    let registry = Registry::default();

    // Serialize the registry to a pretty-printed JSON string
    let json_content = serde_json::to_string_pretty(&registry)?;

    // Construct the path for the JSON file in the target directory
    let json_file_path = target_dir.join("Registry.json");

    // Write the JSON content to the file
    fs::write(json_file_path, json_content)?;

    Ok(())
}
