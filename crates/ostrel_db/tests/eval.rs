//! Edge cases of the shared evaluation in `ostrel_db::eval` (D129) that the conformance cases
//! reach only through a driver: errors of the row lookup, refusal before any lookup, empty
//! input, limit zero, cursor ties and a collection field that changes kind.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ostrel_db::api::{
    CmpOp, CollectionChange, CollectionState, Cursor, DbError, Dir, EnumColumn, Expr, FieldId, Hlc,
    Hop, MapEntry, ModelId, OpId, Path, Query, ReplicaId, RowId, SetChange, Value, Write,
};
use ostrel_db::eval::{self, LiveRow, Lookup};

const M: ModelId = 1;
const OTHER: ModelId = 2;
const F: FieldId = 0;
const REF: FieldId = 1;

fn rid(n: u64) -> RowId {
    RowId::new(Hlc::new(n, 0, ReplicaId(1)).unwrap())
}

fn int(v: i64) -> Value {
    Value::int(v).unwrap()
}

fn row(model: ModelId, fields: Vec<(FieldId, Value)>) -> Rc<LiveRow> {
    Rc::new(LiveRow {
        model,
        version: 1,
        fields: fields.into_iter().collect(),
        collections: BTreeMap::new(),
    })
}

fn query(filter: Option<Expr>) -> Query {
    Query {
        model: M,
        filter,
        order: vec![],
        limit: None,
        after: None,
    }
}

fn field(hops: Vec<Hop>, field: FieldId) -> Expr {
    Expr::Field(Path { hops, field })
}

/// Rows of a map; counts lookups and can fail every one of them.
struct Table {
    rows: BTreeMap<RowId, Rc<LiveRow>>,
    calls: Cell<u32>,
    fail: bool,
}

impl Table {
    fn new(rows: Vec<(RowId, Rc<LiveRow>)>) -> Self {
        Table {
            rows: rows.into_iter().collect(),
            calls: Cell::new(0),
            fail: false,
        }
    }

    fn of(&self, model: ModelId) -> Vec<(RowId, Rc<LiveRow>)> {
        let rows = self.rows.iter().filter(|(_, r)| r.model == model);
        rows.map(|(id, r)| (*id, Rc::clone(r))).collect()
    }
}

impl Lookup for Table {
    fn live(&self, id: RowId) -> Result<Option<Rc<LiveRow>>, DbError> {
        self.calls.set(self.calls.get() + 1);
        if self.fail {
            return Err(DbError::Backend("lookup failed".into()));
        }
        Ok(self.rows.get(&id).cloned())
    }
}

fn ids(t: &Table, q: &Query) -> Result<Vec<RowId>, DbError> {
    Ok(eval::run(&[], t, q, t.of(q.model))?
        .0
        .into_iter()
        .map(|r| r.id)
        .collect())
}

#[test]
fn no_rows_give_no_rows_and_no_lookup() {
    let t = Table::new(vec![]);
    let hop = vec![Hop {
        field: REF,
        model: OTHER,
    }];
    let q = query(Some(Expr::Cmp(
        CmpOp::Eq,
        Box::new(field(hop, F)),
        Box::new(Expr::Const(int(1))),
    )));
    assert_eq!(ids(&t, &q), Ok(vec![]));
    assert_eq!(t.calls.get(), 0);
}

#[test]
fn an_invalid_query_is_refused_before_any_lookup() {
    let mut t = Table::new(vec![(rid(1), row(M, vec![(REF, Value::Ref(rid(2)))]))]);
    t.fail = true;
    let mut q = query(None);
    // A cursor needs one value per sort key.
    q.after = Some(Cursor {
        values: vec![int(1)],
        id: rid(1),
    });
    assert!(matches!(ids(&t, &q), Err(DbError::Invalid(_))));
    assert_eq!(t.calls.get(), 0);
}

#[test]
fn a_lookup_error_ends_the_query_with_that_error() {
    let mut t = Table::new(vec![
        (rid(1), row(M, vec![(REF, Value::Ref(rid(9)))])),
        (rid(9), row(OTHER, vec![(F, int(1))])),
    ]);
    t.fail = true;
    let hop = vec![Hop {
        field: REF,
        model: OTHER,
    }];
    let q = query(Some(Expr::Not(Box::new(Expr::Cmp(
        CmpOp::Eq,
        Box::new(field(hop, F)),
        Box::new(Expr::Const(int(2))),
    )))));
    assert_eq!(ids(&t, &q), Err(DbError::Backend("lookup failed".into())));
}

