use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

/// Recreate the directory structure based on relevant files
fn recreate_directory_structure(
    source_dir: &Path,
    target_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Remove the target directory if it exists
    if target_dir.exists() {
        fs::remove_dir_all(target_dir)?;
    }

    // Collect all relevant files and their parent directories
    let mut relevant_files = Vec::new();
    for entry in WalkDir::new(source_dir) {
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
                    || file_name.ends_with(".odg")
                    || file_name.ends_with(".pdf")
                {
                    relevant_files.push(path.to_path_buf());
                }
            }
        }
    }

    // Create directories and copy relevant files
    for file_path in relevant_files {
        // Compute the relative path
        let relative_path = file_path.strip_prefix(source_dir)?;
        let target_path = target_dir.join(relative_path);

        // Create the parent directory if it doesn't exist
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Copy or convert the file
        if let Some(file_name) = file_path.file_name().and_then(|f| f.to_str()) {
            match file_name {
                "data.json" => {
                    // Copy data.json files
                    fs::copy(&file_path, &target_path)?;
                }
                file if file.ends_with(".png") => {
                    // Copy PNG files
                    fs::copy(&file_path, &target_path)?;
                }
                file if file.ends_with(".odg") => {
                    // Convert ODG files to PNG
                    let parent = target_path.parent().unwrap();
                    convert_odg_to_png(&file_path, parent)?;
                }
                file if file.ends_with(".pdf") => {
                    // Copy PDF files
                    fs::copy(&file_path, &target_path)?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Convert ODG files to PNG using LibreOffice
fn convert_odg_to_png(
    odg_file_path: &Path,
    output_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::new("libreoffice");
    command
        .arg("--headless")
        .arg("--convert-to")
        .arg("png")
        .arg("--outdir")
        .arg(output_dir)
        .arg(odg_file_path);

    command.output()?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the current directory where the binary is executed
    let current_dir = env::current_dir()?;

    // Define the expected file name
    let helper_file_name = "_Wiki_build_helper.json";
    let helper_file_path = current_dir.join(helper_file_name);

    // Check if the file exists in the current directory
    if helper_file_path.exists() {
        // If the file exists, proceed to render the wiki
        let target_dir = current_dir.join("Wiki");
        recreate_directory_structure(&current_dir, &target_dir)?;
    } else {
        // If the file is not found, print an error message
        eprintln!(
            "Error: '{}' not found. The compiler needs to be at the top level of the Wiki directory, \
                on the same level as '{}'.",
            helper_file_name, helper_file_name
        );
        return Err("Required file not found".into());
    }

    Ok(())
}
