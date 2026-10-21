//! Read scope of a subscription: the server's check of the filter and sort of every
//! `Subscribe` before it runs (ARCHITECTURE 5.4, 9, D101).
//!
//! A client chooses the filter and sort of its subscriptions. Without this check it could ask
//! "is there a row whose secret starts with `a`?" and learn a field it may not read, one bit per
//! subscription, or follow a reference into a row it may not see. [`scope_subscribe`] therefore
//! refuses a query that names a field the client may not read, and turns every accepted query
//! into the query the server actually runs:
//!
//! ```text
//! see(root) AND see(target of every reference hop) AND <client filter>
//! ```
//!
//! # Rules
//!
//! * Every field the query names must be client readable on the model it is read from: sort
//!   keys, the last field of every [`Path`], and the reference field of every [`Hop`]. A
//!   `server` field is never client readable. Otherwise [`ScopeError::NotReadable`].
//! * The field of a hop must be a reference to the model the hop names, otherwise
//!   [`ScopeError::WrongHop`]. The client's claim about the target model is never trusted.
//! * Each distinct hop prefix (`room`, `room.owner`) adds the `see` rule of the model it reaches
//!   as an `AND`, rewritten so that its paths start at the queried row. The queried model's own
//!   `see` rule is added as well, so the result is the complete read scope.
//! * A model without a `see` rule is readable by nobody (default deny, D8), so naming it is
//!   [`ScopeError::NoReadRule`].
//! * The client filter may have at most [`MAX_FILTER_NODES`] nodes, and so may the resulting
//!   filter, because a driver refuses a larger one ([`Query::check`]). Paths may have at most
//!   [`MAX_HOPS`] hops, also after a `see` rule was rewritten behind a hop.
//!
//! Each error is an [`ErrorCode`] for this subscription only ([`ScopeError::code`]); nothing of
//! the query has run. The errors depend only on the schema and the query, never on rows, so they
//! reveal nothing a client could not learn from the compiled program.
//!
//! # Why one `AND` at the top
//!
//! A hop whose target row is missing makes the whole filter false, also under `Not` and `Or`
//! (fail closed, filter semantics of [`Query`]). An invisible target row is
//! treated the same way: the `see` conjunct sits at the top level, so no `Not` or `Or` of the
//! client can turn "not visible" into "selected".

use ostrel_db::api::{Expr, Hop, MAX_FILTER_NODES, MAX_HOPS, Path, Query};

use crate::protocol::{ErrorCode, FieldId, ModelId};

/// What the server knows about the read rights of the current principal.
///
/// The server implements it from the compiled program (T4) for one session; tests build one by
/// hand.
pub trait ReadScope {
    /// Whether the client may read `field` of `model`. False for a `server` field and for a field
    /// the model does not have.
    fn readable(&self, model: ModelId, field: FieldId) -> bool;

    /// The model that the reference field `field` of `model` points to; `None` when it is not a
    /// reference field.
    fn ref_target(&self, model: ModelId, field: FieldId) -> Option<ModelId>;

    /// The `see` rule of `model` as a filter over rows of `model`, with `me` and `signed` already
    /// replaced by the constants of this session. `None` when the model has no `see` rule.
    fn see(&self, model: ModelId) -> Option<Expr>;
}

/// Why a `Subscribe` was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// The query names a field the client may not read.
    NotReadable { model: ModelId, field: FieldId },
    /// A hop names a field that is not a reference to the hop's model.
    WrongHop { model: ModelId, field: FieldId },
    /// The query reads a model that has no `see` rule.
    NoReadRule { model: ModelId },
    /// The client filter, or the filter with the `see` rules added, has more than
    /// [`MAX_FILTER_NODES`] nodes.
    TooLarge,
    /// A path has more than [`MAX_HOPS`] hops, also after a `see` rule was rewritten behind a
    /// hop.
    TooManyHops,
}

impl ScopeError {
    /// The code of the [`ServerMsg::Error`](crate::protocol::ServerMsg::Error) for the
    /// subscription.
    pub fn code(&self) -> ErrorCode {
        match self {
            ScopeError::TooLarge | ScopeError::TooManyHops => ErrorCode::FilterTooLarge,
            ScopeError::NotReadable { .. }
            | ScopeError::WrongHop { .. }
            | ScopeError::NoReadRule { .. } => ErrorCode::FilterNotReadable,
        }
    }
}

