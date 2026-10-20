//! Differential test of `none` in filters (AC-33 (b), (e), SYNTAX 4.4, D125).
//!
//! Rows with `none` in every optional field are written through a driver. Every generated
//! filter then runs through the driver and through [`reference`], which states the two valued
//! rules directly: `none == none` is true, `none == x` false, `!=` its negation, ordered
//! comparisons with `none` false, `x in c` with `x` none false, `not` acts on the two valued
//! result, and a hop that reaches no live row makes the whole filter false. Both must select
//! the same rows in the same order.
//!
//! The reference stands in for the VM, which does not evaluate filters over stored rows yet.
//!
//! `Set` elements and `Map` keys that are `none` are not generated: drivers still select such
//! a row for `x in c` with `x` none (T181), so T181 adds them here.
//!
//! A driver crate in `crates/<name>/` includes this module from one integration test:
//!
//! ```ignore
//! #[path = "../../../tests/db-conformance/none-differential/mod.rs"]
//! mod none_differential;
//!
//! #[test]
//! fn ac_33_see_none_differential_sqlite() {
//!     none_differential::run(&ostrel_db_sqlite::SqliteDriver::new(), "sqlite::memory:");
//! }
//! ```

use std::cmp::Ordering;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    CmpOp, CollectionChange, Driver, Expr, FieldId, Hlc, Hop, MapEntry, MigrationPlan, ModelId,
    OpId, Path, Query, ReplicaId, RowId, SchemaHash, SetChange, Value, Write, compare_key,
};

const ITEM: ModelId = 0;
const OWNER: ModelId = 1;
/// Fields of `ITEM`: `n: Int?`, `t: Text?`, `b: Bool?`, `tags: Set[Int]`,
/// `attrs: Map[Int, Int?]`, `owner: Owner?`.
const N: FieldId = 0;
const T: FieldId = 1;
const B: FieldId = 2;
const TAGS: FieldId = 3;
const ATTRS: FieldId = 4;
const OWNER_REF: FieldId = 5;
/// Field of `OWNER`: `n: Int?`.
const OWNER_N: FieldId = 0;

/// Owners: 1 with `n` none, 2 with `n` 1, 3 deleted. Id 4 never exists.
const LIVE_OWNERS: [(u128, Option<i64>); 2] = [(1, None), (2, Some(1))];
const DELETED_OWNER: u128 = 3;
const MISSING_OWNER: u128 = 4;

/// One generated `ITEM` row as written.
#[derive(Debug)]
struct Item {
    id: RowId,
    fields: Vec<(FieldId, Value)>,
    tags: Option<Vec<Value>>,
    attrs: Option<Vec<(Value, Value)>>,
}

