//! AC-56 query order on the SQLite driver (SPEC 12.10, ARCHITECTURE 6.2, `docs/db-mapping.md`
//! 6.4 and 6.5): rows with equal sort values are ordered by `id` in the direction of the sort,
//! so `sort f desc` is exactly the reverse of `sort f`; a query without `sort` is in `id` order;
//! a limit takes the first rows of that order, and `limit 0` gives no rows.
//!
//! The rows are inserted in an order that is neither their `id` order nor their sort order, so
//! a driver that returns insertion order, or breaks ties by an internal row number, fails.

use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_core::value::SafeInt;
use ostrel_db::api::{Connection, Dir, Driver, Query, RowId, Value, Write};
use ostrel_db_sqlite::SqliteDriver;

const POST: u32 = 0;
const POS: u32 = 0;

fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the SQLite driver is expected to finish without waiting"),
    }
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap_or_else(|e| panic!("row id {n}: {e:?}"))
}

/// Rows `(id, pos)`, inserted in this order. Ties: pos 1 for ids 2, 5, 7; pos 2 for ids 3, 6.
const ROWS: [(u128, i64); 7] = [(5, 1), (3, 2), (7, 1), (1, 3), (6, 2), (2, 1), (4, 4)];

fn connect() -> Box<dyn Connection> {
    block_on(async {
        let mut conn = SqliteDriver::new()
            .connect("sqlite::memory:")
            .await
            .unwrap_or_else(|e| panic!("connect: {e:?}"));
        let writes: Vec<Write> = ROWS
            .iter()
            .map(|&(id, pos)| Write::Insert {
                model: POST,
                row: rid(id),
                fields: vec![(
                    POS,
                    Value::Int(SafeInt::new(pos).unwrap_or_else(|_| panic!())),
                )],
                collections: vec![],
            })
            .collect();
        {
            let mut tx = conn
                .begin()
                .await
                .unwrap_or_else(|e| panic!("begin: {e:?}"));
            tx.apply(&writes)
                .await
                .unwrap_or_else(|e| panic!("insert: {e:?}"));
            tx.commit()
                .await
                .unwrap_or_else(|e| panic!("commit: {e:?}"));
        }
        conn
    })
}

/// The ids of the rows `query` returns, in order.
fn ids(conn: &mut dyn Connection, order: &[(u32, Dir)], limit: Option<u32>) -> Vec<RowId> {
    let mut q = Query::all(POST);
    q.order = order.to_vec();
    q.limit = limit;
    let rows = block_on(conn.query(&q)).unwrap_or_else(|e| panic!("query {q:?}: {e:?}"));
    rows.0.into_iter().map(|r| r.id).collect()
}

fn rids(ns: &[u128]) -> Vec<RowId> {
    ns.iter().map(|&n| rid(n)).collect()
}

#[test]
fn ac_56_tie_break_by_id_follows_the_sort_direction() {
    let mut conn = connect();
    let asc = ids(conn.as_mut(), &[(POS, Dir::Asc)], None);
    assert_eq!(asc, rids(&[2, 5, 7, 3, 6, 1, 4]), "sort pos");
    let desc = ids(conn.as_mut(), &[(POS, Dir::Desc)], None);
    assert_eq!(desc, rids(&[4, 1, 6, 3, 7, 5, 2]), "sort pos desc");
    let mut reversed = asc.clone();
    reversed.reverse();
    assert_eq!(
        desc, reversed,
        "sort pos desc is exactly the reverse of sort pos"
    );
}

#[test]
fn ac_56_desc_limit_is_the_last_rows_reversed() {
    let mut conn = connect();
    let asc = ids(conn.as_mut(), &[(POS, Dir::Asc)], None);
    for n in 0..=asc.len() + 1 {
        let limit = u32::try_from(n).unwrap_or(u32::MAX);
        let got = ids(conn.as_mut(), &[(POS, Dir::Desc)], Some(limit));
        // `sort pos last n` in sort order, then reversed.
        let mut want: Vec<RowId> = asc.iter().rev().take(n).cloned().collect();
        assert_eq!(got, want, "sort pos desc limit {n}");
        want.reverse();
        assert_eq!(
            want,
            asc[asc.len().saturating_sub(n)..],
            "sort pos last {n}"
        );
    }
}

#[test]
fn ac_56_query_without_sort_is_in_id_order() {
    let mut conn = connect();
    assert_eq!(ids(conn.as_mut(), &[], None), rids(&[1, 2, 3, 4, 5, 6, 7]));
    assert_eq!(ids(conn.as_mut(), &[], Some(2)), rids(&[1, 2]), "limit 2");
    assert_eq!(ids(conn.as_mut(), &[], Some(0)), rids(&[]), "limit 0");
    assert_eq!(
        ids(conn.as_mut(), &[(POS, Dir::Asc)], Some(0)),
        rids(&[]),
        "sort pos limit 0"
    );
}
