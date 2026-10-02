//! Bytecode compiler and VM tested with hand built IR (work package T2-3).

use ostrel_ir::{
    BinOp, Block, BlockId, Const, FuncId, Function, INT_MAX, INT_MIN, Inst, InstKind, Local,
    Program, Span, Terminator, Ty, UnOp,
};
use ostrel_vm::{Host, Limits, RuntimeError, RuntimeErrorKind, compile, run};

#[derive(Default)]
struct Capture(Vec<String>);

impl Host for Capture {
    fn print(&mut self, text: &str) {
        self.0.push(text.to_string());
    }
}

fn sp(start: u32) -> Span {
    Span {
        file: 0,
        start,
        end: start + 1,
    }
}

fn at(start: u32, kind: InstKind) -> Inst {
    Inst {
        kind,
        span: sp(start),
    }
}

fn i(kind: InstKind) -> Inst {
    at(0, kind)
}

fn l(n: u32) -> Local {
    Local(n)
}

fn int(dst: u32, v: i64) -> InstKind {
    InstKind::Const {
        dst: l(dst),
        value: Const::Int(v),
    }
}

fn text(dst: u32, v: &str) -> InstKind {
    InstKind::Const {
        dst: l(dst),
        value: Const::Text(v.to_string()),
    }
}

fn boolean(dst: u32, v: bool) -> InstKind {
    InstKind::Const {
        dst: l(dst),
        value: Const::Bool(v),
    }
}

fn bin(dst: u32, op: BinOp, lhs: u32, rhs: u32) -> InstKind {
    InstKind::Binary {
        dst: l(dst),
        op,
        lhs: l(lhs),
        rhs: l(rhs),
    }
}

fn print(arg: u32) -> InstKind {
    InstKind::Print { arg: l(arg) }
}

fn call(dst: Option<u32>, func: u32, args: &[u32]) -> InstKind {
    InstKind::Call {
        dst: dst.map(l),
        func: FuncId(func),
        args: args.iter().copied().map(l).collect(),
    }
}

fn block(insts: Vec<Inst>, term: Terminator) -> Block {
    Block { insts, term }
}

fn ret() -> Terminator {
    Terminator::Return(None)
}

fn func(name: &str, params: u32, locals: &[Ty], result: Ty, blocks: Vec<Block>) -> Function {
    Function {
        name: name.to_string(),
        params,
        locals: locals.to_vec(),
        result,
        blocks,
        span: sp(1000),
    }
}

/// A program whose `main` is function 0.
fn program(functions: Vec<Function>) -> Program {
    Program {
        functions,
        main: FuncId(0),
    }
}

fn main_only(locals: &[Ty], insts: Vec<Inst>) -> Program {
    program(vec![func(
        "main",
        0,
        locals,
        Ty::Unit,
        vec![block(insts, ret())],
    )])
}

fn exec_with(program: &Program, limits: Limits) -> (Vec<String>, Result<(), RuntimeError>) {
    let image = compile(program).expect("program compiles");
    let mut host = Capture::default();
    let result = run(&image, &mut host, limits);
    (host.0, result)
}

fn exec(program: &Program) -> (Vec<String>, Result<(), RuntimeError>) {
    exec_with(program, Limits::default())
}

fn err(kind: RuntimeErrorKind, start: u32) -> Result<(), RuntimeError> {
    Err(RuntimeError {
        kind,
        span: sp(start),
    })
}

#[test]
fn hello_prints_one_line() {
    let p = main_only(&[Ty::Text], vec![i(text(0, "hello")), i(print(0))]);
    assert_eq!(exec(&p), (vec!["hello".to_string()], Ok(())));
}

#[test]
fn text_forms_of_int_bool_and_text() {
    let p = main_only(
        &[Ty::Int, Ty::Int, Ty::Bool, Ty::Bool, Ty::Text, Ty::Int],
        vec![
            i(int(0, 0)),
            i(print(0)),
            i(int(1, -42)),
            i(print(1)),
            i(boolean(2, true)),
            i(print(2)),
            i(boolean(3, false)),
            i(print(3)),
            i(text(4, "a \"q\" \u{0} b")),
            i(print(4)),
            i(int(5, INT_MIN)),
            i(print(5)),
        ],
    );
    let (out, result) = exec(&p);
    assert_eq!(result, Ok(()));
    assert_eq!(
        out,
        [
            "0",
            "-42",
            "true",
            "false",
            "a \"q\" \u{0} b",
            "-9007199254740991"
        ]
    );
}