#[test]
fn a_hop_without_a_live_row_drops_the_row_also_under_not_and_or() {
    let t = Table::new(vec![
        // Points to a row that does not exist.
        (rid(1), row(M, vec![(REF, Value::Ref(rid(8)))])),
        // Points to a live row of the wrong model.
        (rid(2), row(M, vec![(REF, Value::Ref(rid(3)))])),
        (rid(3), row(M, vec![(F, int(1))])),
        // Reference never written.
        (rid(4), row(M, vec![])),
    ]);
    let hop = vec![Hop {
        field: REF,
        model: OTHER,
    }];
    let atom = Expr::Cmp(
        CmpOp::Eq,
        Box::new(field(hop, F)),
        Box::new(Expr::Const(int(1))),
    );
    let not = query(Some(Expr::Not(Box::new(atom.clone()))));
    assert_eq!(ids(&t, &not), Ok(vec![]));
    let or = query(Some(Expr::Or(vec![Expr::Const(Value::Bool(true)), atom])));
    assert_eq!(ids(&t, &or), Ok(vec![]));
}

#[test]
fn limit_zero_and_a_cursor_on_a_tie_follow_the_id_order() {
    let t = Table::new(vec![
        (rid(1), row(M, vec![(F, int(5))])),
        (rid(2), row(M, vec![(F, int(5))])),
        (rid(3), row(M, vec![(F, int(5))])),
    ]);
    let mut q = query(None);
    q.order = vec![(F, Dir::Desc)];
    q.limit = Some(0);
    assert_eq!(ids(&t, &q), Ok(vec![]));
    q.limit = None;
    // Equal keys: ids in the direction of the last key, strictly after the cursor.
    q.after = Some(Cursor {
        values: vec![int(5)],
        id: rid(3),
    });
    assert_eq!(ids(&t, &q), Ok(vec![rid(2), rid(1)]));
}

#[test]
fn check_enums_refuses_an_undeclared_variant_and_allows_null() {
    let enums = [EnumColumn {
        model: M,
        field: F,
        variants: vec!["low".into(), "high".into()],
    }];
    let write = |v: Value| Write::Insert {
        model: M,
        row: rid(1),
        fields: vec![(F, v)],
        collections: vec![],
    };
    assert!(eval::check_enums(&enums, &[write(Value::Null)]).is_ok());
    assert!(eval::check_enums(&enums, &[write(Value::Enum("high".into()))]).is_ok());
    assert!(matches!(
        eval::check_enums(&enums, &[write(Value::Enum("mid".into()))]),
        Err(DbError::Invalid(_))
    ));
    // The same field of another model is no enum column.
    let other = Write::Insert {
        model: OTHER,
        row: rid(2),
        fields: vec![(F, Value::Enum("mid".into()))],
        collections: vec![],
    };
    assert!(eval::check_enums(&enums, &[other]).is_ok());
}

#[test]
fn a_collection_field_that_changes_kind_starts_over() {
    let tag = OpId {
        replica: ReplicaId(1),
        seq: 1,
    };
    let mut c = BTreeMap::new();
    let add = |n| SetChange::Add { elem: int(n), tag };
    eval::apply_collection(&mut c, F, &CollectionChange::Set(vec![add(2), add(1)]));
    let Some(CollectionState::Set(tags)) = c.get(&F) else {
        panic!("set expected");
    };
    let elems: Vec<&Value> = tags.iter().map(|t| &t.elem).collect();
    assert_eq!(elems, vec![&int(1), &int(2)]);
    let entry = MapEntry {
        key: int(1),
        value: Some(int(7)),
        hlc: Hlc::new(1, 0, ReplicaId(1)).unwrap(),
    };
    eval::apply_collection(&mut c, F, &CollectionChange::Map(vec![entry.clone()]));
    assert_eq!(c.get(&F), Some(&CollectionState::Map(vec![entry])));
    // An empty change still fixes the kind.
    eval::apply_collection(&mut c, F, &CollectionChange::Set(vec![]));
    assert_eq!(c.get(&F), Some(&CollectionState::Set(vec![])));
}
