//! Tests of the browser backend: the emitted module is loaded in Node 22 next
//! to the repository's `runtime/js` and its functions are called.

// Test code may panic on a broken invariant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use ostrel_codegen_js::{APP_MODULE, EmitError, JsBundle, emit};
use ostrel_core::{FileId, Span};
use ostrel_ir::full::{
    App, BinOp, Block, Const, Enum, EnumId, Field, FieldId, FieldMode, FieldRef, FnKind, FuncId,
    Function, ImplicitField, Inst, InstKind, Local, Merge, Model, ModelId, Placement, Program,
    Registration, Terminator, Ty, UnOp,
};

fn span(at: u32) -> Span {
    Span::new(FileId::from_raw(0), at, at + 1)
}

fn inst(at: u32, kind: InstKind) -> Inst {
    Inst {
        kind,
        span: span(at),
    }
}

fn block(insts: Vec<Inst>, term: Terminator) -> Block {
    Block {
        insts,
        term,
        term_span: span(0),
    }
}

fn function(
    placement: Placement,
    params: u32,
    locals: Vec<Ty>,
    result: Ty,
    blocks: Vec<Block>,
) -> Function {
    Function {
        name: "f".to_string(),
        placement,
        kind: FnKind::Source,
        elevated: false,
        params,
        locals,
        result,
        prologue: None,
        blocks,
        span: span(0),
    }
}

/// `fn(a: Int, b: Int) -> Int { a op b }`, shared.
fn int_binary(op: BinOp) -> Function {
    function(
        Placement::Shared,
        2,
        vec![Ty::Int, Ty::Int, Ty::Int],
        Ty::Int,
        vec![block(
            vec![inst(
                1,
                InstKind::Binary {
                    dst: Local(2),
                    op,
                    lhs: Local(0),
                    rhs: Local(1),
                },
            )],
            Terminator::Return(Some(Local(2))),
        )],
    )
}

/// `fn(a: T, b: T) -> Bool { a op b }`, shared.
fn compare(ty: Ty, op: BinOp) -> Function {
    function(
        Placement::Shared,
        2,
        vec![ty.clone(), ty, Ty::Bool],
        Ty::Bool,
        vec![block(
            vec![inst(
                1,
                InstKind::Binary {
                    dst: Local(2),
                    op,
                    lhs: Local(0),
                    rhs: Local(1),
                },
            )],
            Terminator::Return(Some(Local(2))),
        )],
    )
}

fn program(functions: Vec<Function>) -> Program {
    Program {
        functions,
        ..Program::default()
    }
}

static RUN: AtomicU32 = AtomicU32::new(0);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Writes the bundle into a fresh directory beside the runtime it imports and
/// runs `script` there as an ES module; `app` is the emitted module. Returns
/// stdout.
fn run_node(bundle: &JsBundle, script: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "ostrel_codegen_js_{}_{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &bundle.files {
        let path = dir.join(&file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &file.text).unwrap();
    }
    std::os::unix::fs::symlink(repo_root().join("runtime/js"), dir.join("runtime")).unwrap();
    let main = dir.join("main.mjs");
    std::fs::write(
        &main,
        format!(
            "import * as app from './{APP_MODULE}';\n\
             const out = [];\n\
             function show(v) {{ return Object.is(v, -0) ? '-0' : JSON.stringify(v); }}\n\
             function attempt(f) {{\n\
               try {{ out.push(show(f())); }}\n\
               catch (e) {{ out.push(e instanceof app.OstrelError ? 'error ' + e.kind : 'throw ' + e); }}\n\
             }}\n\
             {script}\n\
             console.log(out.join('\\n'));\n"
        ),
    )
    .unwrap();
    let output = Command::new("node")
        .arg(&main)
        .output()
        .expect("node 22 must be installed (ARCHITECTURE 11)");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn emitted(program: &Program) -> JsBundle {
    match emit(program) {
        Ok(bundle) => bundle,
        Err(error) => panic!("emit failed: {error:?}"),
    }
}

fn app_text(bundle: &JsBundle) -> &str {
    &bundle
        .files
        .iter()
        .find(|f| f.path == APP_MODULE)
        .unwrap()
        .text
}

