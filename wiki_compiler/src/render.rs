use oxipng::StripChunks;
use oxipng::{InFile, Options as OxipngOptions, OutFile, optimize};
use resvg::{render, tiny_skia::Pixmap, usvg};
use std::path::Path;
use usvg::{Options, Rect, Transform, Tree};

/// Render an SVG file to PNG with optional cropping
///
/// This function handles both full SVG rendering and cropped sections
/// based on the provided crop_rect parameter.
///
/// Uses resvg for rendering and oxipng for PNG optimization
pub fn render_svg_to_png(
    svg_file_path: &Path,
    png_file_path: &Path,
    crop_rect: Option<Rect>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the SVG file
    let svg_data = std::fs::read(svg_file_path)?;

    // Parse the SVG data
    let options = Options::default();
    let tree = Tree::from_data(&svg_data, &options)?;

    match crop_rect {
        Some(crop_rect) => {
            // Create a pixmap with the size of the rect (rounded up)
            let width = crop_rect.width().ceil() as u32;
            let height = crop_rect.height().ceil() as u32;
            let mut pixmap = Pixmap::new(width, height).ok_or("Failed to create pixmap")?;

            // Create transform that translates rect top-left to (0,0)
            let ts = usvg::Transform::default().pre_translate(-crop_rect.x(), -crop_rect.y());

            // Render the clipped SVG tree into the pixmap
            resvg::render(&tree, ts, &mut pixmap.as_mut());

            // Save the rendered image as PNG
            pixmap.save_png(png_file_path)?;

            // Optimise the PNG
            optimise_png(png_file_path)?;
        }
        None => {
            // Create a pixmap to render the SVG
            let pixmap_size = tree.size();
            let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32)
                .ok_or("Failed to create pixmap")?;

            // Create a default transform (identity transform)
            let transform = Transform::default();

            // Render the SVG into the pixmap
            render(&tree, transform, &mut pixmap.as_mut());

            // Save the pixmap as a PNG file
            pixmap.save_png(png_file_path)?;

            // Optimise the PNG
            optimise_png(png_file_path)?;
        }
    }

    Ok(())
}

/// Optimise PNG file using oxipng with safe chunk stripping
///
/// Uses oxipng with preset 2 (balanced compression) and strips safe chunks
///
/// Note: Wraps &Path in InFile and OutFile for compatibility with oxipng's API
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
