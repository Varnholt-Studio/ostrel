//! Runtime error behaviour of the VM, written from the language documents rather than from
//! the implementation (work package T2-4, SPEC 5.0 AC-52, 12.3, 12.4, ARCHITECTURE 3.4, G9).
//!
//! Every case states the expected result first and builds the smallest IR that reaches it.
//! The source level goldens for the same rules live in `tests/runtime/`.

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

fn bin(dst: u32, op: BinOp, lhs: u32, rhs: u32) -> InstKind {
    InstKind::Binary {
        dst: l(dst),
        op,
        lhs: l(lhs),
        rhs: l(rhs),
    }
}

fn neg(dst: u32, arg: u32) -> InstKind {
    InstKind::Unary {
        dst: l(dst),
        op: UnOp::Neg,
        arg: l(arg),
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

fn func(name: &str, params: u32, locals: &[Ty], result: Ty, blocks: Vec<Block>) -> Function {
    Function {
        name: name.to_string(),
        params,
        locals: locals.to_vec(),
        result,
        blocks,
        span: sp(9000),
    }
}

fn block(insts: Vec<Inst>, term: Terminator) -> Block {
    Block { insts, term }
}

fn main_only(locals: &[Ty], insts: Vec<Inst>) -> Program {
    Program {
        functions: vec![func(
            "main",
            0,
            locals,
            Ty::Unit,
            vec![block(insts, Terminator::Return(None))],
        )],
        main: FuncId(0),
    }
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

/// `main` computes `a op b` at span 50, prints it, then prints "after".
fn binary_main(a: i64, op: BinOp, b: i64) -> Program {
    main_only(
        &[Ty::Int, Ty::Int, Ty::Int, Ty::Text],
        vec![
            at(1, int(0, a)),
            at(2, int(1, b)),
            at(50, bin(2, op, 0, 1)),
            at(3, print(2)),
            at(4, text(3, "after")),
            at(5, print(3)),
        ],
    )
}

/// Result of `a op b`: the printed value, or the error kind.
fn eval(a: i64, op: BinOp, b: i64) -> Result<String, RuntimeErrorKind> {
    let (out, result) = exec(&binary_main(a, op, b));
    match result {
        Ok(()) => {
            assert_eq!(out.get(1).map(String::as_str), Some("after"));
            Ok(out.first().cloned().unwrap_or_default())
        }
        Err(e) => {
            // A runtime error stops the program at the failing instruction (ARCHITECTURE 3.4).
            assert!(out.is_empty(), "nothing after the error may run: {out:?}");
            assert_eq!(e.span, sp(50), "reported at the failing instruction");
            Err(e.kind)
        }
    }
}

fn ok(v: i64) -> Result<String, RuntimeErrorKind> {
    Ok(v.to_string())
}

const OVERFLOW: Result<String, RuntimeErrorKind> = Err(RuntimeErrorKind::IntOverflow);
const DIV_ZERO: Result<String, RuntimeErrorKind> = Err(RuntimeErrorKind::DivisionByZero);

#[test]
fn ac_52_results_on_the_int_boundary_do_not_overflow() {
    // G9: the range is plus and minus (2^53 minus 1), both ends included.
    assert_eq!(eval(INT_MAX, BinOp::Add, 0), ok(INT_MAX));
    assert_eq!(eval(INT_MAX - 1, BinOp::Add, 1), ok(INT_MAX));
    assert_eq!(eval(INT_MIN + 1, BinOp::Sub, 1), ok(INT_MIN));
    assert_eq!(eval(INT_MAX, BinOp::Add, INT_MIN), ok(0));
    assert_eq!(eval(INT_MIN, BinOp::Sub, INT_MIN), ok(0));
    assert_eq!(eval(INT_MAX, BinOp::Mul, 1), ok(INT_MAX));
    assert_eq!(eval(INT_MIN, BinOp::Mul, 1), ok(INT_MIN));
    assert_eq!(eval(INT_MAX, BinOp::Mul, -1), ok(INT_MIN));
    assert_eq!(eval(INT_MIN, BinOp::Mul, -1), ok(INT_MAX));
    assert_eq!(eval(INT_MAX, BinOp::Mul, 0), ok(0));
    // The range is symmetric, so division by -1 cannot leave it (unlike i64::MIN / -1).
    assert_eq!(eval(INT_MIN, BinOp::Div, -1), ok(INT_MAX));
    assert_eq!(eval(INT_MIN, BinOp::Rem, -1), ok(0));
    assert_eq!(eval(INT_MAX, BinOp::Div, INT_MIN), ok(-1));
    assert_eq!(eval(INT_MIN, BinOp::Rem, INT_MAX), ok(0));
}

#[test]
fn ac_52_one_step_past_the_boundary_is_int_overflow() {
    assert_eq!(eval(INT_MAX, BinOp::Add, 1), OVERFLOW);
    assert_eq!(eval(1, BinOp::Add, INT_MAX), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Add, -1), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Sub, 1), OVERFLOW);
    assert_eq!(eval(INT_MAX, BinOp::Sub, -1), OVERFLOW);
    assert_eq!(eval(0, BinOp::Sub, INT_MIN), ok(INT_MAX));
    assert_eq!(eval(1, BinOp::Sub, INT_MIN), OVERFLOW);
    assert_eq!(eval(INT_MAX, BinOp::Mul, 2), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Mul, 2), OVERFLOW);
}