#[test]
fn int_division_truncates_and_remainder_takes_the_dividend_sign() {
    let bundle = emitted(&program(vec![
        int_binary(BinOp::Div),
        int_binary(BinOp::Rem),
    ]));
    let out = run_node(
        &bundle,
        "attempt(() => app.f0(7, 2));\n\
         attempt(() => app.f0(-7, 2));\n\
         attempt(() => app.f1(-7, 2));\n\
         attempt(() => app.f1(7, -2));\n\
         attempt(() => app.f0(-9007199254740991, -1));\n\
         attempt(() => app.f0(1, 0));\n\
         attempt(() => app.f1(1, 0));\n\
         attempt(() => app.f0(0, -5));\n\
         attempt(() => app.f1(-4, 2));\n\
         attempt(() => app.f0(9007199254740991, 3));\n\
         attempt(() => app.f1(9007199254740991, 2));",
    );
    assert_eq!(
        out,
        "3\n-3\n-1\n1\n9007199254740991\nerror DivisionByZero\nerror DivisionByZero\n0\n0\n3002399751580330\n1\n"
    );
}

#[test]
fn int_arithmetic_is_checked_and_never_gives_negative_zero() {
    let bundle = emitted(&program(vec![
        int_binary(BinOp::Add),
        int_binary(BinOp::Sub),
        int_binary(BinOp::Mul),
    ]));
    let out = run_node(
        &bundle,
        "attempt(() => app.f0(9007199254740991, 0));\n\
         attempt(() => app.f0(9007199254740991, 1));\n\
         attempt(() => app.f1(-9007199254740991, 1));\n\
         attempt(() => app.f1(-9007199254740991, -1));\n\
         attempt(() => app.f2(-5, 0));\n\
         attempt(() => app.f2(94906267, 94906267));\n\
         attempt(() => app.f2(94906265, 94906265));\n\
         attempt(() => app.f2(-9007199254740991, -9007199254740991));",
    );
    assert_eq!(
        out,
        "9007199254740991\nerror IntOverflow\nerror IntOverflow\n-9007199254740990\n0\nerror IntOverflow\n9007199136250225\nerror IntOverflow\n"
    );
}

#[test]
fn int_negation_of_zero_is_zero() {
    let neg = function(
        Placement::Shared,
        1,
        vec![Ty::Int, Ty::Int],
        Ty::Int,
        vec![block(
            vec![inst(
                1,
                InstKind::Unary {
                    dst: Local(1),
                    op: UnOp::Neg,
                    arg: Local(0),
                },
            )],
            Terminator::Return(Some(Local(1))),
        )],
    );
    let out = run_node(
        &emitted(&program(vec![neg])),
        "attempt(() => app.f0(0));\nattempt(() => app.f0(-9007199254740991));",
    );
    assert_eq!(out, "0\n9007199254740991\n");
}

#[test]
fn text_order_is_by_code_point() {
    // U+FF01 is below U+1F600 by code point, above it by UTF-16 unit (D50).
    let bundle = emitted(&program(vec![
        compare(Ty::Text, BinOp::Lt),
        compare(Ty::Text, BinOp::Ge),
        compare(Ty::Text, BinOp::Eq),
        compare(Ty::Rank, BinOp::Gt),
    ]));
    let out = run_node(
        &bundle,
        "attempt(() => app.f0('\\uFF01', '\\u{1F600}'));\n\
         attempt(() => app.f1('\\uFF01', '\\u{1F600}'));\n\
         attempt(() => app.f2('a', 'a'));\n\
         attempt(() => app.f2('a', 'b'));\n\
         attempt(() => app.f3('\\u{1F600}', '\\uFF01'));",
    );
    assert_eq!(out, "true\nfalse\ntrue\nfalse\ntrue\n");
}

#[test]
fn enum_order_is_declaration_order() {
    let mut p = program(vec![compare(Ty::Enum(EnumId(0)), BinOp::Lt)]);
    p.enums.push(Enum {
        name: "Size".to_string(),
        variants: vec![
            "small".to_string(),
            "large".to_string(),
            "medium".to_string(),
        ],
    });
    let out = run_node(
        &emitted(&p),
        "attempt(() => app.f0('small', 'large'));\n\
         attempt(() => app.f0('large', 'medium'));\n\
         attempt(() => app.f0('medium', 'small'));",
    );
    assert_eq!(out, "true\ntrue\nfalse\n");
}