#[test]
fn division_signs_follow_the_architecture() {
    // (-7) / 2, (-7) % 2, 7 / (-2), 7 % (-2)
    let p = main_only(
        &[Ty::Int, Ty::Int, Ty::Int, Ty::Int, Ty::Int, Ty::Int],
        vec![
            i(int(0, 7)),
            i(int(1, 2)),
            i(InstKind::Unary {
                dst: l(2),
                op: UnOp::Neg,
                arg: l(0),
            }),
            i(InstKind::Unary {
                dst: l(3),
                op: UnOp::Neg,
                arg: l(1),
            }),
            i(bin(4, BinOp::Div, 2, 1)),
            i(print(4)),
            i(bin(4, BinOp::Rem, 2, 1)),
            i(print(4)),
            i(bin(4, BinOp::Div, 0, 3)),
            i(print(4)),
            i(bin(4, BinOp::Rem, 0, 3)),
            i(print(4)),
            i(bin(5, BinOp::Mul, 0, 1)),
            i(bin(5, BinOp::Sub, 5, 1)),
            i(bin(5, BinOp::Add, 5, 0)),
            i(print(5)),
        ],
    );
    let (out, result) = exec(&p);
    assert_eq!(result, Ok(()));
    assert_eq!(out, ["-3", "-1", "-3", "1", "19"]);
}

#[test]
fn comparisons_equality_and_not() {
    let p = main_only(
        &[
            Ty::Int,
            Ty::Int,
            Ty::Bool,
            Ty::Text,
            Ty::Text,
            Ty::Bool,
            Ty::Bool,
        ],
        vec![
            i(int(0, 1)),
            i(int(1, 2)),
            i(bin(2, BinOp::Lt, 0, 1)),
            i(print(2)),
            i(bin(2, BinOp::Le, 1, 1)),
            i(print(2)),
            i(bin(2, BinOp::Gt, 0, 1)),
            i(print(2)),
            i(bin(2, BinOp::Ge, 0, 1)),
            i(print(2)),
            i(bin(2, BinOp::Eq, 0, 1)),
            i(print(2)),
            i(bin(2, BinOp::Ne, 0, 1)),
            i(print(2)),
            i(text(3, "ab")),
            i(text(4, "ab")),
            i(bin(2, BinOp::Eq, 3, 4)),
            i(print(2)),
            i(boolean(5, true)),
            i(InstKind::Unary {
                dst: l(6),
                op: UnOp::Not,
                arg: l(5),
            }),
            i(print(6)),
            i(bin(2, BinOp::Ne, 5, 6)),
            i(print(2)),
        ],
    );
    let (out, result) = exec(&p);
    assert_eq!(result, Ok(()));
    assert_eq!(
        out,
        [
            "true", "true", "false", "false", "false", "true", "true", "false", "true"
        ]
    );
}

#[test]
fn interpolation_concatenates_text_forms() {
    // print("n = {n}, ok = {b}!")
    let p = main_only(
        &[
            Ty::Int,
            Ty::Bool,
            Ty::Text,
            Ty::Text,
            Ty::Text,
            Ty::Text,
            Ty::Text,
            Ty::Text,
            Ty::Text,
        ],
        vec![
            i(int(0, -5)),
            i(boolean(1, false)),
            i(text(2, "n = ")),
            i(InstKind::ToText {
                dst: l(3),
                src: l(0),
            }),
            i(text(4, ", ok = ")),
            i(InstKind::ToText {
                dst: l(5),
                src: l(1),
            }),
            i(text(6, "!")),
            i(InstKind::Concat {
                dst: l(7),
                parts: vec![l(2), l(3), l(4), l(5), l(6)],
            }),
            i(print(7)),
            i(InstKind::ToText {
                dst: l(8),
                src: l(7),
            }),
            i(print(8)),
            i(InstKind::Concat {
                dst: l(8),
                parts: vec![],
            }),
            i(print(8)),
        ],
    );
    let (out, result) = exec(&p);
    assert_eq!(result, Ok(()));
    assert_eq!(out, ["n = -5, ok = false!", "n = -5, ok = false!", ""]);
}

