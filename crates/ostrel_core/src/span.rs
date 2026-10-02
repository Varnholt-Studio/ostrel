//! File identifiers and byte spans.

/// Identifies one source file inside a [`SourceMap`](crate::SourceMap).
///
/// A `FileId` is only meaningful for the map that created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl FileId {
    /// Creates a file id from its raw index. Used by the source map and by tests.
    pub const fn from_raw(raw: u32) -> Self {
        FileId(raw)
    }

    /// Returns the raw index of this file id.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A half open byte range `start..end` in one source file.
///
/// Offsets are UTF-8 byte offsets into the file text. Line and column are
/// computed only when a diagnostic is rendered, see
/// [`SourceMap::location`](crate::SourceMap::location). Twelve bytes keep AST
/// nodes small (D54).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    /// The file the offsets refer to.
    pub file: FileId,
    /// Byte offset of the first byte.
    pub start: u32,
    /// Byte offset one past the last byte. Equal to `start` for an empty span.
    pub end: u32,
}

impl Span {
    /// Creates a span. If `end` is smaller than `start`, the two are swapped, so a
    /// span is always well formed.
    pub const fn new(file: FileId, start: u32, end: u32) -> Self {
        if end < start {
            Span {
                file,
                start: end,
                end: start,
            }
        } else {
            Span { file, start, end }
        }
    }

    /// Creates an empty span at `offset`, for example for an end of file diagnostic.
    pub const fn point(file: FileId, offset: u32) -> Self {
        Span {
            file,
            start: offset,
            end: offset,
        }
    }

    /// Length of the span in bytes. Zero for a span built by hand with `end`
    /// before `start`.
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// True if the span covers no byte.
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The smallest span covering `self` and `other`, or `None` if they belong to
    /// different files.
    pub fn to(self, other: Span) -> Option<Span> {
        if self.file != other.file {
            return None;
        }
        Some(Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_orders_offsets() {
        let f = FileId::from_raw(0);
        let s = Span::new(f, 9, 4);
        assert_eq!((s.start, s.end, s.len()), (4, 9, 5));
        assert!(Span::point(f, 3).is_empty());
    }

    #[test]
    fn join_requires_same_file() {
        let a = Span::new(FileId::from_raw(0), 2, 4);
        let b = Span::new(FileId::from_raw(0), 7, 8);
        assert_eq!(a.to(b), Some(Span::new(FileId::from_raw(0), 2, 8)));
        assert_eq!(a.to(Span::new(FileId::from_raw(1), 0, 1)), None);
    }

    #[test]
    fn span_stays_small() {
        assert_eq!(std::mem::size_of::<Span>(), 12);
    }
}
