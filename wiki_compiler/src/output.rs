use owo_colors::OwoColorize;
use std::path::Path;
use std::time::Duration;

/// Formats a duration the way a human wants to read it, instead of
/// `Duration`'s `Debug` output.
///
/// `Duration`'s own formatting is full floating-point precision and
/// switches units inconsistently - `530.411942ms` next to `1.045175306s`
/// in the same column of output looks like noise, not data. This always
/// picks one of two fixed, glanceable formats instead:
///
/// - under 1000ms: whole milliseconds, e.g. `530ms`
/// - 1000ms and over: seconds to two decimals, e.g. `1.05s`
pub fn format_duration(d: Duration) -> String {
    // as_secs_f64 gives fractional seconds as a float, so *1000 converts
    // straight to milliseconds without a separate unit-conversion step
    let millis = d.as_secs_f64() * 1000.0;
    if millis >= 1000.0 {
        format!("{:.2}s", millis / 1000.0)
    } else {
        format!("{millis:.0}ms")
    }
}

/// Shortens an absolute path down to just the part under `base`, for
/// display purposes.
///
/// Every path the compiler touches starts out absolute (e.g.
/// `/home/user/RhythmiRust-Wiki/Main Window/Settings/data.json`), which is
/// far too long to read in a scrolling terminal log. Stripped against the
/// wiki root, that becomes `Main Window/Settings/data.json` instead. If
/// `path` somehow isn't under `base` at all (which shouldn't normally
/// happen, but would otherwise be a hard error), this falls back to
/// showing the original absolute path rather than panicking over a
/// display-only concern.
pub fn relative_label(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Visible width of the longest regular tag once padded, `"[GENLIST 10.00s]"`.
/// Every other tag is padded out to this width so paths start in the same
/// column regardless of which kind of file produced the line - only
/// `[GENLIST CUSTOM ...]`, which is wider still, breaks that alignment.
const REGULAR_TAG_WIDTH: usize = 16;

/// Splits a path into a dimmed folder part and a normal-brightness
/// filename, e.g. `Main Window/Settings/` dimmed followed by `Main.svgz`
/// at full brightness.
///
/// The intent is to let a person's eye run straight down the column of
/// filenames while the folder is still sitting right there, dimmed, the
/// moment it's actually needed to tell two identically-named files (two
/// different folders' `Main.svgz`, say) apart.
fn format_location(location: &str) -> String {
    match location.rfind('/') {
        Some(i) => {
            // split right after the last '/' so dir keeps its trailing
            // slash and file gets none of it
            let (dir, file) = location.split_at(i + 1);
            format!("{}{}", dir.dimmed(), file)
        }
        // No '/' at all - it's a bare filename with no folder to show
        None => location.to_string(),
    }
}

/// The kind of file a status line is reporting on, used to pick that
/// line's tag text and colour.
pub enum FileKind {
    Data,
    Png,
    Svg,
    Genlist,
    GenlistCustom,
}

impl FileKind {
    // Plain text label shown inside the tag's brackets, e.g. "SVG"
    fn label(&self) -> &'static str {
        match self {
            FileKind::Data => "DATA",
            FileKind::Png => "PNG",
            FileKind::Svg => "SVG",
            FileKind::Genlist => "GENLIST",
            FileKind::GenlistCustom => "GENLIST CUSTOM",
        }
    }

    // Applies this kind's colour to an arbitrary piece of text
    fn paint(&self, text: &str) -> String {
        match self {
            FileKind::Data => text.cyan().to_string(),
            FileKind::Png => text.green().to_string(),
            FileKind::Svg => text.blue().to_string(),
            FileKind::Genlist => text.magenta().to_string(),
            FileKind::GenlistCustom => text.bright_magenta().to_string(),
        }
    }

    /// Builds a tag with the duration folded inside it, e.g. `[PNG 910ms]`,
    /// followed by padding so the path after it starts in a consistent
    /// column.
    ///
    /// The padding has to be measured against the *plain* label and
    /// duration text, then added after colouring, not before. If it were
    /// measured after colouring, the invisible ANSI escape codes owo-colors
    /// wraps the text in would count as extra "characters" towards the
    /// padding width, and every tag would end up under-padded by however
    /// many bytes its escape codes added.
    ///
    /// For example, `FileKind::Png.tag(Duration::from_millis(910))`
    /// produces the coloured equivalent of `"[PNG 910ms]  "` - 3 (for
    /// `"PNG"`) + 5 (for `"910ms"`) + 3 (for `"[ "` and `"]"`) = 11 visible
    /// characters, padded up to the 16-character `REGULAR_TAG_WIDTH`.
    fn tag(&self, elapsed: Duration) -> String {
        let duration = format_duration(elapsed);
        // "[" + label + " " + duration + "]" adds 3 punctuation characters
        // beyond the label and duration text themselves
        let visible = self.label().len() + duration.len() + 3;
        let padding = " ".repeat(REGULAR_TAG_WIDTH.saturating_sub(visible));

        format!(
            "{}{}{}{}",
            self.paint(&format!("[{} ", self.label())), // coloured "[LABEL "
            duration.dimmed(),                           // dimmed duration in the middle
            self.paint("]"),                              // coloured closing bracket
            padding
        )
    }
}

/// Assembles one status line: a tag with the duration in it, then the path,
/// then an optional dimmed suffix (used for a genlist's "(N colours)").
fn status_line(kind: &FileKind, location: &str, suffix: &str, elapsed: Duration) -> String {
    format!(
        "  {}  {}{}",
        kind.tag(elapsed),
        format_location(location),
        suffix.dimmed()
    )
}

/// Prints one status line for a plain processed file (a copied PNG, a
/// rendered SVG, or a written data.json).
pub fn log_processed(kind: FileKind, location: &str, elapsed: Duration) {
    println!("{}", status_line(&kind, location, "", elapsed));
}

/// Prints a genlist's status line together with all of its indented
/// per-colour crop lines, as a single write.
///
/// The compiler processes many files in parallel on a shared thread pool,
/// and every file's status line is printed as soon as that file finishes -
/// there's no fixed order. `println!`/`print!` each briefly lock stdout
/// for the duration of that one call, so a single call can never be split
/// up mid-write by another thread's output landing in the middle of it.
/// A genlist's header line plus its crop lines are therefore assembled
/// into one `String` first and printed with a single `print!` call here,
/// rather than one `println!` per line - the latter would risk another
/// thread's one-line `[PNG ...]` status appearing wedged between a
/// genlist's header and its own crop lines.
pub fn log_genlist(
    kind: FileKind,
    location: &str,
    count: usize,
    elapsed: Duration,
    crop_lines: &str,
) {
    let noun = if count == 1 { "colour" } else { "colours" };
    let suffix = format!(" ({count} {noun})");
    let mut block = status_line(&kind, location, &suffix, elapsed);
    block.push('\n'); // the header line needs its own newline before crop lines are appended
    block.push_str(crop_lines); // crop_lines already ends each of its own lines in '\n'
    print!("{block}");
}