/// Checks the filter and sort of a subscription and returns the query the server runs: the
/// client query with the read scope added (module documentation).
pub fn scope_subscribe(query: Query, scope: &dyn ReadScope) -> Result<Query, ScopeError> {
    let root = query.model;
    for (field, _) in &query.order {
        require_readable(scope, root, *field)?;
    }

    let mut hop_prefixes: Vec<Vec<Hop>> = Vec::new();
    if let Some(filter) = &query.filter {
        for read in reads_of(filter)? {
            let target = follow_hops(scope, root, read.hops)?;
            if let Some(field) = read.field {
                require_readable(scope, target, field)?;
            }
            for len in 1..=read.hops.len() {
                let prefix = &read.hops[..len];
                if !hop_prefixes.iter().any(|known| known == prefix) {
                    hop_prefixes.push(prefix.to_vec());
                }
            }
        }
    }

    let mut conjuncts = vec![see_of(scope, root)?];
    for prefix in &hop_prefixes {
        let target = prefix.last().map_or(root, |hop| hop.model);
        conjuncts.push(behind_hops(see_of(scope, target)?, prefix)?);
    }

    let Query {
        model,
        filter,
        order,
        limit,
        after,
    } = query;
    conjuncts.extend(filter);
    let scoped = Expr::And(conjuncts);
    if count_nodes(&scoped) > MAX_FILTER_NODES {
        return Err(ScopeError::TooLarge);
    }
    Ok(Query {
        model,
        filter: Some(scoped),
        order,
        limit,
        after,
    })
}

/// One place where a filter reads data: the hops it follows and the field it reads at the end,
/// or no field for [`Expr::RowId`].
struct Read<'q> {
    hops: &'q [Hop],
    field: Option<FieldId>,
}

impl<'q> Read<'q> {
    fn of_path(path: &'q Path) -> Self {
        Read {
            hops: &path.hops,
            field: Some(path.field),
        }
    }
}

/// Every read of a client filter. The walk is iterative and stops after [`MAX_FILTER_NODES`]
/// nodes, because a hostile filter may be nested far deeper than the limit.
fn reads_of(filter: &Expr) -> Result<Vec<Read<'_>>, ScopeError> {
    let mut reads = Vec::new();
    let mut stack = vec![filter];
    let mut nodes = 0usize;
    while let Some(expr) = stack.pop() {
        nodes += 1;
        if nodes > MAX_FILTER_NODES {
            return Err(ScopeError::TooLarge);
        }
        match expr {
            Expr::Const(_) => {}
            Expr::Field(path) => reads.push(Read::of_path(path)),
            Expr::RowId(hops) => reads.push(Read { hops, field: None }),
            Expr::Cmp(_, a, b) => {
                stack.push(a);
                stack.push(b);
            }
            Expr::And(items) | Expr::Or(items) => stack.extend(items.iter()),
            Expr::Not(inner) | Expr::Coalesce(inner, _) => stack.push(inner),
            Expr::InSet {
                elem: inner,
                set: path,
            }
            | Expr::InMap {
                key: inner,
                map: path,
            }
            | Expr::MapGet {
                map: path,
                key: inner,
            } => {
                reads.push(Read::of_path(path));
                stack.push(inner);
            }
        }
    }
    Ok(reads)
}

/// Follows `hops` from `model` and returns the model reached. Each hop's field must be a client
/// readable reference to the hop's model.
fn follow_hops(scope: &dyn ReadScope, model: ModelId, hops: &[Hop]) -> Result<ModelId, ScopeError> {
    if hops.len() > MAX_HOPS {
        return Err(ScopeError::TooManyHops);
    }
    let mut current = model;
    for hop in hops {
        require_readable(scope, current, hop.field)?;
        if scope.ref_target(current, hop.field) != Some(hop.model) {
            return Err(ScopeError::WrongHop {
                model: current,
                field: hop.field,
            });
        }
        current = hop.model;
    }
    Ok(current)
}

fn require_readable(
    scope: &dyn ReadScope,
    model: ModelId,
    field: FieldId,
) -> Result<(), ScopeError> {
    if scope.readable(model, field) {
        Ok(())
    } else {
        Err(ScopeError::NotReadable { model, field })
    }
}