#[test]
fn ac_52_results_that_fit_in_64_bits_but_not_in_int_overflow() {
    // These fit in i64; a VM that only checks i64 overflow would accept them.
    assert_eq!(eval(INT_MAX, BinOp::Add, INT_MAX), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Add, INT_MIN), OVERFLOW);
    assert_eq!(eval(INT_MAX, BinOp::Sub, INT_MIN), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Sub, INT_MAX), OVERFLOW);
    // 2^26 * 2^27 = 2^53 is one above INT_MAX.
    assert_eq!(eval(1 << 26, BinOp::Mul, 1 << 27), OVERFLOW);
    assert_eq!(eval(-(1 << 26), BinOp::Mul, 1 << 27), OVERFLOW);
    assert_eq!(
        eval(1 << 26, BinOp::Mul, (1 << 27) - 1),
        ok((1 << 53) - (1 << 26))
    );
}

#[test]
fn ac_52_products_beyond_64_bits_are_int_overflow_not_a_wrap() {
    // INT_MAX * INT_MAX is about 2^106: a wrapping or saturating multiply would print a value
    // or panic instead of raising IntOverflow.
    assert_eq!(eval(INT_MAX, BinOp::Mul, INT_MAX), OVERFLOW);
    assert_eq!(eval(INT_MIN, BinOp::Mul, INT_MIN), OVERFLOW);
    assert_eq!(eval(INT_MAX, BinOp::Mul, INT_MIN), OVERFLOW);
    // 2^32 * 2^32 = 2^64 wraps to exactly 0 in 64 bit arithmetic.
    assert_eq!(eval(1 << 32, BinOp::Mul, 1 << 32), OVERFLOW);
}

#[test]
fn ac_52_negation_covers_the_whole_range() {
    let p = main_only(
        &[Ty::Int, Ty::Int, Ty::Int, Ty::Int],
        vec![
            at(1, int(0, INT_MIN)),
            at(2, neg(1, 0)),
            at(3, print(1)),
            at(4, int(2, INT_MAX)),
            at(5, neg(3, 2)),
            at(6, print(3)),
            at(7, int(0, 0)),
            at(8, neg(1, 0)),
            at(9, print(1)),
        ],
    );
    // SPEC 12.3: no `-0`, no `+`.
    assert_eq!(
        exec(&p),
        (
            vec![INT_MAX.to_string(), INT_MIN.to_string(), "0".to_string()],
            Ok(())
        )
    );
}

#[test]
fn ac_52_division_by_zero_for_every_dividend() {
    for a in [0, 1, -1, 7, -7, INT_MAX, INT_MIN] {
        assert_eq!(eval(a, BinOp::Div, 0), DIV_ZERO, "{a} / 0");
        assert_eq!(eval(a, BinOp::Rem, 0), DIV_ZERO, "{a} % 0");
    }
}

#[test]
fn ac_52_division_truncates_and_remainder_follows_the_dividend() {
    // ARCHITECTURE 3.4: `/` truncates toward zero, `%` takes the sign of the dividend, and
    // (a / b) * b + a % b == a.
    let cases = [
        (-1, 2, 0, -1),
        (1, -2, 0, 1),
        (-1, -2, 0, -1),
        (-9, -2, 4, -1),
        (9, -2, -4, 1),
        (-9, 2, -4, -1),
        (0, -5, 0, 0),
        (INT_MAX, 2, (INT_MAX - 1) / 2, 1),
        (INT_MIN, 2, -(INT_MAX - 1) / 2, -1),
        (INT_MIN, INT_MIN, 1, 0),
        (5, INT_MIN, 0, 5),
    ];
    for (a, b, q, r) in cases {
        assert_eq!(eval(a, BinOp::Div, b), ok(q), "{a} / {b}");
        assert_eq!(eval(a, BinOp::Rem, b), ok(r), "{a} % {b}");
        assert_eq!(q * b + r, a, "case table is consistent for {a}, {b}");
    }
}

/// `fn f(n: Int) -> Int { n op k }` called twice from `main`, printing each result; the
/// first call succeeds, the second fails inside `f`.
fn failing_callee(op: BinOp, k: i64, first: i64, second: i64) -> Program {
    Program {
        functions: vec![
            func(
                "main",
                0,
                &[Ty::Int, Ty::Int, Ty::Text],
                Ty::Unit,
                vec![block(
                    vec![
                        at(1, int(0, first)),
                        at(2, call(Some(1), 1, &[0])),
                        at(3, print(1)),
                        at(4, int(0, second)),
                        at(20, call(Some(1), 1, &[0])),
                        at(5, print(1)),
                        at(6, text(2, "not reached")),
                        at(7, print(2)),
                    ],
                    Terminator::Return(None),
                )],
            ),
            func(
                "f",
                1,
                &[Ty::Int, Ty::Int, Ty::Int],
                Ty::Int,
                vec![block(
                    vec![at(30, int(1, k)), at(70, bin(2, op, 0, 1))],
                    Terminator::Return(Some(l(2))),
                )],
            ),
        ],
        main: FuncId(0),
    }
}

