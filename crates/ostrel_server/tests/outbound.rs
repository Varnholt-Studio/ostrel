//! Frame limits of messages from server to client (D121, ARCHITECTURE 5.4 and 5.9): row size,
//! snapshot pages closed by rows or bytes, server ops split over messages, oversized returns.

use ostrel_server::outbound::{
    Oversized, ROW_BYTES, check_row, fit_return, ops_messages, pack, snapshot_pages,
};
use ostrel_sync::protocol::{
    CallError, Hlc, LIMITS, LoggedOp, Op, OpBody, OpId, RejectReason, ReplicaId, Row, RowId,
    ServerMsg, ServerSeq, Value,
};

/// Frame length of a page: envelope, item sizes and one separator between two items.
fn frame_len(envelope: usize, sizes: &[usize]) -> usize {
    envelope + sizes.iter().sum::<usize>() + sizes.len().saturating_sub(1)
}

#[test]
fn row_bytes_is_768_kib_and_fits_a_frame() {
    assert_eq!(ROW_BYTES, 786_432);
    assert!(LIMITS.op_bytes as usize <= ROW_BYTES);
    assert!(ROW_BYTES < LIMITS.frame_bytes as usize);
}

#[test]
fn a_row_at_the_limit_is_admitted_and_one_byte_more_is_reject_limit() {
    assert_eq!(check_row(0), Ok(()));
    assert_eq!(check_row(ROW_BYTES), Ok(()));
    assert_eq!(check_row(ROW_BYTES + 1), Err(RejectReason::Limit));
}

#[test]
fn pack_keeps_order_and_closes_a_page_at_the_item_limit() {
    let pages = pack((0..7).collect::<Vec<u32>>(), 3, 1_000, 10, |_| 1).unwrap();
    assert_eq!(pages, vec![vec![0, 1, 2], vec![3, 4, 5], vec![6]]);
}

#[test]
fn pack_closes_a_page_before_the_item_that_would_overflow_the_frame() {
    // Envelope 10, items of 30 bytes, budget 80: two items take 10 + 30 + 1 + 30 = 71, a third
    // would take 102.
    let sizes = [30usize, 30, 30, 30, 30];
    let pages = pack(sizes.to_vec(), 100, 80, 10, |s| *s).unwrap();
    assert_eq!(pages, vec![vec![30, 30], vec![30, 30], vec![30]]);
    for page in &pages {
        assert!(frame_len(10, page) <= 80);
    }
}

#[test]
fn pack_fills_a_page_exactly_to_the_budget() {
    // 10 + 34 + 1 + 35 = 80: exactly at the budget stays on one page, one byte more does not.
    let exact = pack(vec![34usize, 35], 100, 80, 10, |s| *s).unwrap();
    assert_eq!(exact, vec![vec![34, 35]]);
    let over = pack(vec![34usize, 36], 100, 80, 10, |s| *s).unwrap();
    assert_eq!(over, vec![vec![34], vec![36]]);
}

#[test]
fn pack_reports_an_item_that_cannot_fit_a_frame_alone() {
    let err = pack(vec![5usize, 71, 5], 100, 80, 10, |s| *s).unwrap_err();
    assert_eq!(
        err,
        Oversized {
            index: 1,
            bytes: 71
        }
    );
    // 70 alone fits: 10 + 70 = 80.
    assert!(pack(vec![70usize], 100, 80, 10, |s| *s).is_ok());
}

#[test]
fn pack_of_nothing_is_no_page() {
    let pages = pack(Vec::<u8>::new(), 3, 80, 10, |_| 1).unwrap();
    assert!(pages.is_empty());
}

#[test]
fn pack_treats_an_item_limit_of_zero_as_one() {
    let pages = pack(vec![1u8, 2], 0, 80, 10, |_| 1).unwrap();
    assert_eq!(pages, vec![vec![1], vec![2]]);
}

fn hlc(n: u64) -> Hlc {
    Hlc::new(n, 0, ReplicaId(1)).unwrap()
}

/// A row told apart by its version; ids differ too.
fn row(n: u64) -> Row {
    Row {
        id: RowId::new(hlc(n)),
        version: n,
        fields: Vec::new(),
        collections: Vec::new(),
    }
}

fn row_ids(msg: &ServerMsg) -> (Vec<u64>, bool) {
    match msg {
        ServerMsg::Snapshot { rows, last, .. } => (rows.iter().map(|r| r.version).collect(), *last),
        other => panic!("expected a snapshot page, got {}", other.name()),
    }
}

#[test]
fn snapshot_pages_close_at_rows_per_snapshot() {
    let count = LIMITS.rows_per_snapshot as u64 * 2 + 1;
    let rows: Vec<Row> = (0..count).map(row).collect();
    let pages = snapshot_pages(7, rows, ServerSeq(42), 64, |_| 10).unwrap();
    assert_eq!(pages.len(), 3);
    let mut seen = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let ServerMsg::Snapshot {
            sub,
            seq,
            rows,
            last,
        } = page
        else {
            panic!("expected a snapshot page");
        };
        assert_eq!(*sub, 7);
        assert_eq!(*seq, ServerSeq(42));
        assert!(rows.len() <= LIMITS.rows_per_snapshot as usize);
        assert_eq!(*last, i == pages.len() - 1);
        seen.extend(rows.iter().map(|r| r.version));
    }
    assert_eq!(seen, (0..count).collect::<Vec<_>>());
}