fn see_of(scope: &dyn ReadScope, model: ModelId) -> Result<Expr, ScopeError> {
    scope.see(model).ok_or(ScopeError::NoReadRule { model })
}

/// Rewrites a `see` rule of the model at the end of `prefix` so that it reads from the queried
/// row: every path and row id of the rule starts with `prefix`.
///
/// `see` rules come from the compiler and stay within the filter limits, so this recursion is
/// bounded; client filters are never passed here.
fn behind_hops(rule: Expr, prefix: &[Hop]) -> Result<Expr, ScopeError> {
    let path = |p: Path| -> Result<Path, ScopeError> {
        Ok(Path {
            hops: joined(prefix, p.hops)?,
            field: p.field,
        })
    };
    let boxed = |e: Box<Expr>| behind_hops(*e, prefix).map(Box::new);
    let all = |items: Vec<Expr>| -> Result<Vec<Expr>, ScopeError> {
        items.into_iter().map(|e| behind_hops(e, prefix)).collect()
    };
    Ok(match rule {
        Expr::Const(v) => Expr::Const(v),
        Expr::Field(p) => Expr::Field(path(p)?),
        Expr::RowId(hops) => Expr::RowId(joined(prefix, hops)?),
        Expr::Cmp(op, a, b) => Expr::Cmp(op, boxed(a)?, boxed(b)?),
        Expr::And(items) => Expr::And(all(items)?),
        Expr::Or(items) => Expr::Or(all(items)?),
        Expr::Not(inner) => Expr::Not(boxed(inner)?),
        Expr::InSet { elem, set } => Expr::InSet {
            elem: boxed(elem)?,
            set: path(set)?,
        },
        Expr::InMap { key, map } => Expr::InMap {
            key: boxed(key)?,
            map: path(map)?,
        },
        Expr::MapGet { map, key } => Expr::MapGet {
            map: path(map)?,
            key: boxed(key)?,
        },
        Expr::Coalesce(inner, fallback) => Expr::Coalesce(boxed(inner)?, fallback),
    })
}

fn joined(prefix: &[Hop], rest: Vec<Hop>) -> Result<Vec<Hop>, ScopeError> {
    if prefix.len() + rest.len() > MAX_HOPS {
        return Err(ScopeError::TooManyHops);
    }
    let mut hops = prefix.to_vec();
    hops.extend(rest);
    Ok(hops)
}