#[test]
fn ac_52_errors_in_a_callee_are_reported_there_and_keep_earlier_output() {
    // Mirrors tests/runtime/int_overflow_mul_in_callee and division_by_zero.
    assert_eq!(
        exec(&failing_callee(BinOp::Mul, 2, 3, INT_MAX)),
        (
            vec!["6".to_string()],
            err(RuntimeErrorKind::IntOverflow, 70)
        )
    );
    assert_eq!(
        exec(&failing_callee(BinOp::Div, 0, 0, 1)),
        (vec![], err(RuntimeErrorKind::DivisionByZero, 70))
    );
    // 0 / 0 has no value either: the first call already fails.
    let p = failing_callee(BinOp::Rem, 0, 0, 0);
    assert_eq!(
        exec(&p),
        (vec![], err(RuntimeErrorKind::DivisionByZero, 70))
    );
}

#[test]
fn ac_52_call_depth_limit_follows_the_configured_value() {
    // `fn down(n) { if n == 0 { return } down(n - 1) }`: down(n) holds n + 1 frames plus main.
    fn down_main(n: i64) -> Program {
        Program {
            functions: vec![
                func(
                    "main",
                    0,
                    &[Ty::Int, Ty::Text],
                    Ty::Unit,
                    vec![block(
                        vec![
                            at(1, int(0, n)),
                            at(10, call(None, 1, &[0])),
                            at(2, text(1, "done")),
                            at(3, print(1)),
                        ],
                        Terminator::Return(None),
                    )],
                ),
                func(
                    "down",
                    1,
                    &[Ty::Int, Ty::Int, Ty::Bool],
                    Ty::Unit,
                    vec![
                        block(
                            vec![at(4, int(1, 0)), at(5, bin(2, BinOp::Eq, 0, 1))],
                            Terminator::Branch {
                                cond: l(2),
                                then: BlockId(1),
                                otherwise: BlockId(2),
                            },
                        ),
                        block(vec![], Terminator::Return(None)),
                        block(
                            vec![
                                at(6, int(1, 1)),
                                at(7, bin(1, BinOp::Sub, 0, 1)),
                                at(60, call(None, 1, &[1])),
                            ],
                            Terminator::Return(None),
                        ),
                    ],
                ),
            ],
            main: FuncId(0),
        }
    }
    for max_frames in [2_u32, 3, 100] {
        let limits = Limits {
            max_frames,
            ..Limits::default()
        };
        let fits = i64::from(max_frames) - 2;
        assert_eq!(
            exec_with(&down_main(fits), limits),
            (vec!["done".to_string()], Ok(())),
            "{max_frames} frames"
        );
        let over = exec_with(&down_main(fits + 1), limits);
        // The recursive call that would create frame max_frames + 1 is the one reported.
        assert_eq!(
            over,
            (vec![], err(RuntimeErrorKind::CallDepth, 60)),
            "{max_frames} frames"
        );
    }
    // With room for main only, the first call from main is the one that fails.
    let limits = Limits {
        max_frames: 1,
        ..Limits::default()
    };
    assert_eq!(
        exec_with(&down_main(0), limits),
        (vec![], err(RuntimeErrorKind::CallDepth, 10))
    );
}

#[test]
fn ac_52_step_limit_raised_by_max_steps_lets_the_program_finish() {
    // The same program fails under a small limit and finishes under the default: the limit
    // is a parameter of the run, not of the image (`--max-steps`).
    let p = failing_callee(BinOp::Add, 1, 1, 2);
    let image = compile(&p).expect("program compiles");
    let mut small = Capture::default();
    let limits = Limits {
        max_steps: 3,
        ..Limits::default()
    };
    let result = run(&image, &mut small, limits);
    assert_eq!(result.map_err(|e| e.kind), Err(RuntimeErrorKind::StepLimit));
    let mut full = Capture::default();
    assert_eq!(run(&image, &mut full, Limits::default()), Ok(()));
    assert_eq!(full.0, ["2", "3", "not reached"]);
}

#[test]
fn ac_52_zero_limits_fail_cleanly() {
    // Degenerate limits from a hostile command line must give a runtime error, not a panic.
    let p = failing_callee(BinOp::Add, 1, 1, 2);
    for limits in [
        Limits {
            max_steps: 0,
            ..Limits::default()
        },
        Limits {
            max_frames: 0,
            ..Limits::default()
        },
        Limits {
            max_heap_bytes: 0,
            ..Limits::default()
        },
    ] {
        let (out, result) = exec_with(&p, limits);
        assert!(out.is_empty(), "{limits:?}: {out:?}");
        assert!(result.is_err(), "{limits:?}");
    }
}