#[test]
fn locals_start_with_type_defaults() {
    let p = main_only(
        &[Ty::Int, Ty::Text, Ty::Bool],
        vec![i(print(0)), i(print(1)), i(print(2))],
    );
    assert_eq!(
        exec(&p),
        (vec!["0".into(), String::new(), "false".into()], Ok(()))
    );
}

/// `fn main() { if f(1) and f(2) { print("both") } else { print("not") } }` with
/// `fn f(n: Int) -> Bool { print(n); return n == 1 }`, `and` lowered to branches.
#[test]
fn short_circuit_lowered_to_branches() {
    let f = func(
        "f",
        1,
        &[Ty::Int, Ty::Int, Ty::Bool],
        Ty::Bool,
        vec![block(
            vec![i(print(0)), i(int(1, 1)), i(bin(2, BinOp::Eq, 0, 1))],
            Terminator::Return(Some(l(2))),
        )],
    );
    let make_main = |first: i64| {
        func(
            "main",
            0,
            &[Ty::Int, Ty::Bool, Ty::Text],
            Ty::Unit,
            vec![
                block(
                    vec![i(int(0, first)), i(call(Some(1), 1, &[0]))],
                    Terminator::Branch {
                        cond: l(1),
                        then: BlockId(1),
                        otherwise: BlockId(2),
                    },
                ),
                block(
                    vec![i(int(0, 2)), i(call(Some(1), 1, &[0]))],
                    Terminator::Jump(BlockId(2)),
                ),
                block(
                    vec![],
                    Terminator::Branch {
                        cond: l(1),
                        then: BlockId(3),
                        otherwise: BlockId(4),
                    },
                ),
                block(vec![i(text(2, "both")), i(print(2))], ret()),
                block(vec![i(text(2, "not")), i(print(2))], ret()),
            ],
        )
    };
    let (out, result) = exec(&program(vec![make_main(1), f.clone()]));
    assert_eq!(result, Ok(()));
    assert_eq!(out, ["1", "2", "not"]);
    let (out, result) = exec(&program(vec![make_main(3), f]));
    assert_eq!(result, Ok(()));
    assert_eq!(out, ["3", "not"]);
}

/// `fn fact(n: Int) -> Int { if n <= 1 { return 1 } return n * fact(n - 1) }`.
fn fact() -> Function {
    func(
        "fact",
        1,
        &[Ty::Int, Ty::Int, Ty::Bool, Ty::Int, Ty::Int],
        Ty::Int,
        vec![
            block(
                vec![i(int(1, 1)), i(bin(2, BinOp::Le, 0, 1))],
                Terminator::Branch {
                    cond: l(2),
                    then: BlockId(1),
                    otherwise: BlockId(2),
                },
            ),
            block(vec![], Terminator::Return(Some(l(1)))),
            block(
                vec![
                    i(bin(3, BinOp::Sub, 0, 1)),
                    at(40, call(Some(4), 1, &[3])),
                    at(41, bin(4, BinOp::Mul, 0, 4)),
                ],
                Terminator::Return(Some(l(4))),
            ),
        ],
    )
}

fn fact_main(n: i64) -> Program {
    program(vec![
        func(
            "main",
            0,
            &[Ty::Int, Ty::Int],
            Ty::Unit,
            vec![block(
                vec![i(int(0, n)), i(call(Some(1), 1, &[0])), i(print(1))],
                ret(),
            )],
        ),
        fact(),
    ])
}

#[test]
fn recursion_returns_values() {
    assert_eq!(exec(&fact_main(10)), (vec!["3628800".to_string()], Ok(())));
    // 18! = 6402373705728000 is the largest factorial below 2^53.
    assert_eq!(
        exec(&fact_main(18)),
        (vec!["6402373705728000".to_string()], Ok(()))
    );
}