#[test]
fn branches_and_jumps_run_a_loop() {
    // fn sum(n: Int) -> Int { var s = 0; var i = 1; while i <= n { s = s + i; i = i + 1 }; s }
    let sum = function(
        Placement::Client,
        1,
        vec![Ty::Int, Ty::Int, Ty::Int, Ty::Int, Ty::Bool],
        Ty::Int,
        vec![
            block(
                vec![
                    inst(
                        1,
                        InstKind::Const {
                            dst: Local(1),
                            value: Const::Int(0),
                        },
                    ),
                    inst(
                        2,
                        InstKind::Const {
                            dst: Local(2),
                            value: Const::Int(1),
                        },
                    ),
                    inst(
                        3,
                        InstKind::Const {
                            dst: Local(3),
                            value: Const::Int(1),
                        },
                    ),
                ],
                Terminator::Jump(ostrel_ir::full::BlockId(1)),
            ),
            block(
                vec![inst(
                    4,
                    InstKind::Binary {
                        dst: Local(4),
                        op: BinOp::Le,
                        lhs: Local(2),
                        rhs: Local(0),
                    },
                )],
                Terminator::Branch {
                    cond: Local(4),
                    then: ostrel_ir::full::BlockId(2),
                    otherwise: ostrel_ir::full::BlockId(3),
                },
            ),
            block(
                vec![
                    inst(
                        5,
                        InstKind::Binary {
                            dst: Local(1),
                            op: BinOp::Add,
                            lhs: Local(1),
                            rhs: Local(2),
                        },
                    ),
                    inst(
                        6,
                        InstKind::Binary {
                            dst: Local(2),
                            op: BinOp::Add,
                            lhs: Local(2),
                            rhs: Local(3),
                        },
                    ),
                ],
                Terminator::Jump(ostrel_ir::full::BlockId(1)),
            ),
            block(Vec::new(), Terminator::Return(Some(Local(1)))),
        ],
    );
    let out = run_node(
        &emitted(&program(vec![sum])),
        "attempt(() => app.f0(10));\nattempt(() => app.f0(0));",
    );
    assert_eq!(out, "55\n0\n");
}

#[test]
fn text_constants_round_trip_every_character() {
    let tricky = "quote \" back \\ nl \n tab \t ls \u{2028} ps \u{2029} nul \u{0} \
                  emoji \u{1F600} close </script> ${x} `tick` 'single'";
    let f = function(
        Placement::Shared,
        0,
        vec![Ty::Text],
        Ty::Text,
        vec![block(
            vec![inst(
                1,
                InstKind::Const {
                    dst: Local(0),
                    value: Const::Text(tricky.to_string()),
                },
            )],
            Terminator::Return(Some(Local(0))),
        )],
    );
    let out = run_node(&emitted(&program(vec![f])), "attempt(() => app.f0());");
    let expected = format!("{}\n", json_string(tricky));
    assert_eq!(out, expected);
}

