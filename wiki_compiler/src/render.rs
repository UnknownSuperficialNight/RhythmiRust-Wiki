use oxipng::StripChunks;
use oxipng::{InFile, Options as OxipngOptions, OutFile, optimize};
use resvg::{render, tiny_skia::Pixmap, usvg};
use std::path::Path;
use usvg::{Options, Transform, Tree};

/// Render an SVG file to PNG
pub fn render_svg_to_png(
    svg_file_path: &Path,
    png_file_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the SVG file
    let svg_data = std::fs::read(svg_file_path)?;

    // Parse the SVG data
    let options = Options::default();
    let tree = Tree::from_data(&svg_data, &options)?;

    // Create a pixmap to render the SVG
    let pixmap_size = tree.size(); // Use the `size()` method
    let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32)
        .ok_or("Failed to create pixmap")?;

    // Create a default transform (identity transform)
    let transform = Transform::default();

    // Render the SVG into the pixmap
    render(&tree, transform, &mut pixmap.as_mut());

    // Save the pixmap as a PNG file
    pixmap.save_png(png_file_path)?;

    optimise_png(png_file_path)?;

    Ok(())
}

pub fn optimise_png(png_file_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Optimize the resulting PNG data
    let mut options = OxipngOptions::from_preset(2);
    options.strip = StripChunks::Safe;

    // Create InFile and OutFile instances
    // Idk why they wrapped &Path in InFile and OutFile here there should be a alternative but ok
    let input_file = InFile::Path(png_file_path.to_path_buf());
    let output_file = OutFile::Path {
        path: None,
        preserve_attrs: true,
    };

    optimize(&input_file, &output_file, &options)?;

    Ok(())
}
