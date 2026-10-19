use owo_colors::OwoColorize;
use std::path::Path;
use std::time::Duration;

/// Formats a duration for display instead of using `Duration`'s `Debug` output.
///
/// `Debug` output has full floating-point precision and switches units
/// inconsistently, so `530.411942ms` and `1.045175306s` end up in the same
/// column. This always uses one of two formats:
///
/// - under 1000ms: whole milliseconds, e.g. `530ms`
/// - 1000ms and over: seconds to two decimals, e.g. `1.05s`
pub fn format_duration(d: Duration) -> String {
    let millis = d.as_secs_f64() * 1000.0;
    if millis >= 1000.0 {
        format!("{:.2}s", millis / 1000.0)
    } else {
        format!("{millis:.0}ms")
    }
}

/// Shortens an absolute path to the part under `base`, for display.
///
/// For example, with `/home/user/RhythmiRust-Wiki` as the base,
/// `/home/user/RhythmiRust-Wiki/Main Window/Settings/data.json` becomes
/// `Main Window/Settings/data.json`.
///
/// If `path` is not under `base`, the original path is shown instead of
/// panicking over a display-only problem.
pub fn relative_label(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The visible width of the longest regular tag, `"[GENLIST 10.00s]"`.
///
/// Every other tag is padded to this width so paths start in the same column.
/// Only `[GENLIST CUSTOM ...]` is wider, and it breaks the alignment.
const REGULAR_TAG_WIDTH: usize = 16;

/// Shown next to the time of a PNG that oxipng was skipped on. See
/// `OptimiseCache`.
///
/// It sits inside the tag, so a longer mark needs `REGULAR_TAG_WIDTH` and
/// `DETAIL_TIME_WIDTH` widened to match.
const SKIPPED_MARK: &str = "skip";

/// Splits a path into a dimmed folder part and a normal-brightness filename.
///
/// For example, `Main Window/Settings/` is dimmed and `Main.svgz` is not. The
/// filenames line up as a column, and the folder is still visible to tell apart
/// two files with the same name.
fn format_location(location: &str) -> String {
    match location.rfind('/') {
        Some(i) => {
            // Split right after the last '/', so `dir` keeps its trailing slash.
            let (dir, file) = location.split_at(i + 1);
            format!("{}{}", dir.dimmed(), file)
        }
        // A bare filename has no folder to show.
        None => location.to_string(),
    }
}

/// The kind of file a status line reports on.
///
/// It decides the line's tag text and colour.
pub enum FileKind {
    Data,
    Png,
    Svg,
    Genlist,
    GenlistCustom,
}

impl FileKind {
    // The text shown inside the tag's brackets.
    fn label(&self) -> &'static str {
        match self {
            FileKind::Data => "DATA",
            FileKind::Png => "PNG",
            FileKind::Svg => "SVG",
            FileKind::Genlist => "GENLIST",
            FileKind::GenlistCustom => "GENLIST CUSTOM",
        }
    }

    // Applies this kind's colour to some text.
    fn paint(&self, text: &str) -> String {
        match self {
            FileKind::Data => text.cyan().to_string(),
            FileKind::Png => text.green().to_string(),
            FileKind::Svg => text.blue().to_string(),
            FileKind::Genlist => text.magenta().to_string(),
            FileKind::GenlistCustom => text.bright_magenta().to_string(),
        }
    }

    /// Builds a tag with the duration inside it, followed by padding so the
    /// path after it starts in a consistent column.
    ///
    /// The padding is measured on the plain text and added after colouring. If
    /// it were measured after colouring, the invisible ANSI escape codes would
    /// count as visible characters, and every tag would be padded too little.
    ///
    /// For example, `FileKind::Png.tag(Duration::from_millis(910), false)`
    /// produces `[PNG 910ms]` followed by 5 spaces:
    ///
    /// ```text
    /// "PNG" (3) + "910ms" (5) + "[", " " and "]" (3) = 11 visible characters
    /// 16 - 11 = 5 spaces of padding
    /// ```
    ///
    /// A skipped PNG gets `SKIPPED_MARK` after the duration, e.g.
    /// `[PNG 910ms skip]`. It fits in the same width.
    fn tag(&self, elapsed: Duration, skipped: bool) -> String {
        let duration = format_duration(elapsed);

        // The mark adds a space plus its own text.
        let mark_width = if skipped { SKIPPED_MARK.len() + 1 } else { 0 };

        // The 3 extra characters are the "[", the space after the label, and
        // the closing "]".
        let visible = self.label().len() + duration.len() + 3 + mark_width;
        let padding = " ".repeat(REGULAR_TAG_WIDTH.saturating_sub(visible));
        let mark = if skipped {
            format!(" {}", SKIPPED_MARK.yellow())
        } else {
            String::new()
        };

        format!(
            "{}{}{}{}{}",
            self.paint(&format!("[{} ", self.label())), // coloured "[LABEL "
            duration.dimmed(),                           // dimmed duration in the middle
            mark,                                        // "skip", if oxipng was skipped
            self.paint("]"),                              // coloured closing bracket
            padding
        )
    }
}

