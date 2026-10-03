//! Source files and the mapping from byte offsets to line and column.

use std::fmt;
use std::sync::OnceLock;

use crate::span::FileId;

/// Largest accepted source file in bytes. Offsets are stored as `u32`.
pub const MAX_FILE_BYTES: usize = u32::MAX as usize;

/// A 1 based line and column (SPEC 12.1).
///
/// `column` is 1 plus the number of Unicode scalar values before the position on
/// its line. A tab counts as one. Lines are separated by LF only. A byte order
/// mark (U+FEFF) at byte 0 of the file is not counted, so line 1 column 1 starts
/// after it (D68). U+FEFF anywhere else counts like any other character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    /// 1 based line number.
    pub line: u32,
    /// 1 based column in Unicode scalar values.
    pub column: u32,
}

/// Why a file could not be added to a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// The text is longer than [`MAX_FILE_BYTES`].
    FileTooLarge,
    /// The map already holds `u32::MAX` files.
    TooManyFiles,
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddError::FileTooLarge => {
                write!(f, "source file is larger than {MAX_FILE_BYTES} bytes")
            }
            AddError::TooManyFiles => f.write_str("too many source files"),
        }
    }
}

impl std::error::Error for AddError {}

#[derive(Debug)]
struct SourceFile {
    path: String,
    text: String,
    /// Byte offset of the first byte of every line. Always starts with 0.
    line_starts: Vec<u32>,
    /// Column index, built on the first [`SourceMap::location`] call of the file.
    /// Entry `k` is the number of Unicode scalar values that start before byte
    /// `k * CHECKPOINT_STRIDE`; the last entry is the count for the whole text.
    checkpoints: OnceLock<Vec<u32>>,
}

impl SourceFile {
    /// Byte length of the byte order mark at byte 0, or 0 if there is none.
    fn bom_len(&self) -> usize {
        if self.text.starts_with(BOM) {
            BOM.len_utf8()
        } else {
            0
        }
    }

    /// Byte offset where columns of the 0 based line `idx` start counting.
    /// Equal to the line start, except on line 1 after a byte order mark.
    fn column_origin(&self, idx: usize, line_start: usize) -> usize {
        if idx == 0 {
            line_start.max(self.bom_len())
        } else {
            line_start
        }
    }

    /// Number of Unicode scalar values that start before byte `pos`, which must
    /// be a char boundary. Scans at most [`CHECKPOINT_STRIDE`] bytes, so a
    /// column costs the same on a long line as on a short one.
    fn scalars_before(&self, pos: usize) -> usize {
        let bytes = self.text.as_bytes();
        let checkpoints = self.checkpoints.get_or_init(|| build_checkpoints(bytes));
        let block = pos / CHECKPOINT_STRIDE;
        let base = checkpoints
            .get(block)
            .and_then(|&n| usize::try_from(n).ok())
            .unwrap_or(0);
        let start = block * CHECKPOINT_STRIDE;
        base + bytes.get(start..pos).map_or(0, count_scalar_starts)
    }
}

/// Bytes between two entries of the column index. Bounds the scan per lookup
/// and keeps the index at one `u32` per 256 bytes of text.
const CHECKPOINT_STRIDE: usize = 256;

/// Number of Unicode scalar values starting in `bytes`: every byte that is not
/// a UTF-8 continuation byte starts one.
fn count_scalar_starts(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count()
}

fn build_checkpoints(bytes: &[u8]) -> Vec<u32> {
    let mut checkpoints = Vec::with_capacity(bytes.len() / CHECKPOINT_STRIDE + 2);
    let mut total: u32 = 0;
    for chunk in bytes.chunks(CHECKPOINT_STRIDE) {
        checkpoints.push(total);
        // At most CHECKPOINT_STRIDE per chunk, and the text has at most
        // u32::MAX bytes, so the sum never saturates.
        let n = u32::try_from(count_scalar_starts(chunk)).unwrap_or(u32::MAX);
        total = total.saturating_add(n);
    }
    checkpoints.push(total);
    checkpoints
}

/// The byte order mark that is skipped at byte 0 (D68).
const BOM: char = '\u{FEFF}';

