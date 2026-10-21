//! The messages that carry queries, rows, server ops and call values (ARCHITECTURE 5.4 "Wire
//! form", D132, D135): a round trip per message, the faults that close the connection and the
//! ones that only answer one request, and the frame limits of D121.

use std::collections::BTreeMap;

use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::Value;
use ostrel_db::api::{
    CmpOp, CollectionState, Cursor, Dir, Expr, Hop, MapEntry, Path, Query, SetTag,
};
use ostrel_sync::protocol::{
    CallError, CallId, ClientMsg, ErrorCode, FieldId, FnId, LIMITS, LoggedOp, ModelId, Op, OpBody,
    OpFault, ProtocolError, Row, ServerMsg, SubId, ValueFault,
};
use ostrel_sync::wire::{
    QueryFault, Requests, WireError, WireSchema, WireTy, decode_client, decode_server,
    encode_client, encode_op, encode_server, ops_pages, row_bytes, snapshot_pages,
};

const FRAME: usize = LIMITS.frame_bytes as usize;

// -------------------------------------------------------------------------------------------
// Schema: `Room` and `Person`, and two server functions

const ROOM: ModelId = 0;
const PERSON: ModelId = 1;

/// Fields per model; the position is the field id.
fn fields(model: ModelId) -> &'static [(&'static str, WireTy)] {
    use std::sync::OnceLock;
    static FIELDS: OnceLock<[Vec<(&'static str, WireTy)>; 2]> = OnceLock::new();
    let all = FIELDS.get_or_init(|| {
        use WireTy::*;
        let level = Enum(vec!["member".into(), "admin".into()]);
        [
            vec![
                ("name", Text),
                ("topic", Optional(Box::new(Text))),
                ("owner", Ref),
                ("members", Set(Box::new(Ref))),
                ("roles", Map(Box::new(Ref), Box::new(level))),
                ("made", Time),
                ("size", Int),
                ("keys", List(Box::new(Int))),
            ],
            vec![
                ("handle", Text),
                ("home", Optional(Box::new(Ref))),
                ("age", Int),
            ],
        ]
    });
    all.get(model as usize).map_or(&[], Vec::as_slice)
}

/// Functions: `rename(room: Room, name: Text) -> Bool` and `count() -> Int`.
fn functions() -> &'static [(&'static str, Vec<WireTy>, WireTy)] {
    use std::sync::OnceLock;
    static FUNCTIONS: OnceLock<Vec<(&'static str, Vec<WireTy>, WireTy)>> = OnceLock::new();
    FUNCTIONS.get_or_init(|| {
        vec![
            ("rename", vec![WireTy::Ref, WireTy::Text], WireTy::Bool),
            ("count", vec![], WireTy::Int),
        ]
    })
}

struct Schema;

const MODELS: [&str; 2] = ["Room", "Person"];

impl WireSchema for Schema {
    fn model_id(&self, name: &str) -> Option<ModelId> {
        MODELS
            .iter()
            .position(|m| *m == name)
            .map(|at| at as ModelId)
    }

    fn model_name(&self, model: ModelId) -> Option<&str> {
        MODELS.get(model as usize).copied()
    }

    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
        let at = fields(model).iter().position(|(n, _)| *n == name)?;
        Some(at as FieldId)
    }

    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
        fields(model).get(field as usize).map(|(name, _)| *name)
    }

    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
        fields(model).get(field as usize).map(|(_, ty)| ty)
    }

    fn function_id(&self, name: &str) -> Option<FnId> {
        let at = functions().iter().position(|(n, ..)| *n == name)?;
        Some(at as FnId)
    }

    fn function_name(&self, function: FnId) -> Option<&str> {
        functions().get(function as usize).map(|(name, ..)| *name)
    }

    fn function_params(&self, function: FnId) -> Option<&[WireTy]> {
        functions()
            .get(function as usize)
            .map(|(_, params, _)| params.as_slice())
    }

    fn function_result(&self, function: FnId) -> Option<&WireTy> {
        functions()
            .get(function as usize)
            .map(|(.., result)| result)
    }
}

const RENAME: FnId = 0;
const COUNT: FnId = 1;

/// Open requests of one side: subscriptions with their model, calls with their function.
#[derive(Default)]
struct Open {
    subs: BTreeMap<SubId, ModelId>,
    calls: BTreeMap<CallId, FnId>,
}

impl Requests for Open {
    fn subscription_model(&self, sub: SubId) -> Option<ModelId> {
        self.subs.get(&sub).copied()
    }

    fn call_function(&self, call: CallId) -> Option<FnId> {
        self.calls.get(&call).copied()
    }
}

fn open() -> Open {
    Open {
        subs: BTreeMap::from([(1, ROOM), (2, PERSON)]),
        calls: BTreeMap::from([(7, RENAME), (8, COUNT)]),
    }
}