/// `JSON.stringify` of a string, for the characters used above.
fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{0}' => out.push_str("\\u0000"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[test]
fn interpolation_and_optionals() {
    // fn label(n: Int, flag: Bool, name: Text?) -> Text
    //   "n={n} flag={flag} name={name ?? "none"}"
    let f = function(
        Placement::Client,
        3,
        vec![
            Ty::Int,                          // 0 n
            Ty::Bool,                         // 1 flag
            Ty::Optional(Box::new(Ty::Text)), // 2 name
            Ty::Text,                         // 3 n text
            Ty::Text,                         // 4 flag text
            Ty::Bool,                         // 5 has name
            Ty::Text,                         // 6 name or fallback
            Ty::Text,                         // 7 "n="
            Ty::Text,                         // 8 " flag="
            Ty::Text,                         // 9 " name="
            Ty::Text,                         // 10 result
        ],
        Ty::Text,
        vec![
            block(
                vec![
                    inst(
                        1,
                        InstKind::ToText {
                            dst: Local(3),
                            src: Local(0),
                        },
                    ),
                    inst(
                        2,
                        InstKind::ToText {
                            dst: Local(4),
                            src: Local(1),
                        },
                    ),
                    inst(
                        3,
                        InstKind::IsSome {
                            dst: Local(5),
                            src: Local(2),
                        },
                    ),
                ],
                Terminator::Branch {
                    cond: Local(5),
                    then: ostrel_ir::full::BlockId(1),
                    otherwise: ostrel_ir::full::BlockId(2),
                },
            ),
            block(
                vec![inst(
                    4,
                    InstKind::Unwrap {
                        dst: Local(6),
                        src: Local(2),
                    },
                )],
                Terminator::Jump(ostrel_ir::full::BlockId(3)),
            ),
            block(
                vec![inst(
                    5,
                    InstKind::Const {
                        dst: Local(6),
                        value: Const::Text("none".to_string()),
                    },
                )],
                Terminator::Jump(ostrel_ir::full::BlockId(3)),
            ),
            block(
                vec![
                    inst(
                        6,
                        InstKind::Const {
                            dst: Local(7),
                            value: Const::Text("n=".to_string()),
                        },
                    ),
                    inst(
                        7,
                        InstKind::Const {
                            dst: Local(8),
                            value: Const::Text(" flag=".to_string()),
                        },
                    ),
                    inst(
                        8,
                        InstKind::Const {
                            dst: Local(9),
                            value: Const::Text(" name=".to_string()),
                        },
                    ),
                    inst(
                        9,
                        InstKind::Concat {
                            dst: Local(10),
                            parts: vec![Local(7), Local(3), Local(8), Local(4), Local(9), Local(6)],
                        },
                    ),
                ],
                Terminator::Return(Some(Local(10))),
            ),
        ],
    );
    let out = run_node(
        &emitted(&program(vec![f])),
        "attempt(() => app.f0(-12, true, 'ann'));\nattempt(() => app.f0(0, false, null));",
    );
    assert_eq!(
        out,
        "\"n=-12 flag=true name=ann\"\n\"n=0 flag=false name=none\"\n"
    );
}

#[test]
fn some_none_and_optional_equality() {
    // fn(a: Int) -> Bool { let x: Int? = Some(a); let y: Int? = none; x == y }
    let opt = || Ty::Optional(Box::new(Ty::Int));
    let f = function(
        Placement::Shared,
        1,
        vec![Ty::Int, opt(), opt(), Ty::Bool],
        Ty::Bool,
        vec![block(
            vec![
                inst(
                    1,
                    InstKind::Some {
                        dst: Local(1),
                        src: Local(0),
                    },
                ),
                inst(
                    2,
                    InstKind::Const {
                        dst: Local(2),
                        value: Const::None,
                    },
                ),
                inst(
                    3,
                    InstKind::Binary {
                        dst: Local(3),
                        op: BinOp::Ne,
                        lhs: Local(1),
                        rhs: Local(2),
                    },
                ),
            ],
            Terminator::Return(Some(Local(3))),
        )],
    );
    let out = run_node(&emitted(&program(vec![f])), "attempt(() => app.f0(0));");
    assert_eq!(out, "true\n");
}

#[test]
fn text_limit_is_checked_on_concatenation() {
    // fn(t: Text) -> Text { "{t}{t}" }
    let f = function(
        Placement::Shared,
        1,
        vec![Ty::Text, Ty::Text],
        Ty::Text,
        vec![block(
            vec![inst(
                1,
                InstKind::Concat {
                    dst: Local(1),
                    parts: vec![Local(0), Local(0)],
                },
            )],
            Terminator::Return(Some(Local(1))),
        )],
    );
    // 8 388 608 bytes twice is exactly the limit; one more byte is over it.
    let out = run_node(
        &emitted(&program(vec![f])),
        "attempt(() => app.f0('a'.repeat(8388608)).length);\n\
         attempt(() => app.f0('a'.repeat(8388608) + 'b').length);\n\
         attempt(() => app.f0('\\u00e9'.repeat(4194304)).length);\n\
         attempt(() => app.f0('\\u00e9'.repeat(4194304) + 'b').length);",
    );
    assert_eq!(out, "16777216\nerror TextLimit\n8388608\nerror TextLimit\n");
}

#[test]
fn calls_reach_shared_functions_and_server_code_is_not_emitted() {
    let server = function(
        Placement::Server,
        0,
        Vec::new(),
        Ty::Unit,
        vec![block(Vec::new(), Terminator::Return(None))],
    );
    let double = int_binary(BinOp::Add);
    let caller = function(
        Placement::Client,
        1,
        vec![Ty::Int, Ty::Int],
        Ty::Int,
        vec![block(
            vec![inst(
                1,
                InstKind::Call {
                    dst: Some(Local(1)),
                    func: FuncId(1),
                    args: vec![Local(0), Local(0)],
                },
            )],
            Terminator::Return(Some(Local(1))),
        )],
    );
    let bundle = emitted(&program(vec![server, double, caller]));
    assert!(!app_text(&bundle).contains("function f0("));
    let out = run_node(
        &bundle,
        "attempt(() => app.f2(21));\nout.push(typeof app.f0);",
    );
    assert_eq!(out, "42\nundefined\n");
}

