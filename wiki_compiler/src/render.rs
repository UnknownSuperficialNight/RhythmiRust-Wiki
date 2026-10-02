use oxipng::StripChunks;
use oxipng::{InFile, Options as OxipngOptions, OutFile, optimize};
use resvg::{render, tiny_skia::Pixmap, usvg};
use std::error::Error;
use std::path::Path;
use usvg::{Options, Rect, Transform, Tree, decompress_svgz};

/// Reads an SVG or SVGZ file's raw bytes, decompressing first if needed.
///
/// SVGZ is just gzip-compressed SVG XML, so the only difference between
/// the two formats on disk is whether that gzip layer is present. Rather
/// than have every caller check the file extension, this checks the
/// actual bytes: a gzip stream always starts with the two magic bytes
/// `0x1f, 0x8b`, regardless of what the file happens to be named.
pub fn load_svg_data(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let data = std::fs::read(path)?;

    // Gzip magic bytes. SVGZ is gzip-compressed SVG.
    if data.starts_with(&[0x1f, 0x8b]) {
        Ok(decompress_svgz(&data)?)
    } else {
        Ok(data)
    }
}

/// Renders an SVG to PNG, or just a cropped region of it when `crop_rect`
/// is given.
///
/// Both the full Main.svg render and every genlist colour crop go through
/// this one function, rather than being two separate code paths - the
/// pixmap setup, the actual draw call, and the PNG optimisation step only
/// need to be written once, and a full render is really just the "crop
/// the whole canvas" special case.
///
/// When cropping, the pixmap is allocated at exactly the crop's size, not
/// the SVG's full canvas size, and the SVG is shifted so the crop's
/// top-left corner lands at the pixmap's origin (0, 0). For example,
/// cropping a 40x30 region starting at (100, 50) out of a larger SVG:
///
/// - the pixmap is created at 40x30 pixels, not the full canvas size
/// - the SVG is translated by (-100, -50) before drawing, so the part
///   that used to sit at (100, 50) now lines up with the pixmap's (0, 0)
/// - everything outside the original (100, 50)-(140, 80) region simply
///   falls outside the pixmap's bounds and is never drawn
pub fn render_svg_to_png(
    svg_file_path: &Path,
    png_file_path: &Path,
    crop_rect: Option<Rect>,
) -> Result<(), Box<dyn Error>> {
    // Read the raw SVG/SVGZ bytes and parse them into a tree of drawable nodes
    let svg_data = load_svg_data(svg_file_path)?;
    let options = Options::default();
    let tree = Tree::from_data(&svg_data, &options)?;

    match crop_rect {
        Some(crop_rect) => {
            // Pixmap sized to the crop rather than the whole SVG - rounded
            // up so a fractional bounding box (e.g. a stroke ending at
            // x=39.4) doesn't get clipped a pixel short
            let width = crop_rect.width().ceil() as u32;
            let height = crop_rect.height().ceil() as u32;
            // The actual pixel buffer we're about to draw into
            let mut pixmap = Pixmap::new(width, height).ok_or("Failed to create pixmap")?;

            // Shift the whole SVG so the crop's top-left corner becomes the
            // pixmap's (0, 0) - see the example above
            let ts = usvg::Transform::default().pre_translate(-crop_rect.x(), -crop_rect.y());

            // Draw the SVG tree into the pixmap using that shifted transform
            resvg::render(&tree, ts, &mut pixmap.as_mut());
            // Encode the pixmap's pixels out to an actual PNG file on disk
            pixmap.save_png(png_file_path)?;
            optimise_png(png_file_path)?;
        }
        None => {
            // No crop requested - the pixmap is sized to the SVG's own
            // intrinsic width/height, i.e. the whole image
            let pixmap_size = tree.size();
            let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32)
                .ok_or("Failed to create pixmap")?;
            // Identity transform - draw the SVG exactly as authored, nothing to shift
            let transform = Transform::default();

            render(&tree, transform, &mut pixmap.as_mut());
            pixmap.save_png(png_file_path)?;
            optimise_png(png_file_path)?;
        }
    }

    Ok(())
}

/// Losslessly shrinks a PNG in place using oxipng.
///
/// `from_preset(2)` is oxipng's own default preset - it's deliberately not
/// the strongest available setting. A full wiki rebuild can mean
/// optimising thousands of PNGs in one run, and oxipng's own documentation
/// notes that higher presets buy better compression with steeply
/// diminishing returns for a lot more CPU time. Preset 2 was chosen to
/// keep a full rebuild fast without leaving obvious size savings on the
/// table.
///
/// `StripChunks::Safe` removes only PNG metadata chunks that don't affect
/// how the image displays (things like text comments or timestamps) -
/// nothing that could change how the image looks or decodes is touched.
pub fn optimise_png(png_file_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut options = OxipngOptions::from_preset(2);
    options.strip = StripChunks::Safe;

    // Idk why they wrapped &Path in InFile and OutFile here there should be a alternative but ok
    let input_file = InFile::Path(png_file_path.to_path_buf());
    // path: None + preserve_attrs: true means "overwrite the input file in
    // place, keeping its existing permissions and timestamps"
    let output_file = OutFile::Path {
        path: None,
        preserve_attrs: true,
    };

    optimize(&input_file, &output_file, &options)?;

    Ok(())
}
