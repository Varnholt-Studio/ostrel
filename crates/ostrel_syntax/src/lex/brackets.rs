//! A stack of open brackets of three kinds, shared by the lexer and the
//! parser (D166, ARCHITECTURE 3.5).
//!
//! A closer closes the nearest open bracket of its own kind and every bracket
//! above it; with no open bracket of its kind it closes nothing. Each entry is
//! pushed and popped once and the stack counts its entries per kind, so a
//! closer costs amortized constant time, also for hostile input with millions
//! of brackets.

use super::TokenKind;

/// The kind of an opening or closing bracket token: 0 for `(` `)`, 1 for
/// `[` `]`, 2 for `{` `}`, and whether the token opens.
pub(crate) const fn bracket_of(kind: TokenKind) -> Option<(usize, bool)> {
    match kind {
        TokenKind::LParen => Some((0, true)),
        TokenKind::RParen => Some((0, false)),
        TokenKind::LBracket => Some((1, true)),
        TokenKind::RBracket => Some((1, false)),
        TokenKind::LBrace => Some((2, true)),
        TokenKind::RBrace => Some((2, false)),
        _ => None,
    }
}

/// Open brackets, innermost last, each with a value `T` of its caller.
pub(crate) struct Brackets<T> {
    items: Vec<(usize, T)>,
    count: [usize; 3],
    /// Entries below this length cannot be closed (the code outside a string
    /// while its interpolation is lexed).
    floor: usize,
    /// Per kind, the entries below `floor`.
    floor_count: [usize; 3],
}

impl<T: Copy> Brackets<T> {
    pub(crate) const fn new() -> Self {
        Brackets {
            items: Vec::new(),
            count: [0; 3],
            floor: 0,
            floor_count: [0; 3],
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The outermost open bracket.
    pub(crate) fn first(&self) -> Option<T> {
        self.items.first().map(|&(_, v)| v)
    }

    /// The bracket at depth `index`, outermost first.
    pub(crate) fn get(&self, index: usize) -> Option<T> {
        self.items.get(index).map(|&(_, v)| v)
    }

    pub(crate) fn push(&mut self, kind: usize, value: T) {
        if let Some(n) = self.count.get_mut(kind) {
            *n += 1;
            self.items.push((kind, value));
        }
    }

    fn pop(&mut self) -> Option<(usize, T)> {
        let (kind, value) = self.items.pop()?;
        if let Some(n) = self.count.get_mut(kind) {
            *n = n.saturating_sub(1);
        }
        Some((kind, value))
    }

    /// Closes the nearest open bracket of `kind` above the floor and every
    /// bracket above it. Returns `None` if there is none (the closer is
    /// stray and nothing changes), otherwise `Some` of the outermost bracket
    /// closed over without its own closer, if any.
    pub(crate) fn close(&mut self, kind: usize) -> Option<Option<T>> {
        let open = self.count.get(kind).copied().unwrap_or(0);
        let below = self.floor_count.get(kind).copied().unwrap_or(0);
        if open <= below {
            return None;
        }
        let mut over = None;
        while let Some((k, value)) = self.pop() {
            if k == kind {
                return Some(over);
            }
            over = Some(value);
        }
        Some(over)
    }

    /// Drops every bracket above the first `len`.
    pub(crate) fn truncate(&mut self, len: usize) {
        while self.items.len() > len {
            self.pop();
        }
    }

    /// Drops every bracket.
    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.count = [0; 3];
        self.floor = 0;
        self.floor_count = [0; 3];
    }

    /// Protects the brackets open now from closers until [`Self::unfloor`].
    pub(crate) fn floor(&mut self) {
        self.floor = self.items.len();
        self.floor_count = self.count;
    }

    /// Ends the protection of [`Self::floor`].
    pub(crate) fn unfloor(&mut self) {
        self.floor = 0;
        self.floor_count = [0; 3];
    }

    /// Takes all brackets, outermost first, and leaves the stack empty.
    pub(crate) fn take_kinds(&mut self) -> Vec<usize> {
        let kinds = self.items.iter().map(|&(k, _)| k).collect();
        self.clear();
        kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closer_closes_its_own_kind_and_everything_above() {
        let mut b = Brackets::new();
        b.push(0, 'a');
        b.push(1, 'b');
        b.push(2, 'c');
        // `)` closes the `(` and both brackets above it; the outermost of
        // those is reported.
        assert_eq!(b.close(0), Some(Some('b')));
        assert!(b.is_empty());
        // A stray closer changes nothing.
        b.push(0, 'd');
        assert_eq!(b.close(1), None);
        assert_eq!(b.len(), 1);
        assert_eq!(b.close(0), Some(None));
        assert!(b.is_empty());
    }

    #[test]
    fn the_floor_protects_outer_brackets() {
        let mut b = Brackets::new();
        b.push(0, 1);
        b.floor();
        assert_eq!(b.close(0), None);
        b.push(1, 2);
        b.push(0, 3);
        assert_eq!(b.close(0), Some(None));
        assert_eq!(b.close(0), None);
        assert_eq!(b.len(), 2);
        b.unfloor();
        b.truncate(1);
        assert_eq!(b.len(), 1);
        assert_eq!(b.close(0), Some(None));
    }

    #[test]
    fn many_closers_of_another_kind_are_linear() {
        let mut b = Brackets::new();
        for i in 0..100_000u32 {
            b.push(0, i);
        }
        for _ in 0..100_000 {
            assert_eq!(b.close(1), None);
        }
        assert_eq!(b.close(0), Some(None));
        assert_eq!(b.len(), 99_999);
    }
}
