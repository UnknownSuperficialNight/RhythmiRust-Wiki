use rayon::ThreadPoolBuilder;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use walkdir::WalkDir;

mod render;
use crate::render::*;

/// Recreate the directory structure based on relevant files
fn recreate_directory_structure(
    source_dir: &Arc<PathBuf>,
    target_dir: &Arc<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Remove the target directory if it exists
    if target_dir.exists() {
        fs::remove_dir_all(target_dir.as_path())?;
    }

    // Collect all relevant files and their parent directories
    let mut relevant_files = Vec::new();
    for entry in WalkDir::new(source_dir.as_path()) {
        let entry = entry?;
        let path = entry.path();

        // Skip the `.git` directory
        if path.file_name().map_or(false, |name| name == ".git") {
            continue;
        }

        // Check if the file is relevant
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
                if file_name == "data.json"
                    || file_name.ends_with(".png")
                    || file_name.ends_with(".svg")
                {
                    relevant_files.push(path.to_path_buf());
                }
            }
        }
    }

    let pool = ThreadPoolBuilder::new()
        .num_threads(num_cpus::get())
        .build()
        .unwrap();

    let start = Instant::now();
    pool.scope(|scope| {
        for file_path in relevant_files {
            let source_dir = source_dir.clone();
            let target_dir = target_dir.clone();
            scope.spawn(move |_| {
                // Compute the relative path
                let relative_path = match file_path.strip_prefix(source_dir.as_path()) {
                    Ok(path) => path,
                    Err(e) => {
                        eprintln!("Error computing relative path: {}", e);
                        return;
                    }
                };
                let target_path = target_dir.join(relative_path);

                // Create the parent directory if it doesn't exist
                if let Some(parent) = target_path.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        eprintln!("Error creating directory: {}", e);
                    }
                }

                // Copy or convert the file
                if let Some(file_name) = file_path.file_name().and_then(|f| f.to_str()) {
                    match file_name {
                        "data.json" => {
                            // Copy data.json files
                            if let Err(e) = fs::copy(&file_path, &target_path) {
                                eprintln!("Error copying data.json: {}", e);
                            }
                        }
                        file if file.ends_with(".png") => {
                            // Copy PNG files
                            let start = Instant::now();

                            match fs::copy(&file_path, &target_path) {
                                Ok(_) => {
                                    if let Err(e) = optimise_png(&target_path) {
                                        eprintln!("Error optimising PNG file: {}", e);
                                    }
                                }
                                Err(e) => eprintln!("Error copying PNG file: {}", e),
                            }

                            // Get the end time
                            println!(
                                "Time taken: {:?} for file: {}",
                                start.elapsed(),
                                file_path.file_name().unwrap().to_str().unwrap()
                            );
                        }
                        file if file.ends_with(".svg") => {
                            // Render and optimise svg files
                            let start = Instant::now();

                            if let Err(e) =
                                render_svg_to_png(&file_path, &target_path.with_extension("png"))
                            {
                                eprintln!("Error rendering SVG to PNG: {}", e);
                            }

                            // Get the end time
                            println!(
                                "Time taken: {:?} for file: {}",
                                start.elapsed(),
                                file_path.file_name().unwrap().to_str().unwrap()
                            );
                        }
                        _ => {}
                    }
                }
            })
        }
    });

    // Get the end time
    println!("Time taken total: {:?}", start.elapsed());

    println!("All tasks completed.");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the current directory where the binary is executed
    let current_dir = env::current_dir()?;

    // Ger the directory name for safety checks
    let parent_dir = current_dir.file_name().unwrap();

    // Define the expected file name
    let helper_file_name = "_Wiki_build_helper.json";
    let helper_file_path = current_dir.join(helper_file_name);

    // Check if the helper file exists in the current directory
    if !helper_file_path.exists() {
        eprintln!(
            "Error: '{}' not found. The compiler needs to be at the top level of the Wiki directory, \
             on the same level as '{}'.",
            helper_file_name, helper_file_name
        );
        return Err("Required file not found".into());
    }

    // Check if the file exists in the current directory
    if parent_dir.to_str().unwrap() == "RhythmiRust-Wiki" {
        // If the file exists, proceed to render the wiki
        let target_dir = current_dir.join("Wiki");
        recreate_directory_structure(&Arc::new(current_dir), &Arc::new(target_dir))?;
    } else {
        // If the file is not found, print an error message
        eprintln!(
            "Error: The compiler must be in the top-level 'Wiki' directory. \
                Current parent directory: '{}'.",
            parent_dir.to_str().unwrap()
        );
        return Err("Required file not found".into());
    }

    Ok(())
}