#[test]
fn overflow_is_reported_at_the_failing_instruction() {
    // 19! exceeds 2^53 - 1 although it fits in i64.
    assert_eq!(
        exec(&fact_main(19)),
        (vec![], err(RuntimeErrorKind::IntOverflow, 41))
    );
    let p = main_only(
        &[Ty::Int, Ty::Int, Ty::Int],
        vec![
            i(int(0, INT_MAX)),
            i(int(1, 1)),
            at(7, bin(2, BinOp::Add, 0, 1)),
        ],
    );
    assert_eq!(exec(&p), (vec![], err(RuntimeErrorKind::IntOverflow, 7)));
    let p = main_only(
        &[Ty::Int, Ty::Int, Ty::Int],
        vec![
            i(int(0, INT_MIN)),
            i(int(1, 1)),
            at(8, bin(2, BinOp::Sub, 0, 1)),
        ],
    );
    assert_eq!(exec(&p), (vec![], err(RuntimeErrorKind::IntOverflow, 8)));
}

#[test]
fn division_by_zero_keeps_earlier_output() {
    for op in [BinOp::Div, BinOp::Rem] {
        let p = main_only(
            &[Ty::Int, Ty::Int, Ty::Int],
            vec![
                i(int(0, 1)),
                i(int(1, 0)),
                i(print(0)),
                at(9, bin(2, op, 0, 1)),
                i(print(2)),
            ],
        );
        assert_eq!(
            exec(&p),
            (
                vec!["1".to_string()],
                err(RuntimeErrorKind::DivisionByZero, 9)
            )
        );
    }
}

/// `fn down(n: Int) { if n == 0 { return } down(n - 1) }`, called from `main`
/// with `n`: holds `n + 2` frames at the deepest point.
fn down_main(n: i64) -> Program {
    program(vec![
        func(
            "main",
            0,
            &[Ty::Int, Ty::Text],
            Ty::Unit,
            vec![block(
                vec![
                    i(int(0, n)),
                    at(5, call(None, 1, &[0])),
                    i(text(1, "done")),
                    i(print(1)),
                ],
                ret(),
            )],
        ),
        func(
            "down",
            1,
            &[Ty::Int, Ty::Int, Ty::Bool],
            Ty::Unit,
            vec![
                block(
                    vec![i(int(1, 0)), i(bin(2, BinOp::Eq, 0, 1))],
                    Terminator::Branch {
                        cond: l(2),
                        then: BlockId(1),
                        otherwise: BlockId(2),
                    },
                ),
                block(vec![], ret()),
                block(
                    vec![
                        i(int(1, 1)),
                        i(bin(1, BinOp::Sub, 0, 1)),
                        at(30, call(None, 1, &[1])),
                    ],
                    ret(),
                ),
            ],
        ),
    ])
}

#[test]
fn call_depth_boundary_is_ten_thousand_frames() {
    // main is frame 1; down(9998) reaches frame 10 000.
    assert_eq!(exec(&down_main(9998)), (vec!["done".to_string()], Ok(())));
    // down(9999) would create frame 10 001 at the recursive call.
    assert_eq!(
        exec(&down_main(9999)),
        (vec![], err(RuntimeErrorKind::CallDepth, 30))
    );
    // The first call is reported at its own span when the limit is reached there.
    let limits = Limits {
        max_frames: 1,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&down_main(5), limits),
        (vec![], err(RuntimeErrorKind::CallDepth, 5))
    );
}

#[test]
fn unbounded_recursion_ends_in_call_depth_without_native_stack_growth() {
    // `fn main() { main() }`
    let p = program(vec![func(
        "main",
        0,
        &[],
        Ty::Unit,
        vec![block(vec![at(3, call(None, 0, &[]))], ret())],
    )]);
    let limits = Limits {
        max_frames: 1_000_000,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&p, limits),
        (vec![], err(RuntimeErrorKind::CallDepth, 3))
    );
}

