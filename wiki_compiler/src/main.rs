use std::env;
use std::error::Error;
use std::sync::Arc;

mod compiler;
mod crop_generation;
mod output;
mod reference_check;
mod registry;
mod render;
mod stats;

use crate::compiler::recreate_directory_structure;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<(), Box<dyn Error>> {
    let current_dir = env::current_dir()?;
    let parent_dir = current_dir.file_name().unwrap();

    // recreate_directory_structure wipes and rebuilds a "Wiki" subfolder in
    // whatever directory this binary is run from. Both checks below exist
    // purely to make sure that destructive step can never run anywhere
    // other than the actual wiki repo - accidentally invoking this from
    // the wrong directory would delete an unrelated "Wiki" folder with no
    // warning otherwise.
    let helper_file_name = "_Wiki_build_helper.json";
    let helper_file_path = current_dir.join(helper_file_name);

    // A marker file that's only ever meant to exist at the top of the real wiki repo
    if !helper_file_path.exists() {
        eprintln!(
            "Error: '{}' not found. The compiler needs to be at the top level of the Wiki directory, \
             on the same level as '{}'.",
            helper_file_name, helper_file_name
        );
        return Err("Required file not found".into());
    }

    // A second, independent guard: the folder name itself. The marker file
    // check above could in principle still pass somewhere it shouldn't
    // (a copy of the file elsewhere, for instance), so this checks
    // something unrelated - the actual folder name - before proceeding.
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
