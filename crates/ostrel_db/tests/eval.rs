//! Edge cases of the shared evaluation in `ostrel_db::eval` (D129) that the conformance cases
//! reach only through a driver: errors of the row lookup, refusal before any lookup, empty
//! input, limit zero, cursor ties, a collection field that changes kind and updates applied to
//! a partly loaded row (D148).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ostrel_db::api::{
    CmpOp, CollectionChange, CollectionState, Cursor, DbError, Dir, EnumColumn, Expr, FieldId, Hlc,
    Hop, MapEntry, ModelId, OpId, Path, Query, ReplicaId, RowId, SetChange, Value, Write,
    compare_key,
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

#[test]
fn an_inserted_row_has_version_one_and_its_changes_applied() {
    let tag = OpId {
        replica: ReplicaId(1),
        seq: 1,
    };
    let add = |n| SetChange::Add { elem: int(n), tag };
    let set = CollectionChange::Set(vec![add(2), add(1)]);
    let r = LiveRow::inserted(M, &[(F, int(5))], &[(REF, set.clone())]);
    let mut expected = BTreeMap::new();
    eval::apply_collection(&mut expected, REF, &set);
    assert_eq!(
        r,
        LiveRow {
            model: M,
            version: 1,
            fields: BTreeMap::from([(F, int(5))]),
            collections: expected,
        }
    );
    // No fields and no collections: an empty row, still version 1.
    let empty = LiveRow::inserted(OTHER, &[], &[]);
    assert_eq!((empty.version, empty.fields.len()), (1, 0));
    assert!(empty.collections.is_empty());
}

#[test]
fn check_refuses_another_model_before_another_version() {
    let r = row(M, vec![]);
    assert!(r.check(M, 1).is_ok());
    assert!(matches!(r.check(M, 0), Err(DbError::VersionMismatch)));
    assert!(matches!(r.check(M, 2), Err(DbError::VersionMismatch)));
    assert!(matches!(
        r.check(M, u64::MAX),
        Err(DbError::VersionMismatch)
    ));
    // Wrong model and wrong version together: the row is not found in that model.
    assert!(matches!(r.check(OTHER, 1), Err(DbError::NotFound)));
    assert!(matches!(r.check(OTHER, 7), Err(DbError::NotFound)));
}

#[test]
fn update_raises_the_version_and_keeps_untouched_fields() {
    let mut r = LiveRow::inserted(M, &[(F, int(1)), (REF, int(2))], &[]);
    // An update without fields or collections still counts as a write.
    r.update(&[], &[]);
    assert_eq!(r.version, 2);
    r.update(&[(F, Value::Null)], &[]);
    assert_eq!(r.version, 3);
    assert_eq!(r.fields.get(&F), Some(&Value::Null));
    assert_eq!(r.fields.get(&REF), Some(&int(2)));
    // The same field twice in one update: the last value wins, as in the insert.
    r.update(&[(F, int(8)), (F, int(9))], &[]);
    assert_eq!(r.fields.get(&F), Some(&int(9)));
    let twice = LiveRow::inserted(M, &[(F, int(8)), (F, int(9))], &[]);
    assert_eq!(twice.fields.get(&F), Some(&int(9)));
}

/// Elements or keys a collection change names.
fn named(change: &CollectionChange) -> Vec<Value> {
    match change {
        CollectionChange::Set(cs) => cs
            .iter()
            .map(|c| match c {
                SetChange::Add { elem, .. } | SetChange::Remove { elem, .. } => elem.clone(),
            })
            .collect(),
        CollectionChange::Map(es) => es.iter().map(|e| e.key.clone()).collect(),
    }
}

/// The part of `state` under the elements or keys in `keys`, or (with `keep` false) the rest.
fn part(state: &CollectionState, keys: &[Value], keep: bool) -> CollectionState {
    let hit = |v: &Value| keys.iter().any(|k| compare_key(k, v).is_eq()) == keep;
    match state {
        CollectionState::Set(t) => {
            CollectionState::Set(t.iter().filter(|x| hit(&x.elem)).cloned().collect())
        }
        CollectionState::Map(e) => {
            CollectionState::Map(e.iter().filter(|x| hit(&x.key)).cloned().collect())
        }
    }
}

