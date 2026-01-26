use owo_colors::OwoColorize;
use rayon::ThreadPoolBuilder;
use serde_json::Value;
use serde_json::to_string;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use walkdir::WalkDir;

mod render;
use crate::render::*;
mod registry;
use crate::registry::*;
mod crop_generation;
use crate::crop_generation::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn check_and_warn_if_unreferenced(
    file_path: &Path,
    warnings: &Arc<Mutex<Vec<PathBuf>>>,
) -> Result<(), std::io::Error> {
    if let Some(dir) = file_path.parent() {
        let data_json_path = dir.join("data.json");

        if data_json_path.exists() {
            let data = fs::read_to_string(&data_json_path)?;
            if let Ok(json) = serde_json::from_str::<Value>(&data) {
                let file_stem = file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .expect("Failed to get valid UTF-8 file stem from file_path");
                let json_str = json.to_string();
                if !json_str.contains(file_stem) {
                    let mut w = warnings.lock().unwrap();
                    w.push(file_path.to_path_buf());
                }
            }
        }
    }
    Ok(())
}

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
    let relevant_files: Vec<PathBuf> = WalkDir::new(source_dir.as_path())
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.ok()?; // Handle errors gracefully
            let path = entry.path();

            // Skip the `.git` directory
            if path.file_name().is_some_and(|name| name == ".git") {
                return None;
            }

            // Check if the file is relevant
            if path.is_file()
                && let Some(file_name) = path.file_name().and_then(|f| f.to_str())
                && (file_name == "data.json"
                    || file_name == "GenList.json"
                    || file_name.ends_with(".png")
                    || file_name.ends_with(".svg"))
            {
                return Some(path.to_path_buf());
            }

            None
        })
        .collect();

    let pool = ThreadPoolBuilder::new()
        .num_threads(num_cpus::get())
        .build()
        .unwrap();

    // Shared warnings vector
    let warnings = Arc::new(Mutex::new(Vec::new()));

    let start = Instant::now();
    pool.scope(|scope| {
        for file_path in &relevant_files {
            let source_dir = source_dir.clone();
            let target_dir = target_dir.clone();
            let warnings = Arc::clone(&warnings);

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
                let target_parent = target_path.parent().unwrap();

                // Create the parent directory if it doesn't exist
                if let Some(parent) = target_path.parent()
                    && let Err(e) = fs::create_dir_all(parent)
                {
                    eprintln!("Error creating directory: {}", e);
                }

                // Copy or convert the file
                if let Some(file_name) = file_path.file_name().and_then(|f| f.to_str()) {
                    match file_name {
                        "data.json" => {
                            let start = Instant::now();

                            // Read the json source content
                            let json_content =
                                fs::read_to_string(file_path).expect("Failed to read JSON");

                            // Parse the JSON content into a serde_json::Value
                            let json_value: Value =
                                serde_json::from_str(&json_content).expect("Failed to parse JSON");

                            // Minify the JSON file
                            let minified_json =
                                to_string(&json_value).expect("Failed to minify JSON");

                            // Write the minified JSON to the target file
                            if let Err(e) = fs::write(&target_path, minified_json) {
                                eprintln!("Error writing minified JSON: {}", e);
                            }

                            // Get the end time
                            println!(
                                "Time taken: {:?} for file: {}",
                                start.elapsed(),
                                file_path.file_name().unwrap().to_str().unwrap()
                            );
                        }
                        "GenList.json" => {
                            // Render and optimise svg files
                            let start = Instant::now();

                            if let Err(e) = process_svg_with_genlist(
                                &file_path.with_file_name("Main.svg"),
                                file_path,
                                target_parent,
                            ) {
                                eprintln!("Error processing SVG with genlist: {}", e);
                            }

                            println!("Time taken: {:?} for file: GenList.json", start.elapsed());
                        }
                        file if file.ends_with(".png") => {
                            if let Err(e) = check_and_warn_if_unreferenced(file_path, &warnings) {
                                eprintln!("Error checking file {}: {}", file_path.display(), e);
                            }

                            // Copy PNG files
                            let start = Instant::now();

                            match fs::copy(file_path, &target_path) {
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
                            if let Err(e) = check_and_warn_if_unreferenced(file_path, &warnings) {
                                eprintln!("Error checking file {}: {}", file_path.display(), e);
                            }

                            // Render and optimise svg files
                            let start = Instant::now();

                            if let Err(e) = render_svg_to_png(
                                file_path,
                                &target_path.with_extension("png"),
                                None,
                            ) {
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

    // Display warnings
    let warnings = warnings.lock().unwrap();
    if !warnings.is_empty() {
        println!(
            "{}",
            "The following image files are not referenced in their sibling data.json:".red()
        );
        for path in warnings.iter() {
            // Compute the relative path
            let relative_path = match path.strip_prefix(source_dir.as_path()) {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("Error computing relative path: {}", e);
                    panic!()
                }
            };

            println!("{}", relative_path.display().yellow());
        }
    }

    // Get the end time
    println!(
        "Time taken total: {}",
        format!("{:?}", start.elapsed()).cyan()
    );

    // This saves a JSON file with the current installed Wiki version in it.
    //
    // This version will be checked by the main program and if its version number
    // is not equal to the stored value in the file, delete the old Wiki directory,
    // forcing the user to update to the Wiki for that version.
    write_registry_to_json(target_dir)?;

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
