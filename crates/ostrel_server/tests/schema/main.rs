//! The implicit fields `made`, `changed` and `author` through the server schema and the sync
//! codec (D186, ARCHITECTURE 5.4): a `snapshot` row carries all three by name, a query sorts
//! by `made` and filters and hops on `author`, and no op writes any of them.

use ostrel_core::ids::{Hlc, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::Value;
use ostrel_db::api::{
    CmpOp, Dir, Expr, FIELD_AUTHOR, FIELD_CHANGED, FIELD_MADE, Hop, Path, Query, Row,
};
use ostrel_server::schema::{Auth, FieldDecl, ModelDecl, ServerSchema};
use ostrel_sync::protocol::{
    CallId, ClientMsg, DecodedOp, FnId, ModelId, OpFault, ServerMsg, SubId,
};
use ostrel_sync::wire::{
    Requests, WireSchema, WireTy, decode_client, decode_op, decode_server, encode_server, json,
};

const USER: ModelId = 0;
const ISSUE: ModelId = 1;

/// `User` (0) with `handle`, `Issue` (1) with `title`; the program has `auth`, so `author` of
/// every row refers to `User`.
fn schema() -> Result<ServerSchema, String> {
    let field = |name: &str, ty| FieldDecl {
        name: name.into(),
        ty,
    };
    ServerSchema::new(
        vec![
            ModelDecl {
                name: "User".into(),
                fields: vec![field("handle", WireTy::Text)],
            },
            ModelDecl {
                name: "Issue".into(),
                fields: vec![field("title", WireTy::Text)],
            },
        ],
        Vec::new(),
        Auth::On { user: USER },
    )
    .map_err(|e| e.to_string())
}

/// One open subscription, 4, on `Issue`.
struct Open;

impl Requests for Open {
    fn subscription_model(&self, sub: SubId) -> Option<ModelId> {
        (sub == 4).then_some(ISSUE)
    }

    fn call_function(&self, _: CallId) -> Option<FnId> {
        None
    }
}

fn row_id(physical: u64) -> Result<RowId, String> {
    let hlc = Hlc::new(physical, 0, ReplicaId(3)).map_err(|e| format!("{e:?}"))?;
    Ok(RowId::new(hlc))
}

fn time(ms: i64) -> Result<Value, String> {
    Value::time(ms).map_err(|e| format!("{e:?}"))
}

#[test]
fn snapshot_row_carries_made_changed_and_author_by_name() -> Result<(), String> {
    let s = schema()?;
    let author = row_id(5)?;
    let row = Row {
        id: row_id(9)?,
        version: 0,
        fields: vec![
            (0, Value::Text("Crash on save".into())),
            (FIELD_MADE, time(1_000)?),
            (FIELD_CHANGED, time(2_000)?),
            (FIELD_AUTHOR, Value::Ref(author)),
        ],
        collections: Vec::new(),
    };
    let sent = ServerMsg::Snapshot {
        sub: 4,
        rows: vec![row],
        seq: ServerSeq(7),
        last: true,
    };
    let text = encode_server(&sent, &s, &Open).map_err(|e| format!("{e:?}"))?;
    assert!(text.contains(r#""made":1000"#), "{text}");
    assert!(text.contains(r#""changed":2000"#), "{text}");
    assert!(
        text.contains(&format!(r#""author":"{}""#, author.to_hex())),
        "{text}"
    );
    let read = decode_server(text.as_bytes(), &s, &Open).map_err(|e| format!("{e:?}"))?;
    assert_eq!(read, sent);
    Ok(())
}

/// A stamp of the wrong type in a `snapshot` row is refused like any other field value.
#[test]
fn snapshot_row_types_the_implicit_fields() -> Result<(), String> {
    let s = schema()?;
    let id = row_id(9)?.to_hex();
    let frame = |fields: &str| {
        format!(
            r#"{{"last":true,"rows":[{{"fields":{{{fields}}},"id":"{id}","maps":{{}},"sets":{{}}}}],"seq":"0000000000000007","sub":4,"t":"snapshot","v":0}}"#
        )
    };
    let good = frame(r#""made":1"#);
    assert!(decode_server(good.as_bytes(), &s, &Open).is_ok(), "{good}");
    for bad in [r#""made":"x""#, r#""changed":true"#, r#""author":5"#] {
        let text = frame(bad);
        assert!(decode_server(text.as_bytes(), &s, &Open).is_err(), "{text}");
    }
    Ok(())
}

/// `sort made desc`, `where author == u` and a hop over `author` to `User.handle`.
#[test]
fn query_sorts_by_made_and_filters_on_author() -> Result<(), String> {
    let s = schema()?;
    let user = row_id(5)?;
    let frame = format!(
        r#"{{"query":{{"after":null,"filter":{{"and":[{{"cmp":["eq",{{"field":{{"field":"author","hops":[]}}}},{{"const":"{user}"}}]}},{{"cmp":["eq",{{"field":{{"field":"handle","hops":[["author","User"]]}}}},{{"const":"ada"}}]}}]}},"limit":null,"model":"Issue","order":[["made","desc"],["changed","asc"]]}},"sub":4,"t":"subscribe","v":0}}"#,
        user = user.to_hex()
    );
    let read = decode_client(frame.as_bytes(), &s).map_err(|e| format!("{e:?}"))?;
    let expected = Query {
        model: ISSUE,
        filter: Some(Expr::And(vec![
            Expr::Cmp(
                CmpOp::Eq,
                Box::new(Expr::Field(Path {
                    hops: Vec::new(),
                    field: FIELD_AUTHOR,
                })),
                Box::new(Expr::Const(Value::Ref(user))),
            ),
            Expr::Cmp(
                CmpOp::Eq,
                Box::new(Expr::Field(Path {
                    hops: vec![Hop {
                        field: FIELD_AUTHOR,
                        model: USER,
                    }],
                    field: 0,
                })),
                Box::new(Expr::Const(Value::Text("ada".into()))),
            ),
        ])),
        order: vec![(FIELD_MADE, Dir::Desc), (FIELD_CHANGED, Dir::Asc)],
        limit: None,
        after: None,
    };
    let ClientMsg::Subscribe { sub: 4, query } = read else {
        return Err(format!("{read:?}"));
    };
    assert_eq!(query, expected);
    Ok(())
}

/// `make` and `edit` that name an implicit field are `Invalid` with `UnknownField`, so no op
/// ever writes a stamp or the author (D116, D186).
#[test]
fn make_and_edit_never_write_an_implicit_field() -> Result<(), String> {
    let s = schema()?;
    let head = r#""hlc":"00010203040500070123456789abcdef","id":"0123456789abcdef00000001","model":"Issue","row":"00010203040500070123456789abcdef""#;
    let author = format!("\"{}\"", row_id(5)?.to_hex());
    for (name, value) in [("made", "1"), ("changed", "1"), ("author", author.as_str())] {
        for body in [
            format!(r#"{{"make":{{"{name}":{value},"title":"x"}}}}"#),
            format!(r#"{{"set":{{"field":"{name}","value":{value}}}}}"#),
        ] {
            let text = format!(r#"{{"body":{body},{head}}}"#);
            let parsed = json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?;
            let decoded = decode_op(parsed, &s).map_err(|e| format!("{e:?}"))?;
            match decoded {
                DecodedOp::Invalid {
                    fault: OpFault::UnknownField(ref field),
                    ..
                } if field == name => {}
                other => return Err(format!("{text}: {other:?}")),
            }
        }
    }
    // The same frame with a declared field is a valid op.
    let text = format!(r#"{{"body":{{"set":{{"field":"title","value":"x"}}}},{head}}}"#);
    let parsed = json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?;
    let decoded = decode_op(parsed, &s).map_err(|e| format!("{e:?}"))?;
    assert!(matches!(decoded, DecodedOp::Valid(_)), "{decoded:?}");
    // The schema itself resolves the names, only the op decoder refuses them.
    assert_eq!(s.field_id(ISSUE, "made"), Some(FIELD_MADE));
    Ok(())
}
