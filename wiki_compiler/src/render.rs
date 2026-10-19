use oxipng::StripChunks;
use oxipng::{InFile, Options as OxipngOptions, OutFile, optimize};
use resvg::{render, tiny_skia::Pixmap, usvg};
use std::error::Error;
use std::path::Path;
use usvg::{Options, Rect, Transform, Tree, decompress_svgz};

use crate::optimise_cache::{OptimiseCache, Outcome};

/// Reads an SVG or SVGZ file's bytes, decompressing them if needed.
///
/// SVGZ is gzip-compressed SVG. The check uses the file's bytes instead of its
/// extension, because every gzip stream starts with the magic bytes
/// `0x1f, 0x8b` regardless of the file name.
pub fn load_svg_data(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let data = std::fs::read(path)?;

    if data.starts_with(&[0x1f, 0x8b]) {
        Ok(decompress_svgz(&data)?)
    } else {
        Ok(data)
    }
}

/// Reads and parses an SVG or SVGZ file into a tree that can be rendered any
/// number of times.
///
/// Parsing is separate from rendering so one parse can be shared. Without
/// this, an SVG with a full image and ten genlist crops would be read,
/// decompressed and parsed eleven times, once per PNG.
///
/// The renders can also run concurrently, because `usvg::Tree` is
/// `Send + Sync`.
pub fn parse_svg(path: &Path) -> Result<Tree, Box<dyn Error>> {
    let svg_data = load_svg_data(path)?;
    let options = Options::default();
    Ok(Tree::from_data(&svg_data, &options)?)
}

/// Renders an already-parsed SVG to a PNG, or only a cropped region of it when
/// `crop_rect` is given.
///
/// The full image and every genlist crop use this one function. A full render
/// is the same as cropping the whole canvas.
///
/// When cropping, the pixmap is only as big as the crop, and the SVG is shifted
/// so the crop's top-left corner lands at the pixmap's `(0, 0)`.
///
/// For example, cropping a 40x30 region that starts at `(100, 50)`:
///
/// - the pixmap is 40x30 pixels, not the full canvas size
/// - the SVG is translated by `(-100, -50)` before drawing
/// - everything outside the `(100, 50)` to `(140, 80)` region falls outside the
///   pixmap and is not drawn
///
/// The PNG is encoded to bytes in memory and passed to the cache, which writes
/// it out. The cache identifies an image by its own bytes, so it can recognise
/// one it has optimised before. The SVG's file size cannot do that, because it
/// says nothing about how big the rendered image is.
pub fn render_tree_to_png(
    tree: &Tree,
    png_file_path: &Path,
    crop_rect: Option<Rect>,
    cache: &OptimiseCache,
) -> Result<Outcome, Box<dyn Error>> {
    match crop_rect {
        Some(crop_rect) => {
            // Round up so a fractional bounding box is not clipped a pixel
            // short. For example, a stroke ending at x = 39.4 needs 40 pixels,
            // not 39.
            let width = crop_rect.width().ceil() as u32;
            let height = crop_rect.height().ceil() as u32;
            let mut pixmap = Pixmap::new(width, height).ok_or("Failed to create pixmap")?;

            // Move the crop's top-left corner to the pixmap's `(0, 0)`.
            let ts = usvg::Transform::default().pre_translate(-crop_rect.x(), -crop_rect.y());

            resvg::render(tree, ts, &mut pixmap.as_mut());
            cache.write_optimised(&pixmap.encode_png()?, png_file_path)
        }
        None => {
            // Without a crop, the pixmap uses the SVG's own width and height.
            let pixmap_size = tree.size();
            let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32)
                .ok_or("Failed to create pixmap")?;

            // The identity transform draws the SVG exactly as authored.
            let transform = Transform::default();

            render(tree, transform, &mut pixmap.as_mut());
            cache.write_optimised(&pixmap.encode_png()?, png_file_path)
        }
    }
}

/// The oxipng preset every PNG is optimised with.
///
/// It is a named constant because the optimisation cache only holds results
/// for the preset they were measured with. The cache reads this value to know
/// when its records are out of date.
pub const OXIPNG_PRESET: u8 = 2;

/// Losslessly shrinks a PNG in place using oxipng.
///
/// Preset 2 is oxipng's default. A full rebuild can optimise thousands of
/// PNGs, and higher presets give only small size gains for much more CPU time.
///
/// `StripChunks::Safe` removes only metadata chunks that do not affect how the
/// image displays, such as text comments and timestamps.
pub fn optimise_png(png_file_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut options = OxipngOptions::from_preset(OXIPNG_PRESET);
    options.strip = StripChunks::Safe;

    let input_file = InFile::Path(png_file_path.to_path_buf());

    // `path: None` with `preserve_attrs: true` overwrites the input file in
    // place and keeps its permissions and timestamps.
    let output_file = OutFile::Path {
        path: None,
        preserve_attrs: true,
    };

    optimize(&input_file, &output_file, &options)?;

    Ok(())
}