/// Number of nodes of a filter built by [`scope_subscribe`], counted as [`Query::check`] does.
/// The client part was already counted and bounded by [`reads_of`].
fn count_nodes(filter: &Expr) -> usize {
    let mut stack = vec![filter];
    let mut nodes = 0usize;
    while let Some(expr) = stack.pop() {
        nodes += 1;
        match expr {
            Expr::Const(_) | Expr::Field(_) | Expr::RowId(_) => {}
            Expr::Cmp(_, a, b) => {
                stack.push(a);
                stack.push(b);
            }
            Expr::And(items) | Expr::Or(items) => stack.extend(items.iter()),
            Expr::Not(inner) | Expr::Coalesce(inner, _) => stack.push(inner),
            Expr::InSet { elem: inner, .. }
            | Expr::InMap { key: inner, .. }
            | Expr::MapGet { key: inner, .. } => stack.push(inner),
        }
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use ostrel_core::ids::RowId;
    use ostrel_db::api::{CmpOp, Cursor, Dir, Value};

    // A chat schema: a message lives in a room, rooms have members and a server only secret,
    // users have a server only email. `Draft` has no `see` rule, so nobody may read it.
    const ROOM: ModelId = 0;
    const MESSAGE: ModelId = 1;
    const USER: ModelId = 2;
    const DRAFT: ModelId = 3;

    const ROOM_NAME: FieldId = 0;
    const ROOM_MEMBERS: FieldId = 1;
    const ROOM_SECRET: FieldId = 2;
    const ROOM_OWNER: FieldId = 3;

    const MESSAGE_ROOM: FieldId = 0;
    const MESSAGE_TEXT: FieldId = 1;
    const MESSAGE_DRAFT: FieldId = 2;

    const USER_HANDLE: FieldId = 0;
    const USER_EMAIL: FieldId = 1;

    struct Chat {
        me: Value,
        /// Replaces the `see` rule of `Room`, for the hop limit cases.
        room_see: Option<Expr>,
    }

    impl Chat {
        fn new() -> Self {
            Chat {
                me: Value::Text("me".into()),
                room_see: None,
            }
        }
    }

    impl ReadScope for Chat {
        fn readable(&self, model: ModelId, field: FieldId) -> bool {
            matches!(
                (model, field),
                (ROOM, ROOM_NAME | ROOM_MEMBERS | ROOM_OWNER)
                    | (MESSAGE, MESSAGE_ROOM | MESSAGE_TEXT | MESSAGE_DRAFT)
                    | (USER, USER_HANDLE)
            )
        }

        fn ref_target(&self, model: ModelId, field: FieldId) -> Option<ModelId> {
            match (model, field) {
                (MESSAGE, MESSAGE_ROOM) => Some(ROOM),
                (MESSAGE, MESSAGE_DRAFT) => Some(DRAFT),
                (ROOM, ROOM_OWNER) => Some(USER),
                _ => None,
            }
        }

        fn see(&self, model: ModelId) -> Option<Expr> {
            match model {
                ROOM => Some(
                    self.room_see
                        .clone()
                        .unwrap_or_else(|| self.member_of(vec![])),
                ),
                MESSAGE => Some(self.member_of(vec![hop(MESSAGE_ROOM, ROOM)])),
                USER => Some(Expr::Const(Value::Bool(true))),
                _ => None,
            }
        }
    }

    impl Chat {
        /// `me in <hops>.members`
        fn member_of(&self, hops: Vec<Hop>) -> Expr {
            Expr::InSet {
                elem: Box::new(Expr::Const(self.me.clone())),
                set: Path {
                    hops,
                    field: ROOM_MEMBERS,
                },
            }
        }
    }

    fn hop(field: FieldId, model: ModelId) -> Hop {
        Hop { field, model }
    }

    fn field(hops: Vec<Hop>, field: FieldId) -> Expr {
        Expr::Field(Path { hops, field })
    }

    fn text_eq(left: Expr, text: &str) -> Expr {
        Expr::Cmp(
            CmpOp::Eq,
            Box::new(left),
            Box::new(Expr::Const(Value::Text(text.into()))),
        )
    }

    fn query(model: ModelId, filter: Option<Expr>) -> Query {
        Query {
            filter,
            ..Query::all(model)
        }
    }

    #[test]
    fn filter_on_a_server_field_is_refused() {
        let q = query(ROOM, Some(text_eq(field(vec![], ROOM_SECRET), "a")));
        let err = scope_subscribe(q, &Chat::new()).unwrap_err();
        assert_eq!(
            err,
            ScopeError::NotReadable {
                model: ROOM,
                field: ROOM_SECRET
            }
        );
        assert_eq!(err.code(), ErrorCode::FilterNotReadable);
    }

    #[test]
    fn filter_on_a_server_field_behind_a_hop_is_refused() {
        let owner = vec![hop(ROOM_OWNER, USER)];
        let q = query(ROOM, Some(text_eq(field(owner, USER_EMAIL), "a@b")));
        assert_eq!(
            scope_subscribe(q, &Chat::new()),
            Err(ScopeError::NotReadable {
                model: USER,
                field: USER_EMAIL
            })
        );
    }

    #[test]
    fn server_field_inside_or_not_and_coalesce_is_found() {
        let secret = field(vec![], ROOM_SECRET);
        let hidden = Expr::Or(vec![
            Expr::Const(Value::Bool(false)),
            Expr::Not(Box::new(Expr::Coalesce(
                Box::new(secret),
                Value::Bool(false),
            ))),
        ]);
        let q = query(ROOM, Some(hidden));
        assert!(matches!(
            scope_subscribe(q, &Chat::new()),
            Err(ScopeError::NotReadable { .. })
        ));
    }

    #[test]
    fn sort_on_a_server_field_is_refused() {
        let q = Query {
            order: vec![(ROOM_NAME, Dir::Asc), (ROOM_SECRET, Dir::Desc)],
            ..Query::all(ROOM)
        };
        let err = scope_subscribe(q, &Chat::new()).unwrap_err();
        assert_eq!(
            err,
            ScopeError::NotReadable {
                model: ROOM,
                field: ROOM_SECRET
            }
        );
        assert_eq!(err.code(), ErrorCode::FilterNotReadable);
    }

    #[test]
    fn query_without_filter_gets_the_see_rule_of_its_model() {
        let q = Query {
            order: vec![(ROOM_NAME, Dir::Asc)],
            limit: Some(10),
            after: Some(Cursor {
                values: vec![Value::Text("m".into())],
                id: RowId::from_hex(&"0".repeat(32)).unwrap(),
            }),
            ..Query::all(ROOM)
        };
        let scoped = scope_subscribe(q.clone(), &Chat::new()).unwrap();
        let chat = Chat::new();
        assert_eq!(
            scoped,
            Query {
                filter: Some(Expr::And(vec![chat.member_of(vec![])])),
                ..q
            }
        );
    }

    #[test]
    fn every_hop_carries_the_see_rule_of_its_target() {
        // Messages whose room is called "general": the room must be visible, read from the
        // message through the same hop.
        let room = vec![hop(MESSAGE_ROOM, ROOM)];
        let client = text_eq(field(room.clone(), ROOM_NAME), "general");
        let q = query(MESSAGE, Some(client.clone()));
        let chat = Chat::new();
        let scoped = scope_subscribe(q, &chat).unwrap();
        let expected = Expr::And(vec![
            chat.see(MESSAGE).unwrap(),
            chat.member_of(room),
            client,
        ]);
        assert_eq!(scoped.filter, Some(expected));
    }

    #[test]
    fn two_hops_add_one_see_rule_per_prefix_and_none_twice() {
        let room = vec![hop(MESSAGE_ROOM, ROOM)];
        let owner = vec![hop(MESSAGE_ROOM, ROOM), hop(ROOM_OWNER, USER)];
        let client = Expr::And(vec![
            text_eq(field(owner.clone(), USER_HANDLE), "ada"),
            text_eq(field(room.clone(), ROOM_NAME), "general"),
            Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::RowId(owner.clone())),
                Box::new(Expr::Const(Value::Null)),
            ),
        ]);
        let q = query(MESSAGE, Some(client.clone()));
        let chat = Chat::new();
        let scoped = scope_subscribe(q, &chat).unwrap();
        let expected = Expr::And(vec![
            chat.see(MESSAGE).unwrap(),
            chat.member_of(room),
            Expr::Const(Value::Bool(true)),
            client,
        ]);
        assert_eq!(scoped.filter, Some(expected));
    }

    #[test]
    fn hop_into_a_model_without_see_rule_is_refused() {
        let draft = vec![hop(MESSAGE_DRAFT, DRAFT)];
        let q = query(MESSAGE, Some(Expr::RowId(draft)));
        let err = scope_subscribe(q, &Chat::new()).unwrap_err();
        assert_eq!(err, ScopeError::NoReadRule { model: DRAFT });
        assert_eq!(err.code(), ErrorCode::FilterNotReadable);
    }

    #[test]
    fn subscription_to_a_model_without_see_rule_is_refused() {
        assert_eq!(
            scope_subscribe(Query::all(DRAFT), &Chat::new()),
            Err(ScopeError::NoReadRule { model: DRAFT })
        );
    }

    #[test]
    fn hop_with_a_wrong_target_model_is_refused() {
        // The client claims that `Message.room` points to `User`.
        let lie = vec![hop(MESSAGE_ROOM, USER)];
        let q = query(MESSAGE, Some(text_eq(field(lie, USER_HANDLE), "ada")));
        assert_eq!(
            scope_subscribe(q, &Chat::new()),
            Err(ScopeError::WrongHop {
                model: MESSAGE,
                field: MESSAGE_ROOM
            })
        );
    }

    #[test]
    fn hop_over_a_field_that_is_no_reference_is_refused() {
        let text = vec![hop(MESSAGE_TEXT, ROOM)];
        let q = query(MESSAGE, Some(text_eq(field(text, ROOM_NAME), "x")));
        assert_eq!(
            scope_subscribe(q, &Chat::new()),
            Err(ScopeError::WrongHop {
                model: MESSAGE,
                field: MESSAGE_TEXT
            })
        );
    }

    #[test]
    fn hop_over_an_unreadable_reference_is_refused() {
        let mut chat = Chat::new();
        chat.room_see = Some(Expr::Const(Value::Bool(true)));
        // `Room.secret` is not a reference, but readability is checked first, so the answer
        // never tells whether a server field is a reference.
        let secret = vec![hop(ROOM_SECRET, USER)];
        let q = query(ROOM, Some(Expr::RowId(secret)));
        assert_eq!(
            scope_subscribe(q, &chat),
            Err(ScopeError::NotReadable {
                model: ROOM,
                field: ROOM_SECRET
            })
        );
    }

    #[test]
    fn see_rule_that_needs_too_many_hops_behind_a_hop_is_refused() {
        // `Room.see` reads one hop itself; behind two client hops that would be three.
        let mut chat = Chat::new();
        chat.room_see = Some(text_eq(
            field(vec![hop(ROOM_OWNER, USER)], USER_HANDLE),
            "ada",
        ));
        let q = query(
            MESSAGE,
            Some(text_eq(
                field(vec![hop(MESSAGE_ROOM, ROOM)], ROOM_NAME),
                "x",
            )),
        );
        assert!(scope_subscribe(q, &chat).is_ok(), "two hops in total fit");

        let mut chat = Chat::new();
        chat.room_see = Some(text_eq(
            field(
                vec![hop(ROOM_OWNER, USER), hop(ROOM_OWNER, USER)],
                USER_HANDLE,
            ),
            "ada",
        ));
        let q = query(
            MESSAGE,
            Some(text_eq(
                field(vec![hop(MESSAGE_ROOM, ROOM)], ROOM_NAME),
                "x",
            )),
        );
        let err = scope_subscribe(q, &chat).unwrap_err();
        assert_eq!(err, ScopeError::TooManyHops);
        assert_eq!(err.code(), ErrorCode::FilterTooLarge);
    }

    #[test]
    fn client_path_with_too_many_hops_is_refused() {
        let hops = vec![
            hop(MESSAGE_ROOM, ROOM),
            hop(ROOM_OWNER, USER),
            hop(USER_HANDLE, USER),
        ];
        let q = query(MESSAGE, Some(field(hops, USER_HANDLE)));
        assert_eq!(
            scope_subscribe(q, &Chat::new()),
            Err(ScopeError::TooManyHops)
        );
    }

    /// `And` of `n - 1` constants: a filter of exactly `n` nodes.
    fn filter_of_nodes(n: usize) -> Expr {
        Expr::And(vec![Expr::Const(Value::Bool(true)); n - 1])
    }

    #[test]
    fn client_filter_over_the_node_limit_is_refused() {
        let q = query(ROOM, Some(filter_of_nodes(MAX_FILTER_NODES + 1)));
        let err = scope_subscribe(q, &Chat::new()).unwrap_err();
        assert_eq!(err, ScopeError::TooLarge);
        assert_eq!(err.code(), ErrorCode::FilterTooLarge);
    }

    #[test]
    fn scoped_filter_over_the_node_limit_is_refused() {
        // `Room.see` is `me in members`: 2 nodes, plus the outer `And`.
        let fits = MAX_FILTER_NODES - 3;
        let q = query(ROOM, Some(filter_of_nodes(fits)));
        let scoped = scope_subscribe(q, &Chat::new()).unwrap();
        assert_eq!(count_nodes(&scoped.filter.unwrap()), MAX_FILTER_NODES);

        let q = query(ROOM, Some(filter_of_nodes(fits + 1)));
        assert_eq!(scope_subscribe(q, &Chat::new()), Err(ScopeError::TooLarge));
    }

    #[test]
    fn deeply_nested_client_filter_stops_at_the_node_limit() {
        let mut deep = Expr::Const(Value::Bool(true));
        for _ in 0..2_000 {
            deep = Expr::Not(Box::new(deep));
        }
        let q = query(ROOM, Some(deep));
        assert_eq!(scope_subscribe(q, &Chat::new()), Err(ScopeError::TooLarge));
    }

    #[test]
    fn scoped_query_passes_the_driver_check() {
        let room = vec![hop(MESSAGE_ROOM, ROOM)];
        let q = Query {
            order: vec![(MESSAGE_TEXT, Dir::Desc)],
            ..query(MESSAGE, Some(text_eq(field(room, ROOM_NAME), "general")))
        };
        let scoped = scope_subscribe(q, &Chat::new()).unwrap();
        assert_eq!(scoped.check(&[]), Ok(()));
    }
}