/// `fn main() { print("tick"); loop { } }`: a block that jumps to itself.
fn spin() -> Program {
    program(vec![func(
        "main",
        0,
        &[Ty::Text],
        Ty::Unit,
        vec![
            block(
                vec![i(text(0, "tick")), at(2, print(0))],
                Terminator::Jump(BlockId(1)),
            ),
            block(vec![], Terminator::Jump(BlockId(1))),
        ],
    )])
}

#[test]
fn step_limit_stops_endless_loops() {
    let limits = Limits {
        max_steps: 1000,
        ..Limits::default()
    };
    // The empty spinning block reports the location of the function.
    assert_eq!(
        exec_with(&spin(), limits),
        (
            vec!["tick".to_string()],
            err(RuntimeErrorKind::StepLimit, 1000)
        )
    );
    // Instructions are counted before they run: with 1 step the print never runs.
    let limits = Limits {
        max_steps: 1,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&spin(), limits),
        (vec![], err(RuntimeErrorKind::StepLimit, 2))
    );
}

#[test]
fn step_limit_ends_deep_recursion_too() {
    let limits = Limits {
        max_steps: 500,
        ..Limits::default()
    };
    let (out, result) = exec_with(&down_main(5000), limits);
    assert!(out.is_empty());
    assert_eq!(result.map_err(|e| e.kind), Err(RuntimeErrorKind::StepLimit));
}

/// `let t = "x"`, then `t = "{t}{t}"` repeated `rounds` times, then `print("ok")`.
/// The loop runs on a counter, lowered to blocks by hand.
fn doubling(rounds: i64) -> Program {
    program(vec![func(
        "main",
        0,
        &[Ty::Text, Ty::Int, Ty::Int, Ty::Bool, Ty::Int, Ty::Text],
        Ty::Unit,
        vec![
            block(
                vec![
                    i(text(0, "x")),
                    i(int(1, rounds)),
                    i(int(2, 0)),
                    i(int(4, 1)),
                ],
                Terminator::Jump(BlockId(1)),
            ),
            block(
                vec![i(bin(3, BinOp::Lt, 2, 1))],
                Terminator::Branch {
                    cond: l(3),
                    then: BlockId(2),
                    otherwise: BlockId(3),
                },
            ),
            block(
                vec![
                    at(
                        50,
                        InstKind::Concat {
                            dst: l(0),
                            parts: vec![l(0), l(0)],
                        },
                    ),
                    i(bin(2, BinOp::Add, 2, 4)),
                ],
                Terminator::Jump(BlockId(1)),
            ),
            block(vec![i(text(5, "ok")), i(print(5))], ret()),
        ],
    )])
}

#[test]
fn text_limit_boundary_is_inclusive() {
    // Ten doublings of "x" give exactly 1024 bytes.
    let limits = Limits {
        max_text_bytes: 1024,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&doubling(10), limits),
        (vec!["ok".to_string()], Ok(()))
    );
    assert_eq!(
        exec_with(&doubling(11), limits),
        (vec![], err(RuntimeErrorKind::TextLimit, 50))
    );
}

#[test]
fn text_limit_default_is_sixteen_mib() {
    // 2^24 bytes is allowed, 2^25 is not.
    assert_eq!(exec(&doubling(24)), (vec!["ok".to_string()], Ok(())));
    assert_eq!(
        exec(&doubling(25)),
        (vec![], err(RuntimeErrorKind::TextLimit, 50))
    );
}

#[test]
fn text_limit_applies_to_constants_and_text_forms() {
    let limits = Limits {
        max_text_bytes: 3,
        ..Limits::default()
    };
    let p = main_only(&[Ty::Text], vec![at(4, text(0, "four"))]);
    assert_eq!(
        exec_with(&p, limits),
        (vec![], err(RuntimeErrorKind::TextLimit, 4))
    );
    let p = main_only(
        &[Ty::Int, Ty::Text, Ty::Bool],
        vec![
            i(int(0, -99)),
            i(InstKind::ToText {
                dst: l(1),
                src: l(0),
            }),
            i(print(1)),
            i(boolean(2, false)),
            at(
                6,
                InstKind::ToText {
                    dst: l(1),
                    src: l(2),
                },
            ),
        ],
    );
    assert_eq!(
        exec_with(&p, limits),
        (vec!["-99".to_string()], err(RuntimeErrorKind::TextLimit, 6))
    );
}