/// Owns the text of every source file of one compiler run.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Creates an empty source map.
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Adds a file. `path` is kept exactly as given and appears in rendered
    /// diagnostics.
    pub fn add(
        &mut self,
        path: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<FileId, AddError> {
        let text = text.into();
        if text.len() > MAX_FILE_BYTES {
            return Err(AddError::FileTooLarge);
        }
        let id = u32::try_from(self.files.len()).map_err(|_| AddError::TooManyFiles)?;
        if id == u32::MAX {
            return Err(AddError::TooManyFiles);
        }
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                // i < len <= u32::MAX, so i + 1 fits.
                line_starts.push(u32::try_from(i + 1).map_err(|_| AddError::FileTooLarge)?);
            }
        }
        self.files.push(SourceFile {
            path: path.into(),
            text,
            line_starts,
            checkpoints: OnceLock::new(),
        });
        Ok(FileId::from_raw(id))
    }

    /// Number of files in the map.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True if no file was added.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn file(&self, file: FileId) -> Option<&SourceFile> {
        self.files.get(usize::try_from(file.raw()).ok()?)
    }

    /// The path of a file as given to [`SourceMap::add`].
    pub fn path(&self, file: FileId) -> Option<&str> {
        self.file(file).map(|f| f.path.as_str())
    }

    /// The full text of a file.
    pub fn text(&self, file: FileId) -> Option<&str> {
        self.file(file).map(|f| f.text.as_str())
    }

    /// Number of lines of a file. An empty file has one line, and a trailing LF
    /// starts a further, empty line.
    pub fn line_count(&self, file: FileId) -> Option<usize> {
        self.file(file).map(|f| f.line_starts.len())
    }

    /// Line and column of a byte offset.
    ///
    /// Takes time independent of the length of the line (after a one time pass
    /// over the file on the first call), so many diagnostics on one long line
    /// stay linear in total.
    ///
    /// Returns `None` only for an unknown file. An offset past the end of the
    /// text is treated as the end of the text, and an offset inside a multi byte
    /// character as the start of that character, so a wrong span from a buggy or
    /// hostile caller degrades the position instead of failing.
    pub fn location(&self, file: FileId, offset: u32) -> Option<Location> {
        let f = self.file(file)?;
        let mut pos = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(f.text.len());
        while pos > 0 && !f.text.is_char_boundary(pos) {
            pos -= 1;
        }
        // Index of the last line start that is <= pos. line_starts[0] == 0, so it is >= 1.
        let after = f
            .line_starts
            .partition_point(|&s| usize::try_from(s).is_ok_and(|s| s <= pos));
        let line_idx = after.saturating_sub(1);
        let line_start = f
            .line_starts
            .get(line_idx)
            .and_then(|&s| usize::try_from(s).ok())
            .unwrap_or(0);
        // An offset inside or at the start of a leading byte order mark has
        // already snapped back to 0, so `origin <= pos` holds whenever pos > 0.
        let origin = f.column_origin(line_idx, line_start).min(pos);
        let chars = f
            .scalars_before(pos)
            .saturating_sub(f.scalars_before(origin));
        Some(Location {
            line: u32::try_from(after.max(1)).unwrap_or(u32::MAX),
            column: u32::try_from(chars).unwrap_or(u32::MAX).saturating_add(1),
        })
    }

    /// The text of a 1 based line without its LF, if the file and line exist.
    ///
    /// Line 1 starts after a byte order mark at byte 0, so the scalar value at
    /// index `column - 1` of the returned text is the one at that column.
    pub fn line_text(&self, file: FileId, line: u32) -> Option<&str> {
        let f = self.file(file)?;
        let idx = usize::try_from(line).ok()?.checked_sub(1)?;
        let line_start = usize::try_from(*f.line_starts.get(idx)?).ok()?;
        let start = f.column_origin(idx, line_start);
        let end = match f.line_starts.get(idx + 1) {
            Some(&next) => usize::try_from(next).ok()?.checked_sub(1)?,
            None => f.text.len(),
        };
        f.text.get(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(line: u32, column: u32) -> Option<Location> {
        Some(Location { line, column })
    }

    #[test]
    fn ascii_lines_and_columns() {
        let mut map = SourceMap::new();
        let f = map.add("a.ostl", "fn main()\n  print(1)\n").unwrap();
        assert_eq!(map.location(f, 0), loc(1, 1));
        assert_eq!(map.location(f, 3), loc(1, 4));
        assert_eq!(map.location(f, 9), loc(1, 10));
        assert_eq!(map.location(f, 10), loc(2, 1));
        assert_eq!(map.location(f, 12), loc(2, 3));
        assert_eq!(map.location(f, 21), loc(3, 1));
        assert_eq!(map.line_count(f), Some(3));
        assert_eq!(map.line_text(f, 2), Some("  print(1)"));
        assert_eq!(map.line_text(f, 3), Some(""));
        assert_eq!(map.line_text(f, 4), None);
        assert_eq!(map.line_text(f, 0), None);
    }

    #[test]
    fn columns_count_scalar_values_not_bytes_or_utf16() {
        let mut map = SourceMap::new();
        // e acute (2 bytes), grinning face (4 bytes, 2 UTF-16 units), tab.
        let text = "\u{e9}\u{1F600}\tx";
        let f = map.add("u.ostl", text).unwrap();
        let x = u32::try_from(text.find('x').unwrap()).unwrap();
        assert_eq!(x, 7);
        assert_eq!(map.location(f, x), loc(1, 4));
    }

    #[test]
    fn hostile_offsets_degrade_without_panic() {
        let mut map = SourceMap::new();
        let f = map.add("h.ostl", "a\u{1F600}\nb").unwrap();
        // Inside the 4 byte character: snaps back to its start.
        assert_eq!(map.location(f, 3), loc(1, 2));
        // Past the end: end of text.
        assert_eq!(map.location(f, u32::MAX), loc(2, 2));
        assert_eq!(map.location(FileId::from_raw(7), 0), None);
        assert_eq!(map.path(FileId::from_raw(7)), None);
    }

    #[test]
    fn carriage_return_is_not_a_line_break() {
        let mut map = SourceMap::new();
        let f = map.add("cr.ostl", "a\r\nb\rc").unwrap();
        assert_eq!(map.location(f, 2), loc(1, 3));
        assert_eq!(map.location(f, 5), loc(2, 3));
    }

    #[test]
    fn empty_file_has_one_line() {
        let mut map = SourceMap::new();
        let f = map.add("e.ostl", "").unwrap();
        assert_eq!(map.location(f, 0), loc(1, 1));
        assert_eq!(map.line_count(f), Some(1));
        assert_eq!(map.line_text(f, 1), Some(""));
    }

    #[test]
    fn bom_at_byte_zero_is_not_a_column() {
        let mut map = SourceMap::new();
        let f = map.add("b.ostl", "\u{FEFF}@x\nyz").unwrap();
        // The BOM itself, any offset inside it and the first byte after it are 1:1.
        assert_eq!(map.location(f, 0), loc(1, 1));
        assert_eq!(map.location(f, 1), loc(1, 1));
        assert_eq!(map.location(f, 2), loc(1, 1));
        assert_eq!(map.location(f, 3), loc(1, 1));
        assert_eq!(map.location(f, 4), loc(1, 2));
        assert_eq!(map.location(f, 5), loc(1, 3));
        // Line 2 is not affected.
        assert_eq!(map.location(f, 6), loc(2, 1));
        assert_eq!(map.location(f, 7), loc(2, 2));
        assert_eq!(map.location(f, u32::MAX), loc(2, 3));
        assert_eq!(map.line_text(f, 1), Some("@x"));
        assert_eq!(map.line_text(f, 2), Some("yz"));
        assert_eq!(map.text(f), Some("\u{FEFF}@x\nyz"));
    }

    #[test]
    fn bom_only_file() {
        let mut map = SourceMap::new();
        let f = map.add("o.ostl", "\u{FEFF}").unwrap();
        assert_eq!(map.location(f, 0), loc(1, 1));
        assert_eq!(map.location(f, 3), loc(1, 1));
        assert_eq!(map.location(f, u32::MAX), loc(1, 1));
        assert_eq!(map.line_count(f), Some(1));
        assert_eq!(map.line_text(f, 1), Some(""));
    }

    #[test]
    fn bom_followed_by_line_feed() {
        let mut map = SourceMap::new();
        let f = map.add("n.ostl", "\u{FEFF}\n\u{FEFF}a").unwrap();
        assert_eq!(map.location(f, 3), loc(1, 1));
        assert_eq!(map.line_text(f, 1), Some(""));
        // A BOM at the start of line 2 is not at byte 0 and counts.
        assert_eq!(map.location(f, 4), loc(2, 1));
        assert_eq!(map.location(f, 7), loc(2, 2));
        assert_eq!(map.line_text(f, 2), Some("\u{FEFF}a"));
    }

    #[test]
    fn only_the_first_bom_is_skipped() {
        let mut map = SourceMap::new();
        let f = map.add("d.ostl", "\u{FEFF}\u{FEFF}@").unwrap();
        assert_eq!(map.location(f, 3), loc(1, 1));
        assert_eq!(map.location(f, 6), loc(1, 2));
        assert_eq!(map.line_text(f, 1), Some("\u{FEFF}@"));
    }

    #[test]
    fn bom_after_byte_zero_counts_as_a_column() {
        let mut map = SourceMap::new();
        let f = map.add("m.ostl", "a\u{FEFF}@").unwrap();
        assert_eq!(map.location(f, 4), loc(1, 3));
        assert_eq!(map.line_text(f, 1), Some("a\u{FEFF}@"));
    }

    #[test]
    fn caret_index_matches_column_after_bom() {
        let mut map = SourceMap::new();
        let text = "\u{FEFF}let \u{e9} = @";
        let f = map.add("c.ostl", text).unwrap();
        let at = text.find('@').unwrap();
        let col = map.location(f, u32::try_from(at).unwrap()).unwrap().column;
        let line = map.line_text(f, 1).unwrap();
        let idx = usize::try_from(col - 1).unwrap();
        assert_eq!(line.chars().nth(idx), Some('@'));
    }

    /// The column rule of [`Location`], counted directly from the text.
    fn naive_location(text: &str, offset: usize) -> Location {
        let mut pos = offset.min(text.len());
        while !text.is_char_boundary(pos) {
            pos -= 1;
        }
        let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
        let bom = if text.starts_with(BOM) {
            BOM.len_utf8()
        } else {
            0
        };
        let origin = if line_start == 0 {
            bom.min(pos)
        } else {
            line_start
        };
        Location {
            line: u32::try_from(text[..pos].matches('\n').count() + 1).unwrap(),
            column: u32::try_from(text[origin..pos].chars().count() + 1).unwrap(),
        }
    }

    fn assert_matches_naive(text: &str) {
        let mut map = SourceMap::new();
        let f = map.add("n.ostl", text).unwrap();
        for offset in 0..=text.len() + 2 {
            let got = map.location(f, u32::try_from(offset).unwrap());
            assert_eq!(got, Some(naive_location(text, offset)), "offset {offset}");
        }
    }

    #[test]
    fn columns_across_index_checkpoints() {
        // Lengths just below, at and above one and two strides, multi byte
        // characters straddling a checkpoint, a BOM and several lines.
        for len in [0, 1, 255, 256, 257, 511, 512, 513] {
            assert_matches_naive(&"a".repeat(len));
        }
        for shift in 0..4 {
            let mut text = "a".repeat(254 + shift);
            text.push_str("\u{1F600}\u{e9}\u{202E}x\n");
            text.push_str(&"\u{20AC}".repeat(200));
            text.push_str("\n\u{FEFF}y");
            assert_matches_naive(&text);
            assert_matches_naive(&format!("\u{FEFF}{text}"));
        }
        assert_matches_naive(&"\u{202E}".repeat(700));
        assert_matches_naive(&"}\n".repeat(300));
    }

    #[test]
    fn last_column_of_a_long_line() {
        let text = format!("fn main()\n  print(\"{}\")\n", "\u{202E}".repeat(100_000));
        let mut map = SourceMap::new();
        let f = map.add("l.ostl", text.as_str()).unwrap();
        let end = u32::try_from(text.rfind('"').unwrap()).unwrap();
        assert_eq!(map.location(f, end), loc(2, 100_010));
        assert_eq!(map.location(f, end + 1), loc(2, 100_011));
    }

    /// Asserts that `run` does linear work in the size of its input, without an
    /// absolute time bound (D74). The rule of `ostrel_syntax::test_support`:
    /// one run on an input of size `small * 16` against 16 runs on inputs of
    /// size `small`, alternating, fastest of three rounds per side. Linear work
    /// gives a ratio of about 1, quadratic work about 16; the bound is 4.
    fn assert_linear<T, R>(small: usize, build: impl Fn(usize) -> T, run: impl Fn(&T) -> R) {
        use std::time::{Duration, Instant};
        const FACTOR: usize = 16;
        const MAX_RATIO: u32 = 4;
        let smalls: Vec<T> = (0..FACTOR).map(|_| build(small)).collect();
        let large = build(small * FACTOR);
        let mut batch_time = Duration::MAX;
        let mut large_time = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            let outputs: Vec<R> = smalls.iter().map(&run).collect();
            batch_time = batch_time.min(start.elapsed());
            drop(outputs);

            let start = Instant::now();
            let output = run(&large);
            large_time = large_time.min(start.elapsed());
            drop(output);
        }
        let bound = batch_time.max(Duration::from_micros(100)) * MAX_RATIO;
        assert!(
            large_time < bound,
            "not linear: {batch_time:?} for {FACTOR} inputs of size {small}, \
             {large_time:?} for one input of size {}",
            small * FACTOR
        );
    }

    /// Red #1069: one diagnostic per character of one long line.
    fn every_column_of_one_line(n: usize, unit: &str) {
        assert_linear(
            n,
            |n| {
                let mut map = SourceMap::new();
                let f = map.add("x.ostl", format!("{}\n", unit.repeat(n))).unwrap();
                (map, f, n)
            },
            |(map, f, n)| {
                let step = unit.len();
                (0..*n)
                    .map(|i| {
                        map.location(*f, u32::try_from(i * step).unwrap())
                            .unwrap()
                            .column
                    })
                    .max()
            },
        );
    }

    #[test]
    fn many_locations_on_one_line_are_linear() {
        every_column_of_one_line(4_000, "\u{202E}");
        every_column_of_one_line(4_000, "}");
    }
}