#[test]
fn schema_lists_app_enums_models_and_fields() {
    let mut p = program(Vec::new());
    p.app = Some(App {
        name: "Notes".to_string(),
        auth: Some(Registration::Open),
        home: None,
    });
    p.enums.push(Enum {
        name: "Mood".to_string(),
        variants: vec!["calm".to_string(), "busy".to_string()],
    });
    let field = |name: &str, ty: Ty, mode: FieldMode, merge: Merge| Field {
        name: name.to_string(),
        ty,
        mode,
        merge,
        default: None,
        span: span(0),
    };
    p.models.push(Model {
        name: "Note".to_string(),
        fields: vec![
            field("title", Ty::Text, FieldMode::Plain, Merge::Default),
            field("body", Ty::Text, FieldMode::Plain, Merge::Text),
            field(
                "mood",
                Ty::Optional(Box::new(Ty::Enum(EnumId(0)))),
                FieldMode::Fixed,
                Merge::Default,
            ),
            field(
                "tags",
                Ty::Set(Box::new(Ty::Text)),
                FieldMode::Plain,
                Merge::Default,
            ),
            field(
                "votes",
                Ty::Map(Box::new(Ty::Ref(ModelId(0))), Box::new(Ty::Int)),
                FieldMode::Server,
                Merge::Default,
            ),
            field(
                "number",
                Ty::Int,
                FieldMode::Serial(Some(FieldRef::Implicit(ImplicitField::Author))),
                Merge::Default,
            ),
            field(
                "page",
                Ty::Int,
                FieldMode::Serial(Some(FieldRef::Declared(FieldId(2)))),
                Merge::Default,
            ),
            field("seq", Ty::Int, FieldMode::Serial(None), Merge::Default),
            field(
                "parts",
                Ty::List(Box::new(Ty::Bytes)),
                FieldMode::Plain,
                Merge::Default,
            ),
        ],
        see: None,
        rules: Vec::new(),
        checks: Vec::new(),
        span: span(0),
    });
    let out = run_node(
        &emitted(&p),
        "out.push(JSON.stringify(app.schema));\n\
         out.push(String(Object.isFrozen(app.schema.models[0].fields[2].type)));",
    );
    assert_eq!(
        out,
        concat!(
            r#"{"app":{"name":"Notes","auth":"open"},"#,
            r#""enums":[{"name":"Mood","variants":["calm","busy"]}],"#,
            r#""models":[{"name":"Note","fields":["#,
            r#"{"name":"title","type":"Text","mode":"plain","merge":"default"},"#,
            r#"{"name":"body","type":"Text","mode":"plain","merge":"text"},"#,
            r#"{"name":"mood","type":{"optional":{"enum":0}},"mode":"fixed","merge":"default"},"#,
            r#"{"name":"tags","type":{"set":"Text"},"mode":"plain","merge":"default"},"#,
            r#"{"name":"votes","type":{"map":[{"ref":0},"Int"]},"mode":"server","merge":"default"},"#,
            r#"{"name":"number","type":"Int","mode":"serial","serialPer":"author","merge":"default"},"#,
            r#"{"name":"page","type":"Int","mode":"serial","serialPer":2,"merge":"default"},"#,
            r#"{"name":"seq","type":"Int","mode":"serial","serialPer":null,"merge":"default"},"#,
            r#"{"name":"parts","type":{"list":"Bytes"},"mode":"plain","merge":"default"}]}]}"#,
            "\ntrue\n"
        )
    );
}

#[test]
fn script_program_has_no_app() {
    let out = run_node(
        &emitted(&program(Vec::new())),
        "out.push(JSON.stringify(app.schema));",
    );
    assert_eq!(out, "{\"app\":null,\"enums\":[],\"models\":[]}\n");
}