#[test]
fn heap_limit_counts_live_text() {
    let limits = Limits {
        max_heap_bytes: 64 * 1024,
        ..Limits::default()
    };
    // 2^15 bytes plus the previous 2^14 still live fit; 2^16 does not.
    assert_eq!(
        exec_with(&doubling(15), limits),
        (vec!["ok".to_string()], Ok(()))
    );
    assert_eq!(
        exec_with(&doubling(16), limits),
        (vec![], err(RuntimeErrorKind::HeapLimit, 50))
    );
}

#[test]
fn heap_is_released_when_text_dies() {
    // `loop 100 000 times { t = "{a}{a}" }` with a 1 KiB text: 200 MB in total, but
    // only two texts are ever live.
    let p = program(vec![func(
        "main",
        0,
        &[Ty::Text, Ty::Text, Ty::Int, Ty::Int, Ty::Bool, Ty::Int],
        Ty::Unit,
        vec![
            block(
                vec![
                    i(text(0, &"a".repeat(1024))),
                    i(int(2, 0)),
                    i(int(3, 100_000)),
                    i(int(5, 1)),
                ],
                Terminator::Jump(BlockId(1)),
            ),
            block(
                vec![i(bin(4, BinOp::Lt, 2, 3))],
                Terminator::Branch {
                    cond: l(4),
                    then: BlockId(2),
                    otherwise: BlockId(3),
                },
            ),
            block(
                vec![
                    i(InstKind::Concat {
                        dst: l(1),
                        parts: vec![l(0), l(0)],
                    }),
                    i(bin(2, BinOp::Add, 2, 5)),
                ],
                Terminator::Jump(BlockId(1)),
            ),
            block(vec![i(print(2))], ret()),
        ],
    )]);
    let limits = Limits {
        max_heap_bytes: 16 * 1024,
        ..Limits::default()
    };
    assert_eq!(exec_with(&p, limits), (vec!["100000".to_string()], Ok(())));
}

#[test]
fn heap_limit_counts_frames() {
    let limits = Limits {
        max_heap_bytes: 64 * 1024,
        ..Limits::default()
    };
    let (out, result) = exec_with(&down_main(9000), limits);
    assert!(out.is_empty());
    assert_eq!(result, err(RuntimeErrorKind::HeapLimit, 30));
    // Frames are released on return: many shallow calls fit.
    assert_eq!(
        exec_with(&down_main(100), limits),
        (vec!["done".to_string()], Ok(()))
    );
    // Even main's frame is charged.
    let limits = Limits {
        max_heap_bytes: 0,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&down_main(1), limits),
        (vec![], err(RuntimeErrorKind::HeapLimit, 1000))
    );
}

#[test]
fn an_image_runs_repeatedly_with_fresh_state() {
    let image = compile(&fact_main(5)).expect("compiles");
    for _ in 0..3 {
        let mut host = Capture::default();
        assert_eq!(run(&image, &mut host, Limits::default()), Ok(()));
        assert_eq!(host.0, ["120"]);
    }
}

#[test]
fn void_results_may_be_discarded_and_values_ignored() {
    // `fn one() -> Int { return 1 }`, called as a statement.
    let p = program(vec![
        func(
            "main",
            0,
            &[Ty::Text],
            Ty::Unit,
            vec![block(
                vec![i(call(None, 1, &[])), i(text(0, "fine")), i(print(0))],
                ret(),
            )],
        ),
        func(
            "one",
            0,
            &[Ty::Int],
            Ty::Int,
            vec![block(vec![i(int(0, 1))], Terminator::Return(Some(l(0))))],
        ),
    ]);
    assert_eq!(exec(&p), (vec!["fine".to_string()], Ok(())));
}