/// D148: an update applied to the version plus the named slice of each named collection gives
/// the same named entries as on the full row, and leaves every other entry as it was.
#[test]
fn eval_slice_equals_full() {
    let op = |replica, seq| OpId {
        replica: ReplicaId(replica),
        seq,
    };
    let add = |n, r, s| SetChange::Add {
        elem: int(n),
        tag: op(r, s),
    };
    let remove = |n, r, s| SetChange::Remove {
        elem: int(n),
        tag: op(r, s),
    };
    let put = |k, v: Option<i64>, t| MapEntry {
        key: int(k),
        value: v.map(int),
        hlc: Hlc::new(t, 0, ReplicaId(1)).unwrap(),
    };
    let (set, map) = (2, 3);
    // Elements 1 to 6 with tags of two replicas, map keys 1 to 6.
    let tags: Vec<SetChange> = (1..=6).flat_map(|n| [add(n, 1, 5), add(n, 2, 5)]).collect();
    let entries: Vec<MapEntry> = (1..=6).map(|k| put(k, Some(k), 5)).collect();
    let full = LiveRow::inserted(
        M,
        &[(F, int(1)), (REF, int(2))],
        &[
            (set, CollectionChange::Set(tags)),
            (map, CollectionChange::Map(entries)),
        ],
    );
    let cases: Vec<(&str, Vec<(FieldId, CollectionChange)>)> = vec![
        (
            "add",
            vec![(set, CollectionChange::Set(vec![add(9, 1, 6)]))],
        ),
        (
            "replay",
            vec![(set, CollectionChange::Set(vec![add(2, 1, 5)]))],
        ),
        (
            "older add",
            vec![(set, CollectionChange::Set(vec![add(3, 2, 4)]))],
        ),
        (
            "newer add",
            vec![(set, CollectionChange::Set(vec![add(3, 2, 7)]))],
        ),
        (
            "remove of a foreign tag",
            vec![(
                set,
                CollectionChange::Set(vec![remove(4, 1, 9), remove(5, 3, 5)]),
            )],
        ),
        (
            "remove",
            vec![(set, CollectionChange::Set(vec![remove(4, 1, 5)]))],
        ),
        (
            "map put and overwrite",
            vec![(
                map,
                CollectionChange::Map(vec![put(2, Some(20), 6), put(8, None, 6)]),
            )],
        ),
        (
            "kind change",
            vec![
                (set, CollectionChange::Map(vec![put(1, Some(1), 6)])),
                (map, CollectionChange::Set(vec![add(1, 1, 6)])),
            ],
        ),
        (
            "new field",
            vec![(7, CollectionChange::Set(vec![add(1, 1, 1)]))],
        ),
    ];
    for (name, changes) in cases {
        let mut whole = full.clone();
        whole.update(&[(F, int(9))], &changes);
        // What a driver loads: version and model, and per named field of the same kind only
        // the entries under the named elements or keys.
        let mut slice = LiveRow {
            fields: BTreeMap::new(),
            collections: BTreeMap::new(),
            ..full.clone()
        };
        let same_kind = |f: &FieldId, c: &CollectionChange| {
            matches!(
                (c, full.collections.get(f)),
                (CollectionChange::Set(_), Some(CollectionState::Set(_)))
                    | (CollectionChange::Map(_), Some(CollectionState::Map(_)))
            )
        };
        for (f, c) in &changes {
            if same_kind(f, c) {
                let state = part(&full.collections[f], &named(c), true);
                slice.collections.insert(*f, state);
            }
        }
        slice.update(&[(F, int(9))], &changes);
        assert_eq!(slice.version, whole.version, "{name}");
        assert_eq!(slice.fields.get(&F), whole.fields.get(&F), "{name}");
        for (f, c) in &changes {
            let after = &whole.collections[f];
            if same_kind(f, c) {
                // Named entries as on the full row, and the full row keeps all others.
                let keys = named(c);
                assert_eq!(slice.collections[f], part(after, &keys, true), "{name}");
                let before = &full.collections[f];
                assert_eq!(
                    part(after, &keys, false),
                    part(before, &keys, false),
                    "{name}"
                );
            } else {
                // New or changed kind: the driver writes the field whole.
                assert_eq!(slice.collections.get(f), Some(after), "{name}");
            }
        }
    }
}