/// Writes the rows, runs every filter through `driver` and through [`reference`], and fails
/// with every filter whose rows differ.
pub fn run(driver: &dyn Driver, url: &str) {
    let items = items();
    check_reference(&items);
    let failures = block_on(async {
        let mut conn = driver.connect(url).await.expect("connect");
        conn.migrate(&plan()).await.expect("migrate");
        let writes = writes(&items);
        let mut tx = conn.begin().await.expect("begin");
        tx.apply(&writes).await.expect("apply");
        tx.commit().await.expect("commit");
        let mut failures = Vec::new();
        for filter in filters() {
            let want: Vec<RowId> = items
                .iter()
                .filter(|i| reference(&filter, i))
                .map(|i| i.id)
                .collect();
            let q = Query {
                model: ITEM,
                filter: Some(filter.clone()),
                order: Vec::new(),
                limit: None,
                after: None,
            };
            let got: Vec<RowId> = match conn.query(&q).await {
                Ok(rows) => rows.0.iter().map(|r| r.id).collect(),
                Err(e) => {
                    failures.push(format!("{filter:?}: query failed: {e:?}"));
                    continue;
                }
            };
            if got != want {
                let row = items
                    .iter()
                    .find(|i| got.contains(&i.id) != want.contains(&i.id));
                failures.push(format!(
                    "{filter:?}: driver {} rows, reference {}; first differing row {row:?}",
                    got.len(),
                    want.len()
                ));
            }
        }
        failures
    });
    assert!(
        failures.is_empty(),
        "{} filters differ from the two valued reference (D125):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every combination of `none` and values in the optional fields of `ITEM`.
fn items() -> Vec<Item> {
    let ns = [Value::Null, int(0), int(1)];
    let ts = [Value::Null, Value::Text("a".to_string())];
    let bs = [Value::Null, Value::Bool(false), Value::Bool(true)];
    let tags = [None, Some(vec![int(1)])];
    let attrs = [
        None,
        Some(vec![(int(1), int(1))]),
        Some(vec![(int(0), Value::Null)]),
    ];
    let owners = [
        Value::Null,
        owner(1),
        owner(2),
        owner(DELETED_OWNER),
        owner(MISSING_OWNER),
    ];
    let mut out = Vec::new();
    for n in &ns {
        for t in &ts {
            for b in &bs {
                for tag in &tags {
                    for attr in &attrs {
                        for o in &owners {
                            out.push(Item {
                                id: rid(100 + out.len() as u128),
                                fields: vec![
                                    (N, n.clone()),
                                    (T, t.clone()),
                                    (B, b.clone()),
                                    (OWNER_REF, o.clone()),
                                ],
                                tags: tag.clone(),
                                attrs: attr.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

fn writes(items: &[Item]) -> Vec<Write> {
    let replica = ReplicaId(1);
    let hlc = Hlc::new(1, 0, replica).expect("stamp");
    let mut out: Vec<Write> = LIVE_OWNERS
        .iter()
        .chain([(DELETED_OWNER, None)].iter())
        .map(|&(id, n)| Write::Insert {
            model: OWNER,
            row: rid(id),
            fields: vec![(OWNER_N, n.map_or(Value::Null, int))],
            collections: Vec::new(),
        })
        .collect();
    out.push(Write::Delete {
        model: OWNER,
        row: rid(DELETED_OWNER),
        expect_version: 1,
    });
    for (seq, item) in (1..).zip(items) {
        let mut collections = Vec::new();
        if let Some(tags) = &item.tags {
            let tag = OpId { replica, seq };
            let adds = tags.iter().map(|e| SetChange::Add {
                elem: e.clone(),
                tag,
            });
            collections.push((TAGS, CollectionChange::Set(adds.collect())));
        }
        if let Some(attrs) = &item.attrs {
            let entries = attrs.iter().map(|(k, v)| MapEntry {
                key: k.clone(),
                value: Some(v.clone()),
                hlc,
            });
            collections.push((ATTRS, CollectionChange::Map(entries.collect())));
        }
        out.push(Write::Insert {
            model: ITEM,
            row: item.id,
            fields: item.fields.clone(),
            collections,
        });
    }
    out
}

/// Atoms over every optional field, alone, negated and combined in pairs.
fn filters() -> Vec<Expr> {
    let field = |f| {
        Expr::Field(Path {
            hops: Vec::new(),
            field: f,
        })
    };
    let owner_n = || {
        Expr::Field(Path {
            hops: vec![Hop {
                field: OWNER_REF,
                model: OWNER,
            }],
            field: OWNER_N,
        })
    };
    let cmp = |op, a, b| Expr::Cmp(op, Box::new(a), Box::new(b));
    let konst = Expr::Const;
    let local = |f| Path {
        hops: Vec::new(),
        field: f,
    };
    let atoms = vec![
        cmp(CmpOp::Eq, field(N), konst(Value::Null)),
        cmp(CmpOp::Ne, field(N), konst(Value::Null)),
        cmp(CmpOp::Eq, field(N), konst(int(1))),
        cmp(CmpOp::Ne, field(N), konst(int(1))),
        cmp(CmpOp::Lt, field(N), konst(int(1))),
        cmp(CmpOp::Ge, field(N), konst(int(0))),
        cmp(CmpOp::Eq, field(T), konst(Value::Text("a".to_string()))),
        cmp(CmpOp::Ne, field(T), konst(Value::Null)),
        field(B),
        cmp(CmpOp::Eq, field(B), konst(Value::Bool(false))),
        cmp(CmpOp::Eq, field(OWNER_REF), konst(Value::Null)),
        cmp(CmpOp::Eq, field(N), owner_n()),
        cmp(CmpOp::Eq, owner_n(), konst(Value::Null)),
        Expr::InSet {
            elem: Box::new(field(N)),
            set: local(TAGS),
        },
        Expr::InSet {
            elem: Box::new(konst(Value::Null)),
            set: local(TAGS),
        },
        Expr::InMap {
            key: Box::new(field(N)),
            map: local(ATTRS),
        },
        cmp(
            CmpOp::Eq,
            Expr::MapGet {
                map: local(ATTRS),
                key: Box::new(field(N)),
            },
            konst(Value::Null),
        ),
        cmp(
            CmpOp::Eq,
            Expr::Coalesce(Box::new(field(N)), int(0)),
            konst(int(0)),
        ),
    ];
    let not = |e: &Expr| Expr::Not(Box::new(e.clone()));
    let mut out = Vec::new();
    for (i, a) in atoms.iter().enumerate() {
        out.push(a.clone());
        out.push(not(a));
        for b in &atoms[i + 1..] {
            out.push(Expr::And(vec![a.clone(), b.clone()]));
            out.push(Expr::Or(vec![a.clone(), not(b)]));
            out.push(not(&Expr::Or(vec![a.clone(), b.clone()])));
        }
    }
    out
}

/// The hop of a path did not reach a live row: the whole filter is false (D125).
struct Unreached;

/// Whether the two valued semantics of SYNTAX 4.4 selects `item` for `filter`.
fn reference(filter: &Expr, item: &Item) -> bool {
    matches!(eval(filter, item), Ok(Value::Bool(true)))
}

fn eval(e: &Expr, item: &Item) -> Result<Value, Unreached> {
    let truth = |e: &Expr| Ok::<_, Unreached>(eval(e, item)? == Value::Bool(true));
    Ok(match e {
        Expr::Const(v) => v.clone(),
        Expr::Field(p) => read(p, item)?,
        Expr::RowId(hops) if hops.is_empty() => Value::Ref(item.id),
        Expr::RowId(_) => unreachable!("not generated"),
        Expr::Cmp(op, a, b) => Value::Bool(compare(*op, &eval(a, item)?, &eval(b, item)?)),
        // Every operand is evaluated: an unreached hop anywhere makes the filter false.
        Expr::And(items) => {
            let all = items.iter().map(truth).collect::<Result<Vec<_>, _>>()?;
            Value::Bool(all.iter().all(|t| *t))
        }
        Expr::Or(items) => {
            let all = items.iter().map(truth).collect::<Result<Vec<_>, _>>()?;
            Value::Bool(all.iter().any(|t| *t))
        }
        Expr::Not(a) => Value::Bool(!truth(a)?),
        Expr::InSet { elem, set } => {
            let x = eval(elem, item)?;
            local(set);
            let tags = item.tags.as_deref().unwrap_or_default();
            Value::Bool(x != Value::Null && tags.iter().any(|t| equal(t, &x)))
        }
        Expr::InMap { key, map } => {
            let x = eval(key, item)?;
            local(map);
            Value::Bool(x != Value::Null && entry(item, &x).is_some())
        }
        Expr::MapGet { map, key } => {
            let x = eval(key, item)?;
            local(map);
            entry(item, &x).cloned().unwrap_or(Value::Null)
        }
        Expr::Coalesce(a, c) => match eval(a, item)? {
            Value::Null => c.clone(),
            v => v,
        },
    })
}

fn local(p: &Path) {
    assert!(
        p.hops.is_empty(),
        "collections behind a hop are not generated"
    );
}

fn entry<'i>(item: &'i Item, key: &Value) -> Option<&'i Value> {
    let attrs = item.attrs.as_deref().unwrap_or_default();
    attrs.iter().find(|(k, _)| equal(k, key)).map(|(_, v)| v)
}

fn read(p: &Path, item: &Item) -> Result<Value, Unreached> {
    let own = |f: FieldId| {
        let found = item.fields.iter().find(|(g, _)| *g == f);
        found.map_or(Value::Null, |(_, v)| v.clone())
    };
    match p.hops.as_slice() {
        [] => Ok(own(p.field)),
        [hop] if hop.field == OWNER_REF && p.field == OWNER_N => {
            let Value::Ref(target) = own(OWNER_REF) else {
                return Err(Unreached);
            };
            let live = LIVE_OWNERS.iter().find(|(id, _)| rid(*id) == target);
            let (_, n) = live.ok_or(Unreached)?;
            Ok(n.map_or(Value::Null, int))
        }
        _ => unreachable!("not generated"),
    }
}

/// `==` is true for `none` on both sides and false for `none` on one; ordered comparisons are
/// false with `none` on either side.
fn compare(op: CmpOp, a: &Value, b: &Value) -> bool {
    let order = match (a, b) {
        (Value::Null, Value::Null) => None,
        (Value::Null, _) | (_, Value::Null) => return op == CmpOp::Ne,
        _ => Some(compare_key(a, b)),
    };
    match op {
        CmpOp::Eq => order.is_none_or(Ordering::is_eq),
        CmpOp::Ne => order.is_some_and(Ordering::is_ne),
        CmpOp::Lt => order.is_some_and(Ordering::is_lt),
        CmpOp::Le => order.is_some_and(Ordering::is_le),
        CmpOp::Gt => order.is_some_and(Ordering::is_gt),
        CmpOp::Ge => order.is_some_and(Ordering::is_ge),
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    compare(CmpOp::Eq, a, b)
}

/// Guards the reference against three valued logic: each probe selects rows that SQL `NULL`
/// semantics without `IS` and `COALESCE` would drop or keep the other way.
fn check_reference(items: &[Item]) {
    let n = |i: &Item| i.fields[0].1.clone();
    let count = |f: &Expr| items.iter().filter(|i| reference(f, i)).count();
    let none_rows = items.iter().filter(|i| n(i) == Value::Null).count();
    let field = |f| {
        Expr::Field(Path {
            hops: Vec::new(),
            field: f,
        })
    };
    let eq = |a, b| Expr::Cmp(CmpOp::Eq, Box::new(a), Box::new(b));
    let not = |e| Expr::Not(Box::new(e));
    assert!(none_rows > 0 && none_rows < items.len());
    assert_eq!(count(&eq(field(N), Expr::Const(Value::Null))), none_rows);
    let ne_one = items.iter().filter(|i| n(i) != int(1)).count();
    assert_eq!(count(&not(eq(field(N), Expr::Const(int(1))))), ne_one);
    let in_tags = Expr::InSet {
        elem: Box::new(field(N)),
        set: Path {
            hops: Vec::new(),
            field: TAGS,
        },
    };
    let not_in = items.iter().filter(|i| !reference(&in_tags, i)).count();
    assert_eq!(count(&not(in_tags)), not_in);
    assert!(not_in > none_rows);
    let owner_n = Expr::Field(Path {
        hops: vec![Hop {
            field: OWNER_REF,
            model: OWNER,
        }],
        field: OWNER_N,
    });
    let hop_null = eq(owner_n, Expr::Const(Value::Null));
    let reached = items
        .iter()
        .filter(|i| [owner(1), owner(2)].contains(&i.fields[3].1))
        .count();
    let both = count(&hop_null) + count(&not(hop_null.clone()));
    assert_eq!(both, reached, "an unreached hop selects under neither form");
}

fn plan() -> MigrationPlan {
    MigrationPlan {
        from: None,
        to: SchemaHash([0; 32]),
        schema: "{}".to_string(),
        enums: Vec::new(),
        steps: Vec::new(),
    }
}

fn int(v: i64) -> Value {
    Value::int(v).expect("safe int")
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).expect("row id")
}

fn owner(n: u128) -> Value {
    Value::Ref(rid(n))
}

/// Runs a future whose every await completes at once (in memory, SQLite).
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future waited; run on the driver's runtime"),
    }
}