fn compile_error(p: &Program) -> String {
    match compile(p) {
        Ok(_) => panic!("malformed IR compiled"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn malformed_ir_is_rejected_by_the_compiler() {
    let cases: Vec<(Program, &str)> = vec![
        (
            Program {
                functions: vec![],
                main: FuncId(0),
            },
            "entry function 0 does not exist",
        ),
        (
            program(vec![func(
                "main",
                1,
                &[Ty::Int],
                Ty::Unit,
                vec![block(vec![], ret())],
            )]),
            "entry function must take no parameters",
        ),
        (
            program(vec![func(
                "main",
                0,
                &[],
                Ty::Int,
                vec![block(vec![], ret())],
            )]),
            "entry function must take no parameters",
        ),
        (
            program(vec![func("main", 0, &[], Ty::Unit, vec![])]),
            "no blocks",
        ),
        (main_only(&[], vec![i(int(0, 1))]), "local 0 does not exist"),
        (main_only(&[Ty::Text], vec![i(int(0, 1))]), "expected Int"),
        (main_only(&[Ty::Unit], vec![i(print(0))]), "type Unit"),
        (
            main_only(&[Ty::Int], vec![i(int(0, INT_MAX + 1))]),
            "out of range",
        ),
        (
            main_only(&[Ty::Int, Ty::Bool], vec![i(bin(1, BinOp::Eq, 0, 1))]),
            "expected Int",
        ),
        (
            main_only(&[Ty::Bool, Ty::Bool], vec![i(bin(0, BinOp::Add, 1, 1))]),
            "expected Int",
        ),
        (
            main_only(
                &[Ty::Int, Ty::Text],
                vec![i(InstKind::Concat {
                    dst: l(1),
                    parts: vec![l(0)],
                })],
            ),
            "expected Text",
        ),
        (
            main_only(&[], vec![i(call(None, 7, &[]))]),
            "function 7 does not exist",
        ),
        (
            program(vec![
                func(
                    "main",
                    0,
                    &[],
                    Ty::Unit,
                    vec![block(vec![i(call(None, 1, &[]))], ret())],
                ),
                fact(),
            ]),
            "with 0 arguments, expected 1",
        ),
        (
            program(vec![func(
                "main",
                0,
                &[Ty::Int],
                Ty::Unit,
                vec![block(vec![i(call(Some(0), 0, &[]))], ret())],
            )]),
            "is Unit and cannot be stored",
        ),
        (
            program(vec![func(
                "main",
                0,
                &[],
                Ty::Unit,
                vec![block(vec![], Terminator::Jump(BlockId(3)))],
            )]),
            "block 3 does not exist",
        ),
        (
            program(vec![func(
                "main",
                0,
                &[Ty::Int],
                Ty::Unit,
                vec![block(
                    vec![],
                    Terminator::Branch {
                        cond: l(0),
                        then: BlockId(0),
                        otherwise: BlockId(0),
                    },
                )],
            )]),
            "expected Bool",
        ),
        (
            program(vec![func(
                "main",
                0,
                &[Ty::Int],
                Ty::Unit,
                vec![block(vec![], Terminator::Return(Some(l(0))))],
            )]),
            "return of a value from a Unit function",
        ),
        (
            program(vec![
                func("main", 0, &[], Ty::Unit, vec![block(vec![], ret())]),
                func("f", 0, &[], Ty::Int, vec![block(vec![], ret())]),
            ]),
            "return without a value",
        ),
        (
            program(vec![
                func("main", 0, &[], Ty::Unit, vec![block(vec![], ret())]),
                func("f", 2, &[Ty::Int], Ty::Unit, vec![block(vec![], ret())]),
            ]),
            "more parameters than locals",
        ),
    ];
    for (p, want) in cases {
        let got = compile_error(&p);
        assert!(got.contains(want), "expected {want:?} in {got:?}");
    }
}

#[test]
fn compile_errors_name_function_and_span() {
    let p = main_only(&[Ty::Text], vec![at(77, int(0, 1))]);
    let e = compile(&p).expect_err("malformed");
    assert_eq!(e.function.as_deref(), Some("main"));
    assert_eq!(e.span, sp(77));
}