fn field(model: ModelId, name: &str) -> FieldId {
    Schema.field_id(model, name).unwrap()
}

fn replica() -> ReplicaId {
    ReplicaId(0x0123_4567_89ab_cdef)
}

fn stamp(wall_ms: u64) -> Hlc {
    Hlc::new(wall_ms, 3, replica()).unwrap()
}

fn row(wall_ms: u64) -> RowId {
    RowId::new(stamp(wall_ms))
}

fn op_id(seq: u32) -> OpId {
    OpId {
        replica: replica(),
        seq,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn int(v: i64) -> Value {
    Value::int(v).unwrap()
}

fn client(text: &str) -> Result<ClientMsg, WireError> {
    decode_client(text.as_bytes(), &Schema)
}

fn server(text: &str) -> Result<ServerMsg, WireError> {
    decode_server(text.as_bytes(), &Schema, &open())
}

fn client_round_trip(msg: ClientMsg) -> String {
    let frame = encode_client(&msg, &Schema).unwrap();
    assert_eq!(client(&frame), Ok(msg), "{frame}");
    frame
}

fn server_round_trip(msg: ServerMsg) -> String {
    let frame = encode_server(&msg, &Schema, &open()).unwrap();
    assert_eq!(server(&frame), Ok(msg), "{frame}");
    frame
}

fn path(field: FieldId) -> Path {
    Path {
        hops: Vec::new(),
        field,
    }
}

// -------------------------------------------------------------------------------------------
// subscribe

/// A query that uses every expression kind, a hop, every clause and a cursor.
fn rich_query() -> Query {
    let f = |name| field(ROOM, name);
    let owner_hop = Hop {
        field: f("owner"),
        model: PERSON,
    };
    let filter = Expr::And(vec![
        Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::Field(path(f("name")))),
            Box::new(Expr::Const(text("general"))),
        ),
        Expr::Or(vec![
            Expr::Cmp(
                CmpOp::Ge,
                Box::new(Expr::Const(int(18))),
                Box::new(Expr::Field(Path {
                    hops: vec![owner_hop],
                    field: field(PERSON, "age"),
                })),
            ),
            Expr::Not(Box::new(Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::RowId(vec![owner_hop])),
                Box::new(Expr::Const(Value::Ref(row(5)))),
            ))),
        ]),
        Expr::InSet {
            elem: Box::new(Expr::Const(Value::Ref(row(6)))),
            set: path(f("members")),
        },
        Expr::InMap {
            key: Box::new(Expr::RowId(Vec::new())),
            map: path(f("roles")),
        },
        Expr::Cmp(
            CmpOp::Lt,
            Box::new(Expr::MapGet {
                map: path(f("roles")),
                key: Box::new(Expr::Const(Value::Ref(row(6)))),
            }),
            Box::new(Expr::Const(Value::Enum("admin".into()))),
        ),
        Expr::Cmp(
            CmpOp::Le,
            Box::new(Expr::Coalesce(
                Box::new(Expr::Field(path(f("topic")))),
                text(""),
            )),
            Box::new(Expr::Const(Value::Null)),
        ),
        Expr::Cmp(
            CmpOp::Gt,
            Box::new(Expr::Field(path(f("made")))),
            Box::new(Expr::Const(Value::time(1_700_000_000_000).unwrap())),
        ),
        Expr::Const(Value::Bool(true)),
    ]);
    Query {
        model: ROOM,
        filter: Some(filter),
        order: vec![(f("made"), Dir::Desc), (f("name"), Dir::Asc)],
        limit: Some(50),
        after: Some(Cursor {
            values: vec![Value::time(1_700_000_000_000).unwrap(), Value::Null],
            id: row(9),
        }),
    }
}

#[test]
fn subscribe_round_trips_with_every_expression_kind() {
    client_round_trip(ClientMsg::Subscribe {
        sub: 1,
        query: rich_query(),
    });
    client_round_trip(ClientMsg::Subscribe {
        sub: u32::MAX,
        query: Query::all(PERSON),
    });
}

#[test]
fn subscribe_travels_with_names_in_canonical_form() {
    let query = Query {
        model: PERSON,
        filter: Some(Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::Field(Path {
                hops: vec![Hop {
                    field: field(PERSON, "home"),
                    model: ROOM,
                }],
                field: field(ROOM, "name"),
            })),
            Box::new(Expr::Const(text("x"))),
        )),
        order: vec![(field(PERSON, "age"), Dir::Asc)],
        limit: None,
        after: None,
    };
    let frame = client_round_trip(ClientMsg::Subscribe { sub: 3, query });
    assert_eq!(
        frame,
        r#"{"query":{"after":null,"filter":{"cmp":["eq",{"field":{"field":"name","hops":[["home","Room"]]}},{"const":"x"}]},"limit":null,"model":"Person","order":[["age","asc"]]},"sub":3,"t":"subscribe","v":0}"#
    );
}

