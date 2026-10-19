use std::env;
use std::error::Error;
use std::sync::Arc;

mod compiler;
mod crop_generation;
mod optimise_cache;
mod output;
mod reference_check;
mod registry;
mod render;
mod scheduler;
mod stats;

use crate::compiler::recreate_directory_structure;

/// The compiler version from `Cargo.toml`.
///
/// It is written to `Registry.json` so the RhythmiRust app can tell which
/// compiler built an installed Wiki.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<(), Box<dyn Error>> {
    let current_dir = env::current_dir()?;
    let parent_dir = current_dir.file_name().unwrap();

    // `recreate_directory_structure` deletes and rebuilds a "Wiki" subfolder
    // inside the current directory.
    //
    // Both checks below must pass first. Without them, running the compiler
    // from the wrong directory would delete an unrelated "Wiki" folder.
    let helper_file_name = "_Wiki_build_helper.json";
    let helper_file_path = current_dir.join(helper_file_name);

    // This marker file should only exist at the top of the wiki repository.
    if !helper_file_path.exists() {
        eprintln!(
            "Error: '{}' not found. The compiler needs to be at the top level of the Wiki directory, \
             on the same level as '{}'.",
            helper_file_name, helper_file_name
        );
        return Err("Required file not found".into());
    }

    // The folder name is checked as well, because the marker file could have
    // been copied somewhere else.
    if parent_dir.to_str().unwrap() == "RhythmiRust-Wiki" {
        let target_dir = current_dir.join("Wiki");
        recreate_directory_structure(&Arc::new(current_dir), &Arc::new(target_dir))?;
    } else {
        eprintln!(
            "Error: The compiler must be in the top-level 'Wiki' directory. \
                Current parent directory: '{}'.",
            parent_dir.to_str().unwrap()
        );
        return Err("Required file not found".into());
    }

    Ok(())
}