/// Assembles one status line: a tag with the duration, then the path, then an
/// optional dimmed suffix such as a genlist's "(N colours)".
fn status_line(
    kind: &FileKind,
    location: &str,
    suffix: &str,
    elapsed: Duration,
    skipped: bool,
) -> String {
    format!(
        "  {}  {}{}",
        kind.tag(elapsed, skipped),
        format_location(location),
        suffix.dimmed()
    )
}

/// Prints one status line for a copied PNG, a rendered SVG, or a written
/// `data.json`.
///
/// `skipped` marks a PNG that oxipng was not run on.
pub fn log_processed(kind: FileKind, location: &str, elapsed: Duration, skipped: bool) {
    println!("{}", status_line(&kind, location, "", elapsed, skipped));
}

/// The width the timing on a dropdown line is padded to, e.g. `[12ms]`.
///
/// Without it, `[9ms]`, `[12ms]` and `[295ms skip]` would push the swatch after
/// them into different columns.
const DETAIL_TIME_WIDTH: usize = 12;

/// Builds one indented dropdown line under a genlist's header.
///
/// For example:
///
/// ```text
/// ↳ [12ms]   #D1C4E9 → Download Dir
/// ↳ [12ms skip]   #D1C4E9 → Download Dir
/// ```
///
/// Every PNG an SVG produces gets one line: the full image, labelled with the
/// source file's name, and each genlist crop, labelled with its hex swatch. The
/// full image is in the same dropdown because it is rendered by the same task
/// from the same parsed SVG, so its timing overlaps the crops'.
///
/// `label` is passed in already styled. Its visible width, at least 7
/// characters, keeps the arrows lined up.
///
/// The line ends in a newline, so lines can be joined and passed to
/// `log_genlist` as one block.
pub fn detail_line(elapsed: Duration, label: &str, name: &str, skipped: bool) -> String {
    let duration = format_duration(elapsed);

    // Padding is measured on the plain text, as in `FileKind::tag`. The 2 extra
    // characters are the "[" and "]" around the duration.
    let plain_width = duration.len() + 2 + if skipped { SKIPPED_MARK.len() + 1 } else { 0 };
    let padding = " ".repeat(DETAIL_TIME_WIDTH.saturating_sub(plain_width));
    let mark = if skipped {
        format!(" {}", SKIPPED_MARK.yellow())
    } else {
        String::new()
    };
    format!(
        "      {} {}{}{}{}{} {} {} {}\n",
        "↳".dimmed(),
        "[".dimmed(),
        duration.dimmed(),
        mark,
        "]".dimmed(),
        padding,
        label,
        "→".dimmed(),
        name
    )
}

/// Prints a genlist's status line and all of its dropdown lines in a single
/// write.
///
/// Files finish in no fixed order on the thread pool. A single `print!` cannot
/// be interrupted by another thread's output, so the header and its dropdown
/// lines are built into one `String` first. One `println!` per line could let
/// another file's `[PNG ...]` line appear between them.
///
/// `count` is the number of colour crops. The full image is one of the
/// dropdown lines, but it is not a colour.
///
/// `elapsed` is shown in the header tag. For a genlist this is the sum of every
/// dropdown line's time.
pub fn log_genlist(
    kind: FileKind,
    location: &str,
    count: usize,
    elapsed: Duration,
    detail_lines: &str,
) {
    let noun = if count == 1 { "colour" } else { "colours" };
    let suffix = format!(" ({count} {noun})");
    let mut block = status_line(&kind, location, &suffix, elapsed, false);

    // The header needs its own newline. `detail_line` already ends each of its
    // lines in one.
    block.push('\n');
    block.push_str(detail_lines);
    print!("{block}");
}
