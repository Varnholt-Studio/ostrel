//! Ops on the wire (ARCHITECTURE 5.4 "Wire form", D132): names instead of ids, values typed
//! by the schema, form faults fatal, schema faults only make the op `Invalid`, and the op size
//! of D109.

use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId};
use ostrel_core::value::Value;
use ostrel_sync::protocol::{
    ClientMsg, DecodedOp, FieldId, FnId, LIMITS, ModelId, Op, OpBody, OpFault, ValueFault,
};
use ostrel_sync::wire::json::{self, Json};
use ostrel_sync::wire::{
    WireError, WireSchema, WireTy, decode_client, decode_op, encode_client, encode_op, op_bytes,
    read_value,
};

/// Model 0 `Item` with one field per wire type, model 1 `Empty` without fields.
struct Schema {
    fields: Vec<(&'static str, WireTy)>,
}

fn schema() -> Schema {
    use WireTy::*;
    Schema {
        fields: vec![
            ("done", Bool),
            ("count", Int),
            ("weight", Float),
            ("title", Text),
            ("due", Time),
            ("blob", Bytes),
            ("state", Enum(vec!["open".into(), "closed".into()])),
            ("owner", Ref),
            ("note", Optional(Box::new(Text))),
            ("tags", Set(Box::new(Text))),
            ("roles", Map(Box::new(Ref), Box::new(Int))),
            ("steps", List(Box::new(Int))),
            ("pos", Rank),
            ("number", Serial),
        ],
    }
}

const ITEM: ModelId = 0;

impl WireSchema for Schema {
    fn model_id(&self, name: &str) -> Option<ModelId> {
        match name {
            "Item" => Some(0),
            "Empty" => Some(1),
            _ => None,
        }
    }

    fn model_name(&self, model: ModelId) -> Option<&str> {
        match model {
            0 => Some("Item"),
            1 => Some("Empty"),
            _ => None,
        }
    }

    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
        let at = self.fields.iter().position(|(n, _)| *n == name)?;
        (model == ITEM).then(|| FieldId::try_from(at).ok())?
    }

    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
        let (name, _) = self.fields.get(usize::try_from(field).ok()?)?;
        (model == ITEM).then_some(*name)
    }

    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
        let (_, ty) = self.fields.get(usize::try_from(field).ok()?)?;
        (model == ITEM).then_some(ty)
    }

    fn function_id(&self, _: &str) -> Option<FnId> {
        None
    }

    fn function_name(&self, _: FnId) -> Option<&str> {
        None
    }

    fn function_params(&self, _: FnId) -> Option<&[WireTy]> {
        None
    }

    fn function_result(&self, _: FnId) -> Option<&WireTy> {
        None
    }
}

fn field(name: &str) -> FieldId {
    schema().field_id(ITEM, name).unwrap()
}

fn replica() -> ReplicaId {
    ReplicaId(0x0123_4567_89ab_cdef)
}

fn stamp() -> Hlc {
    Hlc::new(0x0001_0203_0405, 7, replica()).unwrap()
}

fn row() -> RowId {
    RowId::new(stamp())
}

fn op_id(seq: u32) -> OpId {
    OpId {
        replica: replica(),
        seq,
    }
}