/// A `subscribe` frame for subscription 4 with `query` as its query text.
fn subscribe(query: &str) -> String {
    format!(r#"{{"query":{query},"sub":4,"t":"subscribe","v":0}}"#)
}

/// A query on `Room` with `filter` as its filter text.
fn room_filter(filter: &str) -> String {
    subscribe(&format!(
        r#"{{"after":null,"filter":{filter},"limit":null,"model":"Room","order":[]}}"#
    ))
}

fn refused(fault: QueryFault) -> Result<ClientMsg, WireError> {
    Err(WireError::InvalidQuery { sub: 4, fault })
}

#[test]
fn a_query_that_does_not_fit_answers_only_its_subscription() {
    let name = r#"{"field":{"field":"name","hops":[]}}"#;
    let cases = [
        (
            subscribe(r#"{"after":null,"filter":null,"limit":null,"model":"Gone","order":[]}"#),
            QueryFault::UnknownModel("Gone".into()),
        ),
        (
            room_filter(r#"{"cmp":["eq",{"field":{"field":"gone","hops":[]}},{"const":1}]}"#),
            QueryFault::UnknownField("gone".into()),
        ),
        (
            room_filter(&format!(r#"{{"cmp":["eq",{name},{{"const":1}}]}}"#)),
            QueryFault::Value(ValueFault::WrongType),
        ),
        (
            room_filter(r#"{"cmp":["eq",{"const":1},{"const":1}]}"#),
            QueryFault::UntypedConstant,
        ),
        (
            room_filter(r#"{"coalesce":[{"const":1},{"const":1}]}"#),
            QueryFault::UntypedConstant,
        ),
        (
            room_filter(r#"{"cmp":["eq",{"field":{"field":"keys","hops":[]}},{"const":[1]}]}"#),
            QueryFault::CollectionConstant,
        ),
        (
            room_filter(r#"{"in_set":[{"const":"x"},{"field":"name","hops":[]}]}"#),
            QueryFault::WrongField("name".into()),
        ),
        (
            room_filter(r#"{"map_get":[{"field":"members","hops":[]},{"const":"x"}]}"#),
            QueryFault::WrongField("members".into()),
        ),
        (
            room_filter(
                r#"{"cmp":["eq",{"field":{"field":"age","hops":[["name","Person"]]}},{"const":1}]}"#,
            ),
            QueryFault::WrongField("name".into()),
        ),
        (
            room_filter(r#"{"row_id":[["owner","Person"],["home","Room"],["owner","Person"]]}"#),
            QueryFault::TooManyHops,
        ),
        (
            room_filter(r#"{"row_id":[["owner","Gone"]]}"#),
            QueryFault::UnknownModel("Gone".into()),
        ),
        (
            subscribe(
                r#"{"after":{"id":"00000000000000000000000000000001","values":[]},"filter":null,"limit":null,"model":"Room","order":[["name","asc"]]}"#,
            ),
            QueryFault::CursorLength,
        ),
        (
            room_filter(r#"{"xor":[]}"#),
            WireError::WrongType("filter").into(),
        ),
        (
            room_filter(r#"{"cmp":["eq",{"const":1}]}"#),
            WireError::WrongType("cmp").into(),
        ),
        (
            room_filter(r#"{"cmp":["like",{"const":1},{"const":1}]}"#),
            WireError::WrongType("cmp").into(),
        ),
        (subscribe("[]"), WireError::WrongType("query").into()),
        (
            subscribe(r#"{"after":null,"filter":null,"limit":-1,"model":"Room","order":[]}"#),
            WireError::FieldRange("limit").into(),
        ),
        (
            subscribe(r#"{"filter":null,"limit":null,"model":"Room","order":[]}"#),
            WireError::MissingKey("after").into(),
        ),
        (
            subscribe(
                r#"{"after":null,"filter":null,"limit":null,"model":"Room","order":[["name","up"]]}"#,
            ),
            WireError::WrongType("order").into(),
        ),
    ];
    for (frame, fault) in cases {
        let got = client(&frame);
        assert_eq!(got, refused(fault), "{frame}");
        let error = got.unwrap_err();
        assert!(!error.closes_connection());
        assert_eq!(
            error.reply(),
            ServerMsg::Error(ProtocolError {
                sub: Some(4),
                code: ErrorCode::Protocol
            })
        );
    }
}

/// A filter of `nodes` expression nodes: an `and` of true constants.
fn filter_of(nodes: usize) -> String {
    let operands = vec![r#"{"const":true}"#; nodes - 1].join(",");
    room_filter(&format!(r#"{{"and":[{operands}]}}"#))
}

#[test]
fn a_filter_over_the_node_limit_is_filter_too_large() {
    let limit = LIMITS.filter_nodes as usize;
    assert!(matches!(
        client(&filter_of(limit)),
        Ok(ClientMsg::Subscribe { sub: 4, .. })
    ));
    let error = client(&filter_of(limit + 1)).unwrap_err();
    assert_eq!(
        error,
        WireError::InvalidQuery {
            sub: 4,
            fault: QueryFault::TooManyNodes
        }
    );
    assert_eq!(error.code(), ErrorCode::FilterTooLarge);
    assert_eq!(
        error.reply(),
        ServerMsg::Error(ProtocolError {
            sub: Some(4),
            code: ErrorCode::FilterTooLarge
        })
    );
}

#[test]
fn a_coalesce_fallback_is_no_expression_node() {
    // As `Query::check` counts: the fallback of a `coalesce` is a value, not a node.
    let limit = LIMITS.filter_nodes as usize;
    let coalesce = r#"{"coalesce":[{"field":{"field":"topic","hops":[]}},{"const":""}]}"#;
    // `and`, limit - 5 constants, `cmp`, `coalesce`, `field` and the compared constant make
    // exactly the limit; counting the fallback as well would make one node too many.
    let mut operands = vec![r#"{"const":true}"#.to_owned(); limit - 5];
    operands.push(format!(r#"{{"cmp":["eq",{coalesce},{{"const":"x"}}]}}"#));
    let frame = room_filter(&format!(r#"{{"and":[{}]}}"#, operands.join(",")));
    let Ok(ClientMsg::Subscribe { query, .. }) = client(&frame) else {
        panic!("{frame}");
    };
    assert_eq!(query.check(&[]), Ok(()));
}

#[test]
fn a_subscribe_without_a_readable_id_is_fatal() {
    for frame in [
        r#"{"query":{},"sub":-1,"t":"subscribe","v":0}"#,
        r#"{"query":{},"t":"subscribe","v":0}"#,
    ] {
        let error = client(frame).unwrap_err();
        assert!(error.closes_connection(), "{frame}");
    }
    // An unknown key of the frame is fatal even when the query is fine.
    let extra = filter_of(1).replace(r#""sub":4"#, r#""sub":4,"x":1"#);
    assert_eq!(client(&extra), Err(WireError::UnknownKey("x".into())));
}

#[test]
fn query_ids_unknown_to_the_schema_are_not_written() {
    let mut query = Query::all(9);
    assert_eq!(
        encode_client(
            &ClientMsg::Subscribe {
                sub: 1,
                query: query.clone()
            },
            &Schema
        ),
        Err(WireError::NotInSchema("model"))
    );
    query.model = ROOM;
    query.order = vec![(99, Dir::Asc)];
    assert_eq!(
        encode_client(&ClientMsg::Subscribe { sub: 1, query }, &Schema),
        Err(WireError::NotInSchema("field"))
    );
}

// -------------------------------------------------------------------------------------------
// call and return

#[test]
fn call_round_trips_with_typed_arguments() {
    let frame = client_round_trip(ClientMsg::Call {
        call: 7,
        function: RENAME,
        args: vec![Value::Ref(row(1)), text("lobby")],
    });
    assert_eq!(
        frame,
        format!(
            r#"{{"args":["{}","lobby"],"call":7,"function":"rename","t":"call","v":0}}"#,
            row(1).to_hex()
        )
    );
    client_round_trip(ClientMsg::Call {
        call: 0,
        function: COUNT,
        args: Vec::new(),
    });
}

/// A `call` frame for call 9 with `function` and `args` as their texts.
fn call(function: &str, args: &str) -> String {
    format!(r#"{{"args":{args},"call":9,"function":{function},"t":"call","v":0}}"#)
}

#[test]
fn a_call_that_does_not_fit_is_answered_by_its_return() {
    let room = format!("\"{}\"", row(1).to_hex());
    let cases = [
        (call(r#""gone""#, "[]"), CallError::NotFound),
        (call(r#""rename""#, "[]"), CallError::Invalid),
        (
            call(r#""rename""#, &format!(r#"[{room}]"#)),
            CallError::Invalid,
        ),
        (
            call(r#""rename""#, &format!(r#"[{room},"a","b"]"#)),
            CallError::Invalid,
        ),
        (
            call(r#""rename""#, &format!(r#"[{room},1]"#)),
            CallError::Invalid,
        ),
        (call(r#""rename""#, r#"["0123","a"]"#), CallError::Invalid),
        (call(r#""count""#, "{}"), CallError::Invalid),
        (call("3", "[]"), CallError::Invalid),
    ];
    for (frame, error) in cases {
        let got = client(&frame).unwrap_err();
        assert_eq!(
            got,
            WireError::InvalidCall {
                call: 9,
                error: error.clone()
            },
            "{frame}"
        );
        assert!(!got.closes_connection());
        assert_eq!(
            got.reply(),
            ServerMsg::Return {
                call: 9,
                result: Err(error)
            }
        );
    }
    // The frame itself must be well formed.
    let extra = call(r#""count""#, "[]").replace(r#""call":9"#, r#""call":9,"x":1"#);
    assert_eq!(client(&extra), Err(WireError::UnknownKey("x".into())));
    assert_eq!(
        client(r#"{"args":[],"call":"9","function":"count","t":"call","v":0}"#),
        Err(WireError::WrongType("call"))
    );
}

#[test]
fn return_round_trips_with_a_value_and_with_every_error() {
    server_round_trip(ServerMsg::Return {
        call: 7,
        result: Ok(Value::Bool(true)),
    });
    let frame = server_round_trip(ServerMsg::Return {
        call: 8,
        result: Ok(int(-3)),
    });
    assert_eq!(frame, r#"{"call":8,"result":{"ok":-3},"t":"return","v":0}"#);
    for error in [
        CallError::Denied,
        CallError::NotFound,
        CallError::Limit,
        CallError::Invalid,
        CallError::Unavailable,
        CallError::Runtime {
            kind: "IntOverflow".into(),
        },
    ] {
        server_round_trip(ServerMsg::Return {
            call: 7,
            result: Err(error),
        });
    }
    let frame = encode_server(
        &ServerMsg::Return {
            call: 7,
            result: Err(CallError::Runtime {
                kind: "DivisionByZero".into(),
            }),
        },
        &Schema,
        &open(),
    )
    .unwrap();
    assert_eq!(
        frame,
        r#"{"call":7,"result":{"runtime":"DivisionByZero"},"t":"return","v":0}"#
    );
}

#[test]
fn a_return_for_a_call_that_no_longer_waits_is_dropped() {
    let error = server(r#"{"call":99,"result":{"ok":1},"t":"return","v":0}"#).unwrap_err();
    assert_eq!(error, WireError::UnknownCall(99));
    assert!(!error.closes_connection());
}

#[test]
fn a_malformed_return_is_fatal() {
    let cases = [
        (
            r#"{"ok":"1"}"#,
            WireError::Value {
                key: "result",
                fault: ValueFault::WrongType,
            },
        ),
        (r#"{"err":"Offline"}"#, WireError::WrongType("result")),
        (r#"{"err":"denied"}"#, WireError::WrongType("result")),
        (r#"{"runtime":""}"#, WireError::WrongType("result")),
        (
            r#"{"runtime":"Int Overflow"}"#,
            WireError::WrongType("result"),
        ),
        (r#"{"ok":1,"err":"Denied"}"#, WireError::WrongType("result")),
        (r#"{}"#, WireError::WrongType("result")),
        (r#""ok""#, WireError::WrongType("result")),
    ];
    for (result, want) in cases {
        let frame = format!(r#"{{"call":8,"result":{result},"t":"return","v":0}}"#);
        let error = server(&frame).unwrap_err();
        assert_eq!(error, want, "{result}");
        assert!(error.closes_connection());
    }
}

#[test]
fn a_return_over_the_frame_limit_becomes_call_error_limit() {
    let big = |len| ServerMsg::Return {
        call: 7,
        result: Ok(text(&"x".repeat(len))),
    };
    // `{"call":7,"result":{"ok":"..."},"t":"return","v":0}` without the text.
    let envelope = encode_server(&big(0), &Schema, &open()).unwrap().len();
    let fits = encode_server(&big(FRAME - envelope), &Schema, &open()).unwrap();
    assert_eq!(fits.len(), FRAME);
    let over = encode_server(&big(FRAME - envelope + 1), &Schema, &open()).unwrap();
    assert_eq!(
        server(&over),
        Ok(ServerMsg::Return {
            call: 7,
            result: Err(CallError::Limit)
        })
    );
}

// -------------------------------------------------------------------------------------------
// snapshot

fn room_row(wall_ms: u64) -> Row {
    let f = |name| field(ROOM, name);
    Row {
        id: row(wall_ms),
        version: 0,
        fields: vec![
            (f("name"), text("general")),
            (f("topic"), Value::Null),
            (f("owner"), Value::Ref(row(2))),
            (f("made"), Value::time(1_700_000_000_000).unwrap()),
        ],
        collections: vec![
            (
                f("members"),
                CollectionState::Set(vec![
                    SetTag {
                        elem: Value::Ref(row(2)),
                        tag: op_id(1),
                    },
                    SetTag {
                        elem: Value::Ref(row(2)),
                        tag: op_id(4),
                    },
                    SetTag {
                        elem: Value::Ref(row(3)),
                        tag: op_id(2),
                    },
                ]),
            ),
            (
                f("roles"),
                CollectionState::Map(vec![
                    MapEntry {
                        key: Value::Ref(row(2)),
                        value: Some(Value::Enum("admin".into())),
                        hlc: stamp(10),
                    },
                    MapEntry {
                        key: Value::Ref(row(3)),
                        value: None,
                        hlc: stamp(11),
                    },
                ]),
            ),
        ],
    }
}

#[test]
fn snapshot_round_trips_with_fields_sets_and_maps() {
    server_round_trip(ServerMsg::Snapshot {
        sub: 1,
        rows: vec![room_row(20), room_row(21)],
        seq: ServerSeq(77),
        last: false,
    });
    server_round_trip(ServerMsg::Snapshot {
        sub: 2,
        rows: Vec::new(),
        seq: ServerSeq(0),
        last: true,
    });
    // An empty set state stays: every element was removed, the field was changed.
    let mut emptied = room_row(22);
    emptied.collections[0].1 = CollectionState::Set(Vec::new());
    server_round_trip(ServerMsg::Snapshot {
        sub: 1,
        rows: vec![emptied],
        seq: ServerSeq(1),
        last: true,
    });
}

#[test]
fn a_snapshot_row_groups_set_tags_by_element_and_hides_the_version() {
    let mut stored = room_row(20);
    stored.version = 41;
    stored.fields.truncate(1);
    stored.collections.truncate(1);
    let frame = encode_server(
        &ServerMsg::Snapshot {
            sub: 1,
            rows: vec![stored],
            seq: ServerSeq(5),
            last: true,
        },
        &Schema,
        &open(),
    )
    .unwrap();
    let (r2, r3) = (row(2).to_hex(), row(3).to_hex());
    let (t1, t2, t4) = (op_id(1).to_hex(), op_id(2).to_hex(), op_id(4).to_hex());
    assert_eq!(
        frame,
        format!(
            r#"{{"last":true,"rows":[{{"fields":{{"name":"general"}},"id":"{}","maps":{{}},"sets":{{"members":[["{r2}",["{t1}","{t4}"]],["{r3}",["{t2}"]]]}}}}],"seq":"0000000000000005","sub":1,"t":"snapshot","v":0}}"#,
            row(20).to_hex()
        )
    );
    let Ok(ServerMsg::Snapshot { rows, .. }) = server(&frame) else {
        panic!("{frame}");
    };
    assert_eq!(rows[0].version, 0);
}

#[test]
fn a_snapshot_for_a_subscription_the_client_ended_is_dropped() {
    let frame = r#"{"last":true,"rows":[],"seq":"0000000000000001","sub":5,"t":"snapshot","v":0}"#;
    let error = server(frame).unwrap_err();
    assert_eq!(error, WireError::UnknownSubscription(5));
    assert!(!error.closes_connection());
    assert_eq!(
        encode_server(
            &ServerMsg::Snapshot {
                sub: 5,
                rows: Vec::new(),
                seq: ServerSeq(1),
                last: true
            },
            &Schema,
            &open()
        ),
        Err(WireError::UnknownSubscription(5))
    );
}

#[test]
fn a_row_that_does_not_fit_the_schema_is_fatal() {
    let id = row(20).to_hex();
    let tag = op_id(1).to_hex();
    let member = row(2).to_hex();
    let row_frame = |row: &str| {
        format!(
            r#"{{"last":true,"rows":[{row}],"seq":"0000000000000001","sub":1,"t":"snapshot","v":0}}"#
        )
    };
    let row_with = |fields: &str, sets: &str, maps: &str| {
        row_frame(&format!(
            r#"{{"fields":{fields},"id":"{id}","maps":{maps},"sets":{sets}}}"#
        ))
    };
    let cases = [
        (
            row_with(r#"{"gone":1}"#, "{}", "{}"),
            WireError::UnknownField("gone".into()),
        ),
        (
            row_with(r#"{"name":1}"#, "{}", "{}"),
            WireError::Value {
                key: "fields",
                fault: ValueFault::WrongType,
            },
        ),
        (
            row_with(r#"{"members":[]}"#, "{}", "{}"),
            WireError::WrongType("fields"),
        ),
        (
            row_with("{}", r#"{"name":[]}"#, "{}"),
            WireError::WrongType("sets"),
        ),
        (
            row_with("{}", &format!(r#"{{"members":[["{member}",[]]]}}"#), "{}"),
            WireError::WrongType("sets"),
        ),
        (
            row_with(
                "{}",
                &format!(r#"{{"members":[["{member}",["x"]]]}}"#),
                "{}",
            ),
            WireError::InvalidId("sets", OpId::from_hex("x").unwrap_err()),
        ),
        (
            row_with(
                "{}",
                "{}",
                &format!(
                    r#"{{"roles":[["{member}",{{"hlc":"{}","value":"boss"}}]]}}"#,
                    stamp(1).to_hex()
                ),
            ),
            WireError::Value {
                key: "value",
                fault: ValueFault::UnknownVariant,
            },
        ),
        (
            row_with(
                "{}",
                "{}",
                &format!(
                    r#"{{"roles":[["{member}",{{"hlc":"{}","removed":false}}]]}}"#,
                    stamp(1).to_hex()
                ),
            ),
            WireError::WrongType("removed"),
        ),
        (
            row_with(
                "{}",
                "{}",
                &format!(
                    r#"{{"roles":[["{member}",{{"hlc":"{}","removed":true,"value":"admin"}}]]}}"#,
                    stamp(1).to_hex()
                ),
            ),
            WireError::UnknownKey("value".into()),
        ),
        (
            row_with(
                r#"{"members":[]}"#,
                &format!(r#"{{"members":[["{member}",["{tag}"]]]}}"#),
                "{}",
            ),
            WireError::WrongType("fields"),
        ),
        (
            row_frame(&format!(r#"{{"fields":{{}},"id":"{id}","sets":{{}}}}"#)),
            WireError::MissingKey("maps"),
        ),
        (
            row_frame(&format!(
                r#"{{"fields":{{}},"id":"{id}","maps":{{}},"sets":{{}},"version":1}}"#
            )),
            WireError::UnknownKey("version".into()),
        ),
    ];
    for (frame, want) in cases {
        let error = server(&frame).unwrap_err();
        assert_eq!(error, want, "{frame}");
        assert!(error.closes_connection());
    }
}

#[test]
fn row_bytes_is_the_length_of_the_row_in_the_snapshot() {
    let stored = room_row(20);
    let bytes = row_bytes(&stored, ROOM, &Schema).unwrap();
    let frame = encode_server(
        &ServerMsg::Snapshot {
            sub: 1,
            rows: vec![stored],
            seq: ServerSeq(1),
            last: true,
        },
        &Schema,
        &open(),
    )
    .unwrap();
    let empty = encode_server(
        &ServerMsg::Snapshot {
            sub: 1,
            rows: Vec::new(),
            seq: ServerSeq(1),
            last: true,
        },
        &Schema,
        &open(),
    )
    .unwrap();
    assert_eq!(frame.len(), empty.len() + bytes);
}

#[test]
fn a_row_over_the_row_limit_is_never_written() {
    let mut big = room_row(20);
    let name = field(ROOM, "name");
    let topic = field(ROOM, "topic");
    let chunk = "x".repeat(LIMITS.text_bytes as usize);
    big.fields = vec![(name, text(&chunk)), (topic, text(&chunk))];
    big.collections = vec![(
        field(ROOM, "roles"),
        CollectionState::Map(
            (0..4_000)
                .map(|i| MapEntry {
                    key: Value::Ref(row(100 + i)),
                    value: Some(Value::Enum("member".into())),
                    hlc: stamp(1),
                })
                .collect(),
        ),
    )];
    let Err(WireError::RowTooLarge { row: id, bytes }) = row_bytes(&big, ROOM, &Schema) else {
        panic!("not refused");
    };
    assert_eq!(id, row(20));
    assert!(bytes > LIMITS.row_bytes as usize);
}

// -------------------------------------------------------------------------------------------
// ops from the server

fn logged(seq: u64, op_seq: u32, body: OpBody) -> LoggedOp {
    LoggedOp {
        seq: ServerSeq(seq),
        op: Op {
            id: op_id(op_seq),
            hlc: stamp(30),
            model: ROOM,
            row: row(20),
            body,
        },
    }
}

#[test]
fn server_ops_round_trip_with_their_log_position() {
    let frame = server_round_trip(ServerMsg::Ops(vec![
        logged(
            5,
            1,
            OpBody::Set {
                field: field(ROOM, "name"),
                value: text("lobby"),
            },
        ),
        logged(
            6,
            2,
            OpBody::SetAdd {
                field: field(ROOM, "members"),
                elem: Value::Ref(row(2)),
            },
        ),
    ]));
    assert!(
        frame.starts_with(r#"{"ops":[{"op":{"body":{"set":"#),
        "{frame}"
    );
    assert!(frame.contains(r#","seq":"0000000000000005"}"#), "{frame}");
    server_round_trip(ServerMsg::Ops(Vec::new()));
}

#[test]
fn a_server_op_that_does_not_fit_the_schema_is_fatal() {
    let op = logged(5, 1, OpBody::Drop);
    let text = encode_op(&op.op, &Schema).unwrap();
    let frame =
        |op: &str| format!(r#"{{"ops":[{{"op":{op},"seq":"0000000000000005"}}],"t":"ops","v":0}}"#);
    let error = server(&frame(&text.replace(r#""Room""#, r#""Gone""#))).unwrap_err();
    assert_eq!(
        error,
        WireError::ServerOp {
            op: op_id(1),
            fault: OpFault::UnknownModel("Gone".into())
        }
    );
    assert!(error.closes_connection());
    let both = text.replace(r#"{"drop":{}}"#, r#"{"drop":{},"make":{}}"#);
    assert_eq!(
        server(&frame(&both)),
        Err(WireError::ServerOp {
            op: op_id(1),
            fault: OpFault::SeveralKinds
        })
    );
    assert_eq!(
        server(r#"{"ops":[{"op":{}}],"t":"ops","v":0}"#),
        Err(WireError::MissingKey("seq"))
    );
}

// -------------------------------------------------------------------------------------------
// Paging (D121)

/// A `Person` row with a handle of `len` bytes.
fn person(wall_ms: u64, len: usize) -> Row {
    Row {
        id: row(wall_ms),
        version: 3,
        fields: vec![(field(PERSON, "handle"), text(&"h".repeat(len)))],
        collections: Vec::new(),
    }
}

fn page_rows(page: &ServerMsg) -> &[Row] {
    match page {
        ServerMsg::Snapshot { rows, .. } => rows,
        other => panic!("not a snapshot: {other:?}"),
    }
}

#[test]
fn snapshot_pages_close_at_the_row_limit() {
    let per_page = LIMITS.rows_per_snapshot as usize;
    let rows: Vec<Row> = (0..2 * per_page as u64 + 1).map(|i| person(i, 1)).collect();
    let pages = snapshot_pages(2, PERSON, rows, ServerSeq(9), &Schema).unwrap();
    let sizes: Vec<usize> = pages.iter().map(|p| page_rows(p).len()).collect();
    assert_eq!(sizes, [per_page, per_page, 1]);
    for (at, page) in pages.iter().enumerate() {
        let ServerMsg::Snapshot { sub, seq, last, .. } = page else {
            unreachable!()
        };
        assert_eq!((*sub, *seq, *last), (2, ServerSeq(9), at == 2));
    }
}

#[test]
fn snapshot_pages_close_before_the_row_that_would_overflow_the_frame() {
    // Rows of about 200 KiB: five fit a frame, six do not.
    let rows: Vec<Row> = (0..11).map(|i| person(i, 200 << 10)).collect();
    let pages = snapshot_pages(2, PERSON, rows.clone(), ServerSeq(9), &Schema).unwrap();
    let mut seen = Vec::new();
    for page in &pages {
        let frame = encode_server(page, &Schema, &open()).unwrap();
        assert!(frame.len() <= FRAME);
        // The next row would not have fit.
        seen.extend_from_slice(page_rows(page));
    }
    assert_eq!(seen.len(), rows.len());
    assert!(seen.iter().zip(&rows).all(|(a, b)| a.id == b.id));
    let sizes: Vec<usize> = pages.iter().map(|p| page_rows(p).len()).collect();
    assert_eq!(sizes, [5, 5, 1]);
    // No rows: one empty last page.
    let empty = snapshot_pages(2, PERSON, Vec::new(), ServerSeq(9), &Schema).unwrap();
    assert_eq!(
        empty,
        [ServerMsg::Snapshot {
            sub: 2,
            rows: Vec::new(),
            seq: ServerSeq(9),
            last: true
        }]
    );
}

#[test]
fn snapshot_pages_fill_a_frame_exactly() {
    // Two rows whose page is exactly one frame stay on one page.
    let one = |len| person(1, len);
    let envelope = encode_server(
        &ServerMsg::Snapshot {
            sub: 2,
            rows: Vec::new(),
            seq: ServerSeq(9),
            last: false,
        },
        &Schema,
        &open(),
    )
    .unwrap()
    .len();
    let base = row_bytes(&one(0), PERSON, &Schema).unwrap();
    let room = FRAME - envelope - 1 - 2 * base;
    let rows = vec![person(1, room / 2), person(2, room - room / 2)];
    let pages = snapshot_pages(2, PERSON, rows.clone(), ServerSeq(9), &Schema).unwrap();
    assert_eq!(pages.len(), 1);
    let frame = encode_server(
        &ServerMsg::Snapshot {
            sub: 2,
            rows,
            seq: ServerSeq(9),
            last: false,
        },
        &Schema,
        &open(),
    )
    .unwrap();
    assert_eq!(frame.len(), FRAME);
    let over = vec![person(1, room / 2), person(2, room - room / 2 + 1)];
    let pages = snapshot_pages(2, PERSON, over, ServerSeq(9), &Schema).unwrap();
    assert_eq!(pages.len(), 2);
}

#[test]
fn ops_pages_keep_the_order_and_fit_the_frame() {
    let big = |i: u32| {
        logged(
            u64::from(i),
            i,
            OpBody::Set {
                field: field(ROOM, "name"),
                value: text(&"n".repeat(300 << 10)),
            },
        )
    };
    let ops: Vec<LoggedOp> = (1..=7).map(big).collect();
    let pages = ops_pages(ops.clone(), &Schema).unwrap();
    assert_eq!(pages.len(), 3);
    let mut seen = Vec::new();
    for page in &pages {
        let frame = encode_server(page, &Schema, &open()).unwrap();
        assert!(frame.len() <= FRAME);
        let ServerMsg::Ops(ops) = page else {
            unreachable!()
        };
        seen.extend(ops.iter().cloned());
    }
    assert_eq!(seen, ops);
    assert_eq!(ops_pages(Vec::new(), &Schema), Ok(Vec::new()));
}
