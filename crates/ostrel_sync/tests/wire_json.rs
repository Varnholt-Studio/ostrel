//! JSON frames of the sync protocol: round trips per message, the frame limit in both
//! directions, and hostile JSON (depth, repeated keys, numbers outside the safe range).

use ostrel_core::canon::encode;
use ostrel_core::ids::{Hlc, InvalidId, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::Value;
use ostrel_sync::protocol::{
    Ack, CallError, ClientMsg, ErrorCode, Hello, LIMITS, ProtocolError, Query, RejectReason,
    ServerMsg, Welcome,
};
use ostrel_sync::wire::json::{self, Json};
use ostrel_sync::wire::{
    MAX_DEPTH, MAX_ECHO_BYTES, WireError, decode_client, decode_server, encode_client,
    encode_server,
};

const FRAME: usize = LIMITS.frame_bytes as usize;

fn replica() -> ReplicaId {
    ReplicaId(0x0123_4567_89ab_cdef)
}

fn op(seq: u32) -> OpId {
    OpId {
        replica: replica(),
        seq,
    }
}

fn stamp(wall_ms: u64) -> Hlc {
    Hlc::new(wall_ms, 7, replica()).unwrap()
}

fn row(wall_ms: u64) -> RowId {
    RowId::new(stamp(wall_ms))
}

fn client_round_trip(msg: ClientMsg) {
    let frame = encode_client(&msg).unwrap();
    assert_eq!(decode_client(frame.as_bytes()), Ok(msg), "frame {frame}");
}

fn server_round_trip(msg: ServerMsg) {
    let frame = encode_server(&msg).unwrap();
    assert_eq!(decode_server(frame.as_bytes()), Ok(msg), "frame {frame}");
}

fn client(text: &str) -> Result<ClientMsg, WireError> {
    decode_client(text.as_bytes())
}

fn server(text: &str) -> Result<ServerMsg, WireError> {
    decode_server(text.as_bytes())
}

// -------------------------------------------------------------------------------------------
// Round trips

#[test]
fn hello_round_trips_with_and_without_optional_fields() {
    client_round_trip(ClientMsg::Hello(Hello {
        token: None,
        replica: None,
        replica_secret: None,
        last_server_seq: None,
    }));
    client_round_trip(ClientMsg::Hello(Hello {
        token: Some("v4.public.eyJzdWIiOiJ1In0".into()),
        replica: Some(replica()),
        replica_secret: Some("00112233445566778899aabbccddeeff".into()),
        last_server_seq: Some(ServerSeq(u64::MAX)),
    }));
}

#[test]
fn small_client_messages_round_trip() {
    client_round_trip(ClientMsg::Unsubscribe { sub: u32::MAX });
    client_round_trip(ClientMsg::Held {
        sub: 3,
        rows: vec![row(1), row(2)],
        last: false,
    });
    client_round_trip(ClientMsg::Held {
        sub: 0,
        rows: Vec::new(),
        last: true,
    });
}

#[test]
fn welcome_round_trips_with_the_limits_table() {
    server_round_trip(ServerMsg::Welcome(Welcome {
        replica: replica(),
        replica_secret: Some("00112233445566778899aabbccddeeff".into()),
        server_ms: 1_760_000_000_000,
        limits: LIMITS,
    }));
    server_round_trip(ServerMsg::Welcome(Welcome {
        replica: ReplicaId(0),
        replica_secret: None,
        server_ms: 0,
        limits: LIMITS,
    }));
}

#[test]
fn small_server_messages_round_trip() {
    server_round_trip(ServerMsg::Ack(Ack {
        op: op(9),
        seq: ServerSeq(42),
        restamped: vec![(op(8), stamp(5)), (op(9), stamp(6))],
    }));
    server_round_trip(ServerMsg::Ack(Ack {
        op: op(1),
        seq: ServerSeq(1),
        restamped: Vec::new(),
    }));
    for reason in RejectReason::ALL {
        server_round_trip(ServerMsg::Reject { op: op(2), reason });
    }
    server_round_trip(ServerMsg::Defer {
        op: op(3),
        retry_after_ms: 1_000,
    });
    server_round_trip(ServerMsg::Evict {
        sub: 1,
        model: 4,
        rows: vec![row(10)],
    });
    server_round_trip(ServerMsg::Reset { sub: 12 });
}

#[test]
fn every_error_code_round_trips_with_and_without_subscription() {
    let codes = [
        ErrorCode::Protocol,
        ErrorCode::FrameTooLarge,
        ErrorCode::Unauthorized,
        ErrorCode::ReplicaExpired,
        ErrorCode::TooManySubscriptions,
        ErrorCode::FilterTooLarge,
        ErrorCode::FilterNotReadable,
    ];
    for code in codes {
        for sub in [None, Some(5)] {
            server_round_trip(ServerMsg::Error(ProtocolError { sub, code }));
        }
    }
}

#[test]
fn encoded_frames_are_canonical() {
    assert_eq!(
        encode_server(&ServerMsg::Reset { sub: 7 }).unwrap(),
        r#"{"sub":7,"t":"reset","v":0}"#
    );
    assert_eq!(
        encode_server(&ServerMsg::Reject {
            op: op(2),
            reason: RejectReason::Forged
        })
        .unwrap(),
        r#"{"op":"0123456789abcdef00000002","reason":"Forged","t":"reject","v":0}"#
    );
    assert_eq!(
        encode_client(&ClientMsg::Hello(Hello {
            token: None,
            replica: Some(replica()),
            replica_secret: None,
            last_server_seq: None,
        }))
        .unwrap(),
        r#"{"last_server_seq":null,"replica":"0123456789abcdef","replica_secret":null,"t":"hello","token":null,"v":0}"#
    );
}

#[test]
fn key_order_and_whitespace_of_received_frames_do_not_matter() {
    assert_eq!(
        server(" { \"v\" : 0 ,\n\"sub\":7, \"t\":\"reset\" } "),
        Ok(ServerMsg::Reset { sub: 7 })
    );
}

// -------------------------------------------------------------------------------------------
// Frame limit, both directions (D121)

#[test]
fn a_received_frame_of_exactly_the_limit_is_read() {
    let head = r#"{"sub":7,"t":"reset","v":0}"#;
    let mut frame = String::from(head);
    frame.insert_str(1, &" ".repeat(FRAME - head.len()));
    assert_eq!(frame.len(), FRAME);
    assert_eq!(server(&frame), Ok(ServerMsg::Reset { sub: 7 }));
}

#[test]
fn a_received_frame_over_the_limit_is_refused_before_parsing() {
    // Not JSON at all: the size check must come first.
    let frame = vec![b'x'; FRAME + 1];
    let expected = Err(WireError::FrameTooLarge { bytes: FRAME + 1 });
    assert_eq!(decode_server(&frame), expected);
    let client_side = decode_client(&frame);
    assert_eq!(
        client_side,
        Err(WireError::FrameTooLarge { bytes: FRAME + 1 })
    );
    assert_eq!(client_side.unwrap_err().code(), ErrorCode::FrameTooLarge);
}

#[test]
fn a_frame_over_the_limit_is_never_written() {
    let huge = ClientMsg::Hello(Hello {
        token: Some("a".repeat(FRAME)),
        replica: None,
        replica_secret: None,
        last_server_seq: None,
    });
    assert!(matches!(
        encode_client(&huge),
        Err(WireError::FrameTooLarge { bytes }) if bytes > FRAME
    ));
    let rows = vec![row(1); FRAME / 32];
    let evict = ServerMsg::Evict {
        sub: 1,
        model: 0,
        rows,
    };
    assert!(matches!(
        encode_server(&evict),
        Err(WireError::FrameTooLarge { .. })
    ));
}

// -------------------------------------------------------------------------------------------
// Hostile JSON

fn nested(depth: usize) -> String {
    format!(
        r#"{{"sub":7,"t":"reset","v":0,"x":{}{}}}"#,
        "[".repeat(depth),
        "]".repeat(depth)
    )
}

#[test]
fn nesting_up_to_the_depth_limit_parses() {
    // The frame object is one level, so the extra key may hold MAX_DEPTH - 1 arrays; the frame
    // then fails only for its unknown key.
    assert_eq!(
        server(&nested(MAX_DEPTH - 1)),
        Err(WireError::UnknownKey("x".into()))
    );
}

#[test]
fn nesting_over_the_depth_limit_is_too_deep() {
    assert_eq!(server(&nested(MAX_DEPTH)), Err(WireError::TooDeep));
}

#[test]
fn very_deep_nesting_fails_without_exhausting_the_stack() {
    let open = "[".repeat(FRAME / 2);
    assert_eq!(client(&open), Err(WireError::TooDeep));
    let objects = r#"{"a":"#.repeat(FRAME / 5);
    assert_eq!(client(&objects), Err(WireError::TooDeep));
}

#[test]
fn repeated_keys_are_refused_at_every_level() {
    assert_eq!(
        server(r#"{"sub":7,"sub":8,"t":"reset","v":0}"#),
        Err(WireError::DuplicateKey("sub".into()))
    );
    assert_eq!(
        server(r#"{"v":0,"v":0,"t":"reset","sub":7}"#),
        Err(WireError::DuplicateKey("v".into()))
    );
    // Inside a nested object, and the same key in sibling objects is fine.
    let mut welcome = encode_server(&ServerMsg::Welcome(Welcome {
        replica: replica(),
        replica_secret: None,
        server_ms: 1,
        limits: LIMITS,
    }))
    .unwrap();
    let at = welcome.find(r#""bulk_rows":"#).unwrap();
    welcome.insert_str(at, r#""bulk_rows":1,"#);
    assert_eq!(
        server(&welcome),
        Err(WireError::DuplicateKey("bulk_rows".into()))
    );
    assert_eq!(
        server(r#"{"sub":7,"t":"reset","v":0,"x":[{"a":1},{"a":2}]}"#),
        Err(WireError::UnknownKey("x".into()))
    );
}

#[test]
fn an_object_with_many_distinct_keys_is_read() {
    // The duplicate check uses a set (json.rs), so this frame stays linear; a scan of the
    // earlier keys would compare about 1.8e9 pairs here.
    let keys: Vec<String> = (0..60_000).map(|i| format!(r#""k{i}":0"#)).collect();
    let frame = format!(
        r#"{{"sub":7,"t":"reset","v":0,"x":{{{}}}}}"#,
        keys.join(",")
    );
    assert!(frame.len() <= FRAME);
    assert_eq!(server(&frame), Err(WireError::UnknownKey("x".into())));
}

#[test]
fn integers_outside_the_safe_range_are_refused() {
    // Whatever the form: beyond u64 (serde_json hands those over as floats) and with an
    // exponent count the same as a plain integer token.
    for number in [
        "9007199254740992",
        "-9007199254740992",
        "18446744073709551615",
        "18446744073709551616",
        "-18446744073709551617",
        "-9223372036854775808",
        "100000000000000000000000000000",
        "1e20",
        "-1e20",
        "9007199254740992.0",
        "1.5e300",
    ] {
        let frame = format!(r#"{{"sub":{number},"t":"reset","v":0}}"#);
        assert_eq!(server(&frame), Err(WireError::NumberOutOfRange), "{number}");
        let frame = format!(r#"{{"sub":1,"t":"reset","v":{number}}}"#);
        assert_eq!(
            server(&frame),
            Err(WireError::NumberOutOfRange),
            "v {number}"
        );
    }
}

#[test]
fn numbers_are_checked_by_the_type_of_their_place() {
    // A number is range checked where its type is known (dora #1738 F2): the canonical
    // encoding writes some floats as integer tokens, so a bare token outside the safe range
    // is not an error by itself. In a key the message does not have, the key is the fault.
    for number in ["9007199254740992", "18446744073709551616", "1e20"] {
        let frame = format!(r#"{{"sub":1,"t":"reset","v":0,"x":[{number}]}}"#);
        assert_eq!(
            server(&frame),
            Err(WireError::UnknownKey("x".into())),
            "{number}"
        );
    }
}

#[test]
fn every_canonical_float_reads_back_exactly() {
    // canon::encode -> json::parse is the identity on every finite float (dora #1738 F2),
    // also where the encoding is an integer token beyond the safe range or beyond u64.
    let mut edge = vec![
        0.0,
        -0.0,
        2f64.powi(53),
        -(2f64.powi(53)),
        2f64.powi(64),
        1e20,
        1e21,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        5e-324,
        0.1 + 0.2,
        123_456_789_012_345_680.0,
    ];
    // Plus a fixed pseudo random sample over all bit patterns (xorshift, seed fixed).
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..20_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        edge.push(f64::from_bits(x));
    }
    for v in edge.into_iter().filter(|v| v.is_finite()) {
        let text = encode(&Value::float(v).unwrap());
        let Ok(Json::Num(n)) = json::parse(text.as_bytes()) else {
            panic!("{text} did not parse as a number");
        };
        // -0 is written as 0 (ARCHITECTURE 5.3).
        let want = if v == 0.0 { 0.0 } else { v };
        assert_eq!(n.value().to_bits(), want.to_bits(), "{text}");
    }
}

#[test]
fn the_edges_of_the_safe_range_parse_but_must_fit_their_field() {
    assert_eq!(
        server(r#"{"sub":9007199254740991,"t":"reset","v":0}"#),
        Err(WireError::FieldRange("sub"))
    );
    assert_eq!(
        server(r#"{"sub":-1,"t":"reset","v":0}"#),
        Err(WireError::FieldRange("sub"))
    );
    assert_eq!(
        server(r#"{"sub":4294967295,"t":"reset","v":0}"#),
        Ok(ServerMsg::Reset { sub: u32::MAX })
    );
    let max_ms = r#"{"limits":LIMITS,"replica":"0000000000000000","replica_secret":null,"server_ms":9007199254740991,"t":"welcome","v":0}"#;
    let limits = encode_server(&ServerMsg::Welcome(Welcome {
        replica: ReplicaId(0),
        replica_secret: None,
        server_ms: 0,
        limits: LIMITS,
    }))
    .unwrap();
    let limits = &limits[limits.find('{').unwrap() + 10..limits.find('}').unwrap() + 1];
    let frame = max_ms.replace("LIMITS", limits);
    assert!(matches!(
        server(&frame),
        Ok(ServerMsg::Welcome(Welcome {
            server_ms: 9_007_199_254_740_991,
            ..
        }))
    ));
}

#[test]
fn a_server_time_above_the_safe_range_is_not_written() {
    let welcome = ServerMsg::Welcome(Welcome {
        replica: replica(),
        replica_secret: None,
        server_ms: 1 << 53,
        limits: LIMITS,
    });
    assert_eq!(
        encode_server(&welcome),
        Err(WireError::FieldRange("server_ms"))
    );
}

#[test]
fn numbers_with_fraction_or_exponent_are_not_integers() {
    for number in ["7.0", "7e0", "0.5", "-0.0"] {
        let frame = format!(r#"{{"sub":{number},"t":"reset","v":0}}"#);
        assert_eq!(server(&frame), Err(WireError::WrongType("sub")), "{number}");
    }
    // A float too large for f64 is malformed JSON.
    assert!(matches!(
        server(r#"{"sub":1e400,"t":"reset","v":0}"#),
        Err(WireError::Malformed(_))
    ));
}

#[test]
fn malformed_text_is_refused() {
    for text in [
        "",
        "{",
        r#"{"sub":7,"t":"reset","v":0} x"#,
        r#"{"sub":7,"t":"reset","v":0,}"#,
        "\u{feff}{\"sub\":7,\"t\":\"reset\",\"v\":0}",
        r#"{"sub":07,"t":"reset","v":0}"#,
        r#"{'sub':7}"#,
        r#"{"sub":NaN,"t":"reset","v":0}"#,
    ] {
        assert!(
            matches!(server(text), Err(WireError::Malformed(_))),
            "{text:?}"
        );
    }
    let mut bytes = br#"{"sub":7,"t":"reset","v":0,"x":"#.to_vec();
    bytes.extend_from_slice(b"\"\xff\"}");
    assert!(matches!(
        decode_server(&bytes),
        Err(WireError::Malformed(_))
    ));
}

#[test]
fn unpaired_surrogates_are_refused() {
    let frame = r#"{"last_server_seq":null,"replica":null,"replica_secret":null,"t":"hello","token":"\ud800","v":0}"#;
    assert!(matches!(client(frame), Err(WireError::Malformed(_))));
    let paired = frame.replace(r"\ud800", r"😀");
    assert!(matches!(
        client(&paired),
        Ok(ClientMsg::Hello(Hello { token: Some(t), .. })) if t == "\u{1f600}"
    ));
}

// -------------------------------------------------------------------------------------------
// Envelope and fields

#[test]
fn the_envelope_is_checked_before_the_fields() {
    assert_eq!(server("[]"), Err(WireError::NotAnObject));
    assert_eq!(server("7"), Err(WireError::NotAnObject));
    assert_eq!(
        server(r#"{"t":"reset","sub":7}"#),
        Err(WireError::MissingKey("v"))
    );
    assert_eq!(
        server(r#"{"v":1,"t":"reset","sub":7}"#),
        Err(WireError::UnsupportedVersion)
    );
    assert_eq!(
        server(r#"{"v":"0","t":"reset","sub":7}"#),
        Err(WireError::WrongType("v"))
    );
    // Another version may have other messages: the version decides first.
    assert_eq!(
        server(r#"{"v":1,"t":"future"}"#),
        Err(WireError::UnsupportedVersion)
    );
    assert_eq!(
        server(r#"{"v":0,"sub":7}"#),
        Err(WireError::MissingKey("t"))
    );
    assert_eq!(
        server(r#"{"v":0,"t":7,"sub":7}"#),
        Err(WireError::WrongType("t"))
    );
}

#[test]
fn message_names_are_per_direction() {
    assert_eq!(
        client(r#"{"v":0,"t":"reset","sub":7}"#),
        Err(WireError::UnknownMessage("reset".into()))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"unsubscribe","sub":7}"#),
        Err(WireError::UnknownMessage("unsubscribe".into()))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"Reset","sub":7}"#),
        Err(WireError::UnknownMessage("Reset".into()))
    );
}

#[test]
fn every_key_is_required_and_no_other_key_is_allowed() {
    // An absent optional value is null, never a missing key.
    assert_eq!(
        client(r#"{"v":0,"t":"hello","token":null,"replica":null,"replica_secret":null}"#),
        Err(WireError::MissingKey("last_server_seq"))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"reset","sub":7,"extra":true}"#),
        Err(WireError::UnknownKey("extra".into()))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"error","code":"Protocol"}"#),
        Err(WireError::MissingKey("sub"))
    );
}

#[test]
fn ids_must_be_strict_hex_of_their_width() {
    let frame = |op: &str| format!(r#"{{"v":0,"t":"reject","op":"{op}","reason":"Denied"}}"#);
    assert_eq!(
        server(&frame("0123456789ABCDEF00000002")),
        Err(WireError::InvalidId("op", InvalidId::Char { offset: 10 }))
    );
    assert_eq!(
        server(&frame("0123456789abcdef0000002")),
        Err(WireError::InvalidId(
            "op",
            InvalidId::Width {
                expected: 24,
                found: 23
            }
        ))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"reject","op":2,"reason":"Denied"}"#),
        Err(WireError::WrongType("op"))
    );
    assert_eq!(
        client(r#"{"v":0,"t":"held","sub":1,"rows":["00"],"last":true}"#),
        Err(WireError::InvalidId(
            "rows",
            InvalidId::Width {
                expected: 32,
                found: 2
            }
        ))
    );
}

#[test]
fn names_and_pairs_are_checked() {
    assert_eq!(
        server(r#"{"v":0,"t":"reject","op":"0123456789abcdef00000002","reason":"denied"}"#),
        Err(WireError::WrongType("reason"))
    );
    assert_eq!(
        server(r#"{"v":0,"t":"error","sub":null,"code":"Teapot"}"#),
        Err(WireError::WrongType("code"))
    );
    let ack = |restamped: &str| {
        format!(
            r#"{{"v":0,"t":"ack","op":"0123456789abcdef00000002","seq":"0000000000000001","restamped":{restamped}}}"#
        )
    };
    let pair = format!(r#"["0123456789abcdef00000002","{}"]"#, stamp(5).to_hex());
    assert!(server(&ack(&format!("[{pair}]"))).is_ok());
    assert_eq!(
        server(&ack(r#"[["0123456789abcdef00000002"]]"#)),
        Err(WireError::WrongType("restamped"))
    );
    assert_eq!(
        server(&ack(&format!(r#"[{}]"#, pair.replace(']', r#","x"]"#)))),
        Err(WireError::WrongType("restamped"))
    );
    assert_eq!(
        server(&ack(r#"{}"#)),
        Err(WireError::WrongType("restamped"))
    );
}

#[test]
fn a_missing_or_extra_limit_is_refused() {
    let welcome = encode_server(&ServerMsg::Welcome(Welcome {
        replica: replica(),
        replica_secret: None,
        server_ms: 1,
        limits: LIMITS,
    }))
    .unwrap();
    let without = welcome.replace(r#""bulk_rows":1000,"#, "");
    assert_eq!(server(&without), Err(WireError::MissingKey("bulk_rows")));
    let extra = welcome.replace(r#""bulk_rows":1000,"#, r#""bulk_rows":1000,"row_cap":1,"#);
    assert_eq!(server(&extra), Err(WireError::UnknownKey("row_cap".into())));
}

#[test]
fn errors_echo_at_most_a_short_prefix_of_hostile_names() {
    let long = "\u{e9}".repeat(1_000);
    let frame = format!(r#"{{"v":0,"t":"reset","sub":7,"{long}":1}}"#);
    let Err(WireError::UnknownKey(echo)) = server(&frame) else {
        panic!("expected an unknown key");
    };
    assert!(echo.len() <= MAX_ECHO_BYTES);
    assert!(long.starts_with(&echo));
    let frame = format!(r#"{{"v":0,"t":"{long}"}}"#);
    let Err(WireError::UnknownMessage(echo)) = server(&frame) else {
        panic!("expected an unknown message");
    };
    assert!(echo.len() <= MAX_ECHO_BYTES);
}

#[test]
fn every_error_but_the_frame_limit_is_a_protocol_error() {
    assert_eq!(WireError::TooDeep.code(), ErrorCode::Protocol);
    assert_eq!(WireError::NumberOutOfRange.code(), ErrorCode::Protocol);
    assert_eq!(
        WireError::DuplicateKey("v".into()).code(),
        ErrorCode::Protocol
    );
    assert_eq!(
        WireError::FrameTooLarge { bytes: FRAME + 1 }.code(),
        ErrorCode::FrameTooLarge
    );
}

// -------------------------------------------------------------------------------------------
// Messages with values are not covered yet

#[test]
fn messages_with_values_queries_or_rows_are_unsupported_for_now() {
    let unsupported_client = [
        ClientMsg::Subscribe {
            sub: 1,
            query: Query::all(0),
        },
        ClientMsg::Ops(Vec::new()),
        ClientMsg::Call {
            call: 1,
            function: 0,
            args: Vec::new(),
        },
    ];
    for msg in unsupported_client {
        assert_eq!(encode_client(&msg), Err(WireError::Unsupported(msg.name())));
        let frame = format!(r#"{{"v":0,"t":"{}"}}"#, msg.name());
        assert_eq!(client(&frame), Err(WireError::Unsupported(msg.name())));
    }
    let unsupported_server = [
        ServerMsg::Snapshot {
            sub: 1,
            rows: Vec::new(),
            seq: ServerSeq(0),
            last: true,
        },
        ServerMsg::Ops(Vec::new()),
        ServerMsg::Return {
            call: 1,
            result: Err(CallError::Denied),
        },
    ];
    for msg in unsupported_server {
        assert_eq!(encode_server(&msg), Err(WireError::Unsupported(msg.name())));
        let frame = format!(r#"{{"v":0,"t":"{}"}}"#, msg.name());
        assert_eq!(server(&frame), Err(WireError::Unsupported(msg.name())));
    }
}