#[test]
fn not_yet_supported_instructions_fail_the_emit() {
    let remote = function(
        Placement::Client,
        0,
        vec![Ty::Remote(Box::new(Ty::Unit))],
        Ty::Unit,
        vec![block(
            vec![inst(
                7,
                InstKind::RemoteCall {
                    dst: Some(Local(0)),
                    func: FuncId(0),
                    args: Vec::new(),
                },
            )],
            Terminator::Return(None),
        )],
    );
    assert_eq!(
        emit(&program(vec![remote])),
        Err(EmitError::Unsupported {
            span: span(7),
            what: "RemoteCall",
        })
    );
    let float_add = function(
        Placement::Shared,
        2,
        vec![Ty::Float, Ty::Float, Ty::Float],
        Ty::Float,
        vec![block(
            vec![inst(
                3,
                InstKind::Binary {
                    dst: Local(2),
                    op: BinOp::Add,
                    lhs: Local(0),
                    rhs: Local(1),
                },
            )],
            Terminator::Return(Some(Local(2))),
        )],
    );
    assert_eq!(
        emit(&program(vec![float_add])),
        Err(EmitError::Unsupported {
            span: span(3),
            what: "Float arithmetic",
        })
    );
    let list_eq = compare(Ty::List(Box::new(Ty::Int)), BinOp::Eq);
    assert_eq!(
        emit(&program(vec![list_eq])),
        Err(EmitError::Unsupported {
            span: span(1),
            what: "equality of this type",
        })
    );
}

#[test]
fn inconsistent_ir_is_an_error_not_a_panic() {
    let bad_local = function(
        Placement::Shared,
        0,
        Vec::new(),
        Ty::Int,
        vec![block(Vec::new(), Terminator::Return(Some(Local(3))))],
    );
    assert!(matches!(
        emit(&program(vec![bad_local])),
        Err(EmitError::Invalid(_))
    ));
    let bad_block = function(
        Placement::Shared,
        0,
        Vec::new(),
        Ty::Unit,
        vec![block(
            Vec::new(),
            Terminator::Jump(ostrel_ir::full::BlockId(4)),
        )],
    );
    assert!(matches!(
        emit(&program(vec![bad_block])),
        Err(EmitError::Invalid(_))
    ));
    let bad_call = function(
        Placement::Client,
        0,
        Vec::new(),
        Ty::Unit,
        vec![block(
            vec![inst(
                2,
                InstKind::Call {
                    dst: None,
                    func: FuncId(9),
                    args: Vec::new(),
                },
            )],
            Terminator::Return(None),
        )],
    );
    assert_eq!(
        emit(&program(vec![bad_call])),
        Err(EmitError::Invalid(span(2)))
    );
    let mut bad_enum = program(Vec::new());
    bad_enum.models.push(Model {
        name: "M".to_string(),
        fields: vec![Field {
            name: "e".to_string(),
            ty: Ty::Enum(EnumId(5)),
            mode: FieldMode::Plain,
            merge: Merge::Default,
            default: None,
            span: span(9),
        }],
        see: None,
        rules: Vec::new(),
        checks: Vec::new(),
        span: span(8),
    });
    assert_eq!(emit(&bad_enum), Err(EmitError::Invalid(span(9))));
    let no_blocks = function(Placement::Shared, 0, Vec::new(), Ty::Unit, Vec::new());
    assert!(matches!(
        emit(&program(vec![no_blocks])),
        Err(EmitError::Invalid(_))
    ));
}

#[test]
fn emitted_text_is_deterministic_and_free_of_html_sinks() {
    let p = program(vec![int_binary(BinOp::Div), compare(Ty::Text, BinOp::Lt)]);
    let first = emitted(&p);
    assert_eq!(first, emitted(&p));
    // The pattern list of ARCHITECTURE 7.5 (ci/checks/58_html_sinks.sh).
    let patterns = [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "writeln",
        "createContextualFragment",
        "setHTMLUnsafe",
        "parseHTMLUnsafe",
        "DOMParser",
        "srcdoc",
        "new Function",
        "eval(",
    ];
    for file in &first.files {
        for pattern in patterns {
            assert!(
                !file.text.contains(pattern),
                "{} contains {pattern}",
                file.path
            );
        }
    }
    // One import specifier for the runtime, so every module shares one instance
    // of each runtime module (ada, review of T150).
    assert!(app_text(&first).contains("from './runtime/crdt/canon/canon.mjs';"));
}
