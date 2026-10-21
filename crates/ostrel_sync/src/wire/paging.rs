//! Splitting rows and server ops over frames that fit the frame limit (D121).
//!
//! The server never sends a frame over [`LIMITS`]`.frame_bytes`. A `snapshot` page closes at
//! [`LIMITS`]`.rows_per_snapshot` rows or before the row that would push its frame over the
//! limit; server `ops` are split the same way by bytes, keeping their order. Sizes are the
//! lengths of the canonical encodings, so every page encodes to a frame within the limit.

use super::frame::{encode_server, logged_op_text};
use super::row::encode_row;
use super::{Requests, WireError, WireSchema};
use crate::protocol::{CallId, FnId, LIMITS, LoggedOp, ModelId, Row, ServerMsg, ServerSeq, SubId};

/// The `snapshot` pages of subscription `sub` on `model` at log position `seq`, in the order of
/// `rows`. Every page carries `seq`; only the last one is marked `last`. No rows give one empty
/// last page.
pub fn snapshot_pages(
    sub: SubId,
    model: ModelId,
    rows: Vec<Row>,
    seq: ServerSeq,
    schema: &dyn WireSchema,
) -> Result<Vec<ServerMsg>, WireError> {
    let page = |rows, last| ServerMsg::Snapshot {
        sub,
        rows,
        seq,
        last,
    };
    // `"last":false` is the longer flag, so the envelope is measured with it.
    let envelope = encode_server(
        &page(Vec::new(), false),
        schema,
        &OneSubscription(sub, model),
    )?;
    let sized = rows
        .into_iter()
        .map(|row| {
            let bytes = encode_row(&row, model, schema)?.len();
            Ok((row, bytes))
        })
        .collect::<Result<Vec<_>, WireError>>()?;
    let pages = pack(sized, envelope.len(), LIMITS.rows_per_snapshot as usize)?;
    let count = pages.len();
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(at, rows)| page(rows, at + 1 == count))
        .collect())
}

/// The server `ops` messages for `ops`, as many per frame as fit, in the order of `ops`
/// (D121). No ops give no message.
pub fn ops_pages(ops: Vec<LoggedOp>, schema: &dyn WireSchema) -> Result<Vec<ServerMsg>, WireError> {
    if ops.is_empty() {
        return Ok(Vec::new());
    }
    let envelope = encode_server(&ServerMsg::Ops(Vec::new()), schema, &OneSubscription(0, 0))?;
    let sized = ops
        .into_iter()
        .map(|op| {
            let bytes = logged_op_text(&op, schema)?.len();
            Ok((op, bytes))
        })
        .collect::<Result<Vec<_>, WireError>>()?;
    let pages = pack(sized, envelope.len(), usize::MAX)?;
    Ok(pages.into_iter().map(ServerMsg::Ops).collect())
}

/// Splits sized items into pages of at most `max_items` whose frame, `envelope` bytes plus the
/// items and the commas between them, stays within the frame limit. Always at least one page.
/// An item that does not fit even alone is [`WireError::FrameTooLarge`].
fn pack<T>(
    items: Vec<(T, usize)>,
    envelope: usize,
    max_items: usize,
) -> Result<Vec<Vec<T>>, WireError> {
    let limit = LIMITS.frame_bytes as usize;
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut frame = envelope;
    for (item, bytes) in items {
        if envelope + bytes > limit {
            return Err(WireError::FrameTooLarge {
                bytes: envelope + bytes,
            });
        }
        let comma = usize::from(!page.is_empty());
        if page.len() == max_items || frame + comma + bytes > limit {
            pages.push(std::mem::take(&mut page));
            frame = envelope + bytes;
        } else {
            frame += comma + bytes;
        }
        page.push(item);
    }
    pages.push(page);
    Ok(pages)
}

/// [`Requests`] that knows a single subscription and no call, for measuring envelopes.
struct OneSubscription(SubId, ModelId);

impl Requests for OneSubscription {
    fn subscription_model(&self, sub: SubId) -> Option<ModelId> {
        (sub == self.0).then_some(self.1)
    }

    fn call_function(&self, _: CallId) -> Option<FnId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use ostrel_core::canon::encode_array;

    use super::*;

    #[test]
    fn pages_close_at_the_item_count_and_at_the_frame_limit() {
        let limit = LIMITS.frame_bytes as usize;
        // Three items of 10 bytes, at most two per page.
        let pages = pack(vec![(1, 10), (2, 10), (3, 10)], 20, 2).unwrap();
        assert_eq!(pages, [vec![1, 2], vec![3]]);
        // Two items fill the frame exactly with their comma; a third opens a new page.
        let half = (limit - 20 - 1) / 2;
        let exact = limit - 20 - 1 - half;
        let pages = pack(vec![(1, half), (2, exact), (3, 1)], 20, usize::MAX).unwrap();
        assert_eq!(pages, [vec![1, 2], vec![3]]);
        assert_eq!(pack(Vec::<((), usize)>::new(), 20, 2).unwrap().len(), 1);
        assert_eq!(
            pack(vec![((), limit - 19)], 20, 2),
            Err(WireError::FrameTooLarge { bytes: limit + 1 })
        );
        // The array brackets belong to the envelope: `[]` grows by the items and commas.
        assert_eq!(encode_array(&["a".into(), "b".into()]).len(), 2 + 1 + 1 + 1);
    }
}