fn op(body: OpBody) -> Op {
    Op {
        id: op_id(1),
        hlc: stamp(),
        model: ITEM,
        row: row(),
        body,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn int(v: i64) -> Value {
    Value::int(v).unwrap()
}

fn decode_text(text: &str) -> Result<DecodedOp, WireError> {
    decode_op(json::parse(text.as_bytes())?, &schema())
}

fn round_trip(op: Op) {
    let text = encode_op(&op, &schema()).unwrap();
    assert_eq!(decode_text(&text), Ok(DecodedOp::Valid(op)), "{text}");
}

/// A valid op text with `body` in place of the body.
fn with_body(body: &str) -> String {
    let op = encode_op(&op(OpBody::Drop), &schema()).unwrap();
    op.replace(r#"{"drop":{}}"#, body)
}

fn invalid(fault: OpFault) -> Result<DecodedOp, WireError> {
    Ok(DecodedOp::Invalid {
        id: op_id(1),
        hlc: stamp(),
        row: row(),
        fault,
    })
}

// -------------------------------------------------------------------------------------------
// Round trips and form

#[test]
fn a_make_with_every_wire_type_round_trips() {
    let other = RowId::new(Hlc::new(9, 0, replica()).unwrap());
    round_trip(op(OpBody::Make {
        fields: vec![
            (field("done"), Value::Bool(true)),
            (field("count"), int(-9_007_199_254_740_991)),
            (field("weight"), Value::float(2f64.powi(53)).unwrap()),
            (field("title"), text("a \"quoted\" title\n")),
            (field("due"), Value::time(1_700_000_000_000).unwrap()),
            (field("blob"), Value::Bytes(vec![0, 0xfb, 0xff])),
            (field("state"), Value::Enum("closed".into())),
            (field("owner"), Value::Ref(other)),
            (field("note"), Value::Null),
            (field("tags"), Value::Set(vec![text("a"), text("b")])),
            (
                field("roles"),
                Value::Map(vec![(Value::Ref(other), int(2))]),
            ),
            (field("steps"), Value::List(vec![int(3), int(1), int(3)])),
            (field("pos"), Value::Rank("U".into())),
            (field("number"), int(42)),
        ],
    }));
}

#[test]
fn every_body_kind_round_trips() {
    let tags = vec![op_id(1), op_id(2)];
    for body in [
        OpBody::Drop,
        OpBody::Set {
            field: field("pos"),
            value: Value::Rank("a1".into()),
        },
        OpBody::Set {
            field: field("steps"),
            value: Value::List(vec![]),
        },
        OpBody::Set {
            field: field("note"),
            value: text("n"),
        },
        OpBody::SetAdd {
            field: field("tags"),
            elem: text("x"),
        },
        OpBody::SetRemove {
            field: field("tags"),
            elem: text("x"),
            tags,
        },
        OpBody::MapPut {
            field: field("roles"),
            key: Value::Ref(row()),
            value: int(1),
        },
        OpBody::MapRemove {
            field: field("roles"),
            key: Value::Ref(row()),
        },
    ] {
        round_trip(op(body));
    }
}

#[test]
fn ops_travel_with_names_in_canonical_form() {
    let op = op(OpBody::Set {
        field: field("count"),
        value: int(5),
    });
    let text = encode_op(&op, &schema()).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"body":{{"set":{{"field":"count","value":5}}}},"hlc":"{}","id":"{}","model":"Item","row":"{}"}}"#,
            stamp().to_hex(),
            op_id(1).to_hex(),
            row().to_hex()
        )
    );
    assert_eq!(op_bytes(&op, &schema()), Ok(text.len()));
}

