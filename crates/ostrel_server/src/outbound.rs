//! Frame limits of messages from server to client (D121, ARCHITECTURE 5.4 and 5.9).
//!
//! The 1 MiB frame limit holds in both directions: the server never sends a larger frame. This
//! module holds the rules that keep it so, independent of the wire codec:
//!
//! * a row whose canonical encoding would exceed [`ROW_BYTES`] after a merge is
//!   `Reject Limit` for the op that causes it ([`check_row`]);
//! * a `Snapshot` page closes at [`Limits::rows_per_snapshot`] rows or before the row that
//!   would push the frame over [`Limits::frame_bytes`] ([`snapshot_pages`]);
//! * the server splits its `Ops` over several messages so that each frame fits, keeping their
//!   order ([`ops_messages`]);
//! * a `Return` larger than one frame is sent as `CallError` `Limit` instead ([`fit_return`]).
//!
//! Sizes come from the caller as encoded byte lengths, so these rules do not depend on how a
//! message is encoded. A page of `n` items is measured as its envelope plus the item sizes plus
//! one separator byte between two items, which is the length of a JSON array in a frame. The
//! envelope is the length of the frame with an empty list in its longest form (for a snapshot
//! page, `"last":false`).
//!
//! [`Limits::rows_per_snapshot`]: ostrel_sync::protocol::Limits::rows_per_snapshot
//! [`Limits::frame_bytes`]: ostrel_sync::protocol::Limits::frame_bytes

use ostrel_sync::protocol::{
    CallError, CallId, LIMITS, LoggedOp, RejectReason, Row, ServerMsg, ServerSeq, SubId, Value,
};

/// Largest canonical encoding of one row as a `Snapshot` would carry it: all fields, `Set` and
/// `Map` contents and CRDT state (D121, 768 KiB). Kept here until the protocol limits carry
/// `row_bytes`.
pub const ROW_BYTES: usize = 786_432;

/// Bytes between two items of a page: the comma of a JSON array.
const SEPARATOR: usize = 1;

/// An item that does not fit a frame even alone. The server never sends it; for a row or an
/// op this is a server defect, because [`ROW_BYTES`] and the op limit keep each one below a
/// frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Oversized {
    /// Position of the item in the input.
    pub index: usize,
    /// Its encoded length.
    pub bytes: usize,
}

/// Decides the row size limit for the row an op would produce: `Reject Limit` when its
/// canonical encoding after the merge is over [`ROW_BYTES`] (ARCHITECTURE 5.5 step 6).
pub fn check_row(encoded_row_bytes: usize) -> Result<(), RejectReason> {
    if encoded_row_bytes > ROW_BYTES {
        Err(RejectReason::Limit)
    } else {
        Ok(())
    }
}

/// Splits `items` into pages in their order. A page closes when it holds `max_items` items, or
/// before the item that would make `envelope` plus item sizes plus separators exceed `budget`.
/// A `max_items` of zero counts as one. No items give no page.
pub fn pack<T>(
    items: Vec<T>,
    max_items: usize,
    budget: usize,
    envelope: usize,
    size: impl Fn(&T) -> usize,
) -> Result<Vec<Vec<T>>, Oversized> {
    let max_items = max_items.max(1);
    let mut pages = Vec::new();
    let mut page: Vec<T> = Vec::new();
    let mut used = envelope;
    for (index, item) in items.into_iter().enumerate() {
        let bytes = size(&item);
        if envelope.saturating_add(bytes) > budget {
            return Err(Oversized { index, bytes });
        }
        let full = page.len() == max_items;
        if !page.is_empty() && (full || used + SEPARATOR + bytes > budget) {
            pages.push(std::mem::take(&mut page));
            used = envelope;
        }
        if !page.is_empty() {
            used += SEPARATOR;
        }
        // `used` stays at most `budget` here, so the additions cannot overflow.
        used += bytes;
        page.push(item);
    }
    if !page.is_empty() {
        pages.push(page);
    }
    Ok(pages)
}

/// The `Snapshot` pages of one subscription at log position `seq`. Every page holds at most
/// [`Limits::rows_per_snapshot`](ostrel_sync::protocol::Limits::rows_per_snapshot) rows and
/// fits the frame limit; only the final page has `last` set. A snapshot without rows is one
/// final page without rows.
pub fn snapshot_pages(
    sub: SubId,
    rows: Vec<Row>,
    seq: ServerSeq,
    envelope: usize,
    row_bytes: impl Fn(&Row) -> usize,
) -> Result<Vec<ServerMsg>, Oversized> {
    let mut pages = pack(
        rows,
        LIMITS.rows_per_snapshot as usize,
        LIMITS.frame_bytes as usize,
        envelope,
        row_bytes,
    )?;
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    let count = pages.len();
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(i, rows)| ServerMsg::Snapshot {
            sub,
            rows,
            seq,
            last: i + 1 == count,
        })
        .collect())
}

/// The `Ops` messages that carry `ops` to one client, in their order, each within the frame
/// limit. No ops give no message.
pub fn ops_messages(
    ops: Vec<LoggedOp>,
    envelope: usize,
    op_bytes: impl Fn(&LoggedOp) -> usize,
) -> Result<Vec<ServerMsg>, Oversized> {
    let pages = pack(
        ops,
        usize::MAX,
        LIMITS.frame_bytes as usize,
        envelope,
        op_bytes,
    )?;
    Ok(pages.into_iter().map(ServerMsg::Ops).collect())
}

/// The `Return` of call `call`. When its frame would be larger than the frame limit, the result
/// is replaced by `CallError` `Limit` (D121); `frame_bytes` gives the encoded frame length of a
/// message.
pub fn fit_return(
    call: CallId,
    result: Result<Value, CallError>,
    frame_bytes: impl Fn(&ServerMsg) -> usize,
) -> ServerMsg {
    let msg = ServerMsg::Return { call, result };
    if frame_bytes(&msg) > LIMITS.frame_bytes as usize {
        ServerMsg::Return {
            call,
            result: Err(CallError::Limit),
        }
    } else {
        msg
    }
}