#[test]
fn snapshot_pages_close_by_bytes_before_the_row_limit() {
    // Rows of 300 KiB: a 1 MiB frame takes three of them, far below 1000 rows.
    let big = 300 << 10;
    let rows: Vec<Row> = (0..7).map(row).collect();
    let pages = snapshot_pages(1, rows, ServerSeq(1), 100, |_| big).unwrap();
    let ids: Vec<(Vec<u64>, bool)> = pages.iter().map(row_ids).collect();
    assert_eq!(
        ids,
        vec![
            (vec![0, 1, 2], false),
            (vec![3, 4, 5], false),
            (vec![6], true),
        ]
    );
    assert!(frame_len(100, &[big; 3]) <= LIMITS.frame_bytes as usize);
}

#[test]
fn a_row_at_row_bytes_always_fits_a_snapshot_page_alone() {
    let rows: Vec<Row> = (0..3).map(row).collect();
    let pages = snapshot_pages(1, rows, ServerSeq(1), 4_096, |_| ROW_BYTES).unwrap();
    assert_eq!(pages.len(), 3);
}

#[test]
fn an_empty_snapshot_is_one_last_page() {
    let pages = snapshot_pages(3, Vec::new(), ServerSeq(9), 64, |_| 10).unwrap();
    assert_eq!(
        pages,
        vec![ServerMsg::Snapshot {
            sub: 3,
            rows: Vec::new(),
            seq: ServerSeq(9),
            last: true,
        }]
    );
}

#[test]
fn a_snapshot_row_over_the_frame_is_reported_not_sent() {
    let rows: Vec<Row> = (0..2).map(row).collect();
    let frame = LIMITS.frame_bytes as usize;
    let err = snapshot_pages(1, rows, ServerSeq(1), 64, |r| {
        if r.version == 1 { frame } else { 10 }
    })
    .unwrap_err();
    assert_eq!(
        err,
        Oversized {
            index: 1,
            bytes: frame
        }
    );
}

fn logged(n: u64) -> LoggedOp {
    LoggedOp {
        seq: ServerSeq(n),
        op: Op {
            id: OpId {
                replica: ReplicaId(2),
                seq: u32::try_from(n).unwrap(),
            },
            hlc: hlc(n),
            model: 0,
            row: RowId::new(hlc(1)),
            body: OpBody::Drop,
        },
    }
}

#[test]
fn server_ops_are_split_over_messages_in_order() {
    // Ops of 200 KiB: five fit a 1 MiB frame, twelve need three messages.
    let size = 200 << 10;
    let ops: Vec<LoggedOp> = (1..=12).map(logged).collect();
    let msgs = ops_messages(ops, 50, |_| size).unwrap();
    let mut seqs = Vec::new();
    for msg in &msgs {
        let ServerMsg::Ops(batch) = msg else {
            panic!("expected an ops message, got {}", msg.name());
        };
        assert!(!batch.is_empty());
        assert!(frame_len(50, &vec![size; batch.len()]) <= LIMITS.frame_bytes as usize);
        seqs.extend(batch.iter().map(|o| o.seq.0));
    }
    assert_eq!(msgs.len(), 3);
    assert_eq!(seqs, (1..=12).collect::<Vec<u64>>());
}

#[test]
fn many_small_server_ops_stay_in_one_message() {
    let ops: Vec<LoggedOp> = (1..=5_000).map(logged).collect();
    let msgs = ops_messages(ops, 50, |_| 100).unwrap();
    assert_eq!(msgs.len(), 1);
}

#[test]
fn no_server_ops_is_no_message() {
    assert!(ops_messages(Vec::new(), 50, |_| 100).unwrap().is_empty());
}

#[test]
fn a_return_within_the_frame_is_sent_unchanged() {
    let frame = LIMITS.frame_bytes as usize;
    let msg = fit_return(4, Ok(Value::Bool(true)), |_| frame);
    assert_eq!(
        msg,
        ServerMsg::Return {
            call: 4,
            result: Ok(Value::Bool(true)),
        }
    );
}

#[test]
fn a_return_over_the_frame_becomes_call_error_limit() {
    let frame = LIMITS.frame_bytes as usize;
    let msg = fit_return(4, Ok(Value::Bool(true)), |m| match m {
        ServerMsg::Return { result: Ok(_), .. } => frame + 1,
        _ => 40,
    });
    assert_eq!(
        msg,
        ServerMsg::Return {
            call: 4,
            result: Err(CallError::Limit),
        }
    );
}

#[test]
fn an_error_return_is_measured_too() {
    let msg = fit_return(
        9,
        Err(CallError::Runtime {
            kind: "x".repeat(16),
        }),
        |m| match m {
            ServerMsg::Return {
                result: Err(CallError::Limit),
                ..
            } => 40,
            _ => usize::MAX,
        },
    );
    assert_eq!(
        msg,
        ServerMsg::Return {
            call: 9,
            result: Err(CallError::Limit),
        }
    );
}