#[test]
fn a_malformed_op_is_a_protocol_error() {
    let good = with_body(r#"{"drop":{}}"#);
    assert!(matches!(decode_text(&good), Ok(DecodedOp::Valid(_))));
    let cases = [
        (r#"[1]"#.to_owned(), WireError::WrongType("ops")),
        (
            good.replace(r#""model":"Item","#, ""),
            WireError::MissingKey("model"),
        ),
        (
            good.replace(r#""model":"Item""#, r#""model":0"#),
            WireError::WrongType("model"),
        ),
        (
            good.replace(r#""model":"Item","#, r#""model":"Item","x":1,"#),
            WireError::UnknownKey("x".into()),
        ),
        (
            good.replace(&stamp().to_hex(), "zz"),
            WireError::InvalidId("hlc", ostrel_core::ids::Hlc::from_hex("zz").unwrap_err()),
        ),
        (with_body(r#"{}"#), WireError::WrongType("body")),
        (with_body(r#"{"move":{}}"#), WireError::WrongType("body")),
        (
            with_body(r#"{"drop":{},"move":{}}"#),
            WireError::WrongType("body"),
        ),
        (with_body(r#"{"drop":[]}"#), WireError::WrongType("body")),
        (
            with_body(r#"{"drop":{"field":"pos"}}"#),
            WireError::UnknownKey("field".into()),
        ),
        (
            with_body(r#"{"set":{"field":"pos"}}"#),
            WireError::MissingKey("value"),
        ),
        (
            with_body(r#"{"set":{"field":3,"value":1}}"#),
            WireError::WrongType("field"),
        ),
        (
            with_body(r#"{"make":{"title":"a","title":"b"}}"#),
            WireError::DuplicateKey("title".into()),
        ),
    ];
    for (text, want) in cases {
        assert_eq!(decode_text(&text), Err(want), "{text}");
    }
}

// -------------------------------------------------------------------------------------------
// Schema faults: only the op is invalid

#[test]
fn unknown_names_make_only_the_op_invalid() {
    let text = with_body(r#"{"drop":{}}"#).replace(r#""Item""#, r#""Gone""#);
    assert_eq!(
        decode_text(&text),
        invalid(OpFault::UnknownModel("Gone".into()))
    );
    assert_eq!(
        decode_text(&with_body(r#"{"set":{"field":"gone","value":1}}"#)),
        invalid(OpFault::UnknownField("gone".into()))
    );
    assert_eq!(
        decode_text(&with_body(r#"{"make":{"title":"t","gone":1}}"#)),
        invalid(OpFault::UnknownField("gone".into()))
    );
    // A model without fields knows none of them.
    let text =
        with_body(r#"{"set":{"field":"title","value":"t"}}"#).replace(r#""Item""#, r#""Empty""#);
    assert_eq!(
        decode_text(&text),
        invalid(OpFault::UnknownField("title".into()))
    );
}

#[test]
fn a_body_must_suit_the_type_of_its_field() {
    for (body, name) in [
        (r#"{"add":{"field":"title","elem":"x"}}"#, "title"),
        (r#"{"set":{"field":"tags","value":["x"]}}"#, "tags"),
        (r#"{"set":{"field":"roles","value":[]}}"#, "roles"),
        (r#"{"put":{"field":"tags","key":"x","value":1}}"#, "tags"),
        (r#"{"remove_key":{"field":"count","key":1}}"#, "count"),
        (
            r#"{"remove":{"field":"steps","elem":1,"tags":["000000000000000100000001"]}}"#,
            "steps",
        ),
    ] {
        assert_eq!(
            decode_text(&with_body(body)),
            invalid(OpFault::WrongBody { field: field(name) }),
            "{body}"
        );
    }
}

#[test]
fn a_remove_names_one_to_sixty_four_valid_tags() {
    let tag = |seq: u32| format!("\"{}\"", op_id(seq).to_hex());
    let remove = |tags: Vec<String>| {
        with_body(&format!(
            r#"{{"remove":{{"field":"tags","elem":"x","tags":[{}]}}}}"#,
            tags.join(",")
        ))
    };
    let max: Vec<String> = (1..=LIMITS.tags_per_remove).map(tag).collect();
    assert!(matches!(
        decode_text(&remove(max.clone())),
        Ok(DecodedOp::Valid(_))
    ));
    let over: Vec<String> = (1..=LIMITS.tags_per_remove + 1).map(tag).collect();
    for tags in [
        vec![],
        over,
        vec!["\"ABC\"".to_owned()],
        vec!["1".to_owned()],
    ] {
        assert_eq!(
            decode_text(&remove(tags)),
            invalid(OpFault::Tags {
                field: field("tags")
            })
        );
    }
}

#[test]
fn values_that_do_not_fit_their_type_make_the_op_invalid() {
    let cases = [
        ("count", "9007199254740992", ValueFault::IntRange),
        ("count", "18446744073709551616", ValueFault::IntRange),
        ("count", "1e20", ValueFault::IntRange),
        ("count", "1.5", ValueFault::WrongType),
        ("count", "\"1\"", ValueFault::WrongType),
        ("number", "9007199254740992", ValueFault::IntRange),
        ("due", "-9007199254740992", ValueFault::IntRange),
        ("done", "null", ValueFault::WrongType),
        ("weight", "\"1\"", ValueFault::WrongType),
        ("state", "\"Open\"", ValueFault::UnknownVariant),
        ("owner", "\"0123\"", ValueFault::InvalidRef),
        ("blob", "\"AA==\"", ValueFault::InvalidBytes),
        ("pos", "\"a0\"", ValueFault::InvalidRank),
        ("pos", "\"\"", ValueFault::InvalidRank),
        ("note", "1", ValueFault::WrongType),
        ("steps", "[1,\"2\"]", ValueFault::WrongType),
    ];
    for (name, value, fault) in cases {
        let body = format!(r#"{{"set":{{"field":"{name}","value":{value}}}}}"#);
        assert_eq!(
            decode_text(&with_body(&body)),
            invalid(OpFault::Value {
                field: field(name),
                fault
            }),
            "{name} = {value}"
        );
    }
}

#[test]
fn sets_and_maps_hold_each_element_and_key_once() {
    let owner = format!("\"{}\"", row().to_hex());
    let cases = [
        ("tags", r#"["a","b","a"]"#.to_owned(), ValueFault::Repeated),
        (
            "roles",
            format!("[[{owner},1],[{owner},2]]"),
            ValueFault::Repeated,
        ),
        ("roles", format!("[[{owner}]]"), ValueFault::WrongType),
    ];
    for (name, value, fault) in cases {
        let body = format!(r#"{{"make":{{"{name}":{value}}}}}"#);
        assert_eq!(
            decode_text(&with_body(&body)),
            invalid(OpFault::Value {
                field: field(name),
                fault
            }),
            "{body}"
        );
    }
}

#[test]
fn floats_read_from_every_canonical_form() {
    // Integral floats are written as integer tokens (ARCHITECTURE 5.3), also beyond the safe
    // integer range; a `Float` field reads them, an `Int` field refuses them.
    for (token, want) in [
        ("9007199254740992", 2f64.powi(53)),
        ("100000000000000000000", 1e20),
        ("1e+21", 1e21),
        ("-0", 0.0),
        ("0.5", 0.5),
    ] {
        let body = format!(r#"{{"set":{{"field":"weight","value":{token}}}}}"#);
        let Ok(DecodedOp::Valid(op)) = decode_text(&with_body(&body)) else {
            panic!("{token} not read");
        };
        let OpBody::Set {
            value: Value::Float(f),
            ..
        } = op.body
        else {
            panic!("{token} not a float");
        };
        assert_eq!(f.get(), want, "{token}");
    }
}

#[test]
fn text_is_limited_by_utf8_bytes() {
    let limit = LIMITS.text_bytes as usize;
    let fits = Json::Str("é".repeat(limit / 2));
    assert_eq!(
        read_value(&WireTy::Text, fits.clone()),
        Ok(Value::Text("é".repeat(limit / 2)))
    );
    let over = Json::Str(format!("{}a", "é".repeat(limit / 2)));
    assert_eq!(
        read_value(&WireTy::Text, over),
        Err(ValueFault::TextTooLong)
    );
}

#[test]
fn values_nest_at_most_thirty_two_collections_deep() {
    let depth = LIMITS.nesting_depth as usize;
    let mut ty = WireTy::Int;
    for _ in 0..=depth {
        ty = WireTy::List(Box::new(ty));
    }
    let nested = |levels: usize| {
        let text = format!("{}1{}", "[".repeat(levels), "]".repeat(levels));
        json::parse(text.as_bytes()).unwrap()
    };
    // `ty` allows depth + 1 levels; the limit stops the value at depth + 1.
    let WireTy::List(inner) = &ty else {
        unreachable!()
    };
    assert!(read_value(inner, nested(depth)).is_ok());
    assert_eq!(read_value(&ty, nested(depth + 1)), Err(ValueFault::TooDeep));
}

// -------------------------------------------------------------------------------------------
// Op size (D109)

#[test]
fn an_op_over_the_op_limit_is_invalid() {
    // Three texts within the text limit make an op over the op limit.
    let big = "x".repeat(LIMITS.text_bytes as usize);
    let body = format!(r#"{{"make":{{"title":"{big}","note":"{big}","tags":["{big}"]}}}}"#);
    let text = with_body(&body);
    let Ok(DecodedOp::Invalid {
        fault: OpFault::TooLarge { bytes },
        ..
    }) = decode_text(&text)
    else {
        panic!("not refused as too large");
    };
    assert_eq!(bytes, text.len());
    assert!(bytes > LIMITS.op_bytes as usize);
}

#[test]
fn an_op_of_exactly_the_op_limit_is_valid() {
    // Two text fields share the bytes, each within the text limit; ASCII letters encode as
    // one byte each, so the op grows by exactly the bytes added.
    let make = |title: usize, note: usize| {
        op(OpBody::Make {
            fields: vec![
                (field("title"), text(&"y".repeat(title))),
                (field("note"), text(&"z".repeat(note))),
            ],
        })
    };
    let fill = LIMITS.op_bytes as usize - op_bytes(&make(0, 0), &schema()).unwrap();
    let exact = make(fill / 2, fill - fill / 2);
    assert_eq!(op_bytes(&exact, &schema()), Ok(LIMITS.op_bytes as usize));
    let text = encode_op(&exact, &schema()).unwrap();
    assert_eq!(decode_text(&text), Ok(DecodedOp::Valid(exact)));
    let over = encode_op(&make(fill / 2, fill - fill / 2 + 1), &schema()).unwrap();
    assert_eq!(
        decode_text(&over),
        invalid(OpFault::TooLarge {
            bytes: LIMITS.op_bytes as usize + 1
        })
    );
}

#[test]
fn ids_unknown_to_the_schema_are_not_written() {
    let mut unknown_model = op(OpBody::Drop);
    unknown_model.model = 9;
    assert_eq!(
        encode_op(&unknown_model, &schema()),
        Err(WireError::NotInSchema("model"))
    );
    let unknown_field = op(OpBody::Set {
        field: 99,
        value: int(1),
    });
    assert_eq!(
        encode_op(&unknown_field, &schema()),
        Err(WireError::NotInSchema("field"))
    );
}

// -------------------------------------------------------------------------------------------
// The client `ops` message (D135)

fn ops_frame(ops: &[String]) -> String {
    format!(r#"{{"ops":[{}],"t":"ops","v":0}}"#, ops.join(","))
}

fn decode_ops_frame(frame: &str) -> Result<ClientMsg, WireError> {
    decode_client(frame.as_bytes(), &schema())
}

fn op_with(seq: u32, body: OpBody) -> Op {
    Op {
        id: op_id(seq),
        ..op(body)
    }
}

#[test]
fn a_client_ops_batch_round_trips() {
    let batch = ClientMsg::Ops(vec![
        DecodedOp::Valid(op_with(1, OpBody::Drop)),
        DecodedOp::Valid(op_with(
            2,
            OpBody::Set {
                field: field("title"),
                value: text("x"),
            },
        )),
    ]);
    let frame = encode_client(&batch, &schema()).unwrap();
    assert_eq!(decode_ops_frame(&frame), Ok(batch), "{frame}");
    let empty = ClientMsg::Ops(Vec::new());
    let frame = encode_client(&empty, &schema()).unwrap();
    assert_eq!(frame, r#"{"ops":[],"t":"ops","v":0}"#);
    assert_eq!(decode_ops_frame(&frame), Ok(empty));
}

#[test]
fn an_op_that_does_not_fit_the_schema_leaves_the_rest_of_the_batch_valid() {
    let good = encode_op(&op_with(1, OpBody::Drop), &schema()).unwrap();
    let bad = encode_op(
        &op_with(
            2,
            OpBody::Set {
                field: field("count"),
                value: int(1),
            },
        ),
        &schema(),
    )
    .unwrap()
    .replace(r#""value":1"#, r#""value":"1""#);
    let later = encode_op(&op_with(3, OpBody::Drop), &schema()).unwrap();
    let Ok(ClientMsg::Ops(ops)) = decode_ops_frame(&ops_frame(&[good, bad, later])) else {
        panic!("expected an ops batch");
    };
    assert_eq!(
        ops,
        [
            DecodedOp::Valid(op_with(1, OpBody::Drop)),
            DecodedOp::Invalid {
                id: op_id(2),
                hlc: stamp(),
                row: row(),
                fault: OpFault::Value {
                    field: field("count"),
                    fault: ValueFault::WrongType,
                },
            },
            DecodedOp::Valid(op_with(3, OpBody::Drop)),
        ]
    );
}

#[test]
fn a_malformed_op_head_fails_the_whole_frame() {
    let good = encode_op(&op_with(1, OpBody::Drop), &schema()).unwrap();
    let bad_head = good.replace(r#""row":""#, r#""row":"x"#);
    assert!(matches!(
        decode_ops_frame(&ops_frame(&[good.clone(), bad_head])),
        Err(WireError::InvalidId("row", _))
    ));
    assert_eq!(
        decode_ops_frame(r#"{"ops":{},"t":"ops","v":0}"#),
        Err(WireError::WrongType("ops"))
    );
    assert_eq!(
        decode_ops_frame(r#"{"ops":[1],"t":"ops","v":0}"#),
        Err(WireError::WrongType("ops"))
    );
    assert_eq!(
        decode_ops_frame(r#"{"t":"ops","v":0}"#),
        Err(WireError::MissingKey("ops"))
    );
}

#[test]
fn encoding_an_invalid_op_is_a_typed_error() {
    let batch = ClientMsg::Ops(vec![
        DecodedOp::Valid(op_with(1, OpBody::Drop)),
        DecodedOp::Invalid {
            id: op_id(2),
            hlc: stamp(),
            row: row(),
            fault: OpFault::UnknownModel("Gone".into()),
        },
    ]);
    assert_eq!(
        encode_client(&batch, &schema()),
        Err(WireError::InvalidOp(op_id(2)))
    );
}

#[test]
fn a_batch_over_the_batch_limit_is_read_whole() {
    // The codec does not count ops: the server answers a batch over the limit with one
    // `Reject Limit` per op, in batch order, so it needs every op head (D106).
    let count = LIMITS.ops_per_batch + 1;
    let ops: Vec<String> = (1..=count)
        .map(|seq| encode_op(&op_with(seq, OpBody::Drop), &schema()).unwrap())
        .collect();
    let Ok(ClientMsg::Ops(decoded)) = decode_ops_frame(&ops_frame(&ops)) else {
        panic!("expected an ops batch");
    };
    assert_eq!(decoded.len(), count as usize);
    assert!(
        decoded
            .iter()
            .zip(1..)
            .all(|(op, seq)| *op == DecodedOp::Valid(op_with(seq, OpBody::Drop)))
    );
}

#[test]
fn a_client_never_writes_an_op_over_the_op_limit() {
    // The client does not apply such a write and sends no op (D109), so the codec refuses to
    // write one instead of leaving the check to every caller.
    let make = |title: usize| {
        op(OpBody::Make {
            fields: vec![
                (field("title"), text(&"y".repeat(title))),
                (field("note"), text(&"z".repeat(LIMITS.text_bytes as usize))),
            ],
        })
    };
    let fill = LIMITS.op_bytes as usize - op_bytes(&make(0), &schema()).unwrap();
    let exact = ClientMsg::Ops(vec![DecodedOp::Valid(make(fill))]);
    assert!(encode_client(&exact, &schema()).is_ok());
    let over = ClientMsg::Ops(vec![
        DecodedOp::Valid(op_with(2, OpBody::Drop)),
        DecodedOp::Valid(make(fill + 1)),
    ]);
    assert_eq!(
        encode_client(&over, &schema()),
        Err(WireError::OpTooLarge {
            op: op_id(1),
            bytes: LIMITS.op_bytes as usize + 1
        })
    );
}

// -------------------------------------------------------------------------------------------
// A body with several kinds (D147, AC-55)

#[test]
fn a_body_with_several_kinds_makes_only_that_op_invalid() {
    let tag = op_id(1).to_hex();
    let owner = row().to_hex();
    for body in [
        // The hijack of `ac_55_hijack_one_op_both_directions`: add and remove in one op.
        format!(
            r#"{{"add":{{"field":"tags","elem":"x"}},"remove":{{"field":"tags","elem":"y","tags":["{tag}"]}}}}"#
        ),
        format!(
            r#"{{"put":{{"field":"roles","key":"{owner}","value":1}},"remove_key":{{"field":"roles","key":"{owner}"}}}}"#
        ),
        r#"{"drop":{},"make":{}}"#.to_owned(),
        r#"{"drop":{},"set":{"field":"count","value":1},"add":{"field":"tags","elem":"x"}}"#
            .to_owned(),
    ] {
        assert_eq!(
            decode_text(&with_body(&body)),
            invalid(OpFault::SeveralKinds),
            "{body}"
        );
    }
}

#[test]
fn several_kinds_leave_the_rest_of_the_batch_and_the_connection_alone() {
    let good = encode_op(&op_with(1, OpBody::Drop), &schema()).unwrap();
    let both = encode_op(&op_with(2, OpBody::Drop), &schema())
        .unwrap()
        .replace(
            r#"{"drop":{}}"#,
            r#"{"add":{"field":"tags","elem":"x"},"drop":{}}"#,
        );
    let later = encode_op(&op_with(3, OpBody::Drop), &schema()).unwrap();
    assert_eq!(
        decode_ops_frame(&ops_frame(&[good, both, later])),
        Ok(ClientMsg::Ops(vec![
            DecodedOp::Valid(op_with(1, OpBody::Drop)),
            DecodedOp::Invalid {
                id: op_id(2),
                hlc: stamp(),
                row: row(),
                fault: OpFault::SeveralKinds,
            },
            DecodedOp::Valid(op_with(3, OpBody::Drop)),
        ]))
    );
}

#[test]
fn several_kinds_stay_fatal_when_one_is_unknown_or_malformed() {
    let add = r#""add":{"field":"tags","elem":"x"}"#;
    for (body, want) in [
        (
            format!(r#"{{{add},"move":{{}}}}"#),
            WireError::WrongType("body"),
        ),
        (
            format!(r#"{{{add},"drop":[]}}"#),
            WireError::WrongType("body"),
        ),
        (
            format!(r#"{{{add},"remove":{{"field":"tags","elem":"x"}}}}"#),
            WireError::MissingKey("tags"),
        ),
        (
            format!(r#"{{{add},"set":{{"field":"count","value":1,"x":1}}}}"#),
            WireError::UnknownKey("x".into()),
        ),
    ] {
        assert_eq!(decode_text(&with_body(&body)), Err(want), "{body}");
    }
}
