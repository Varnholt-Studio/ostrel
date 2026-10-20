//! Hostile input hardening of the bytecode compiler and the VM under `ostrel run`
//! (gap G-1, SPEC 9, D41, AC-04).
//!
//! The VM is the last stage of `ostrel run` and the one that turns hostile programs into
//! running time and memory. `ostrel_vm` does not depend on the parser, so this file builds
//! its inputs as IR: seeded random programs, well formed and malformed, and the shapes of
//! the `run_` cases of `tests/hostile/cases/` at sizes the limits must stop. It asserts:
//! * `compile` returns an image or a `CompileError` for any IR, never a panic,
//! * `run` ends within its step limit with `Ok` or a `RuntimeError` for every image and
//!   for any `Limits`, including zero and maximal values, never a panic,
//! * a runtime error points at a span that exists in the program,
//! * two runs of one image give the same output and the same result (D74).
//!
//! The source level `run_` cases themselves are run by `tests/hostile/run.sh`; the checker
//! and the lowering are covered by the `hostile` tests of `ostrel_sema` and `ostrel_ir`.

use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};

use ostrel_ir::{
    BinOp, Block, BlockId, Const, FuncId, Function, INT_MAX, INT_MIN, Inst, InstKind, Local,
    Program, Span, Terminator, Ty, UnOp,
};
use ostrel_vm::{Host, Image, Limits, RuntimeError, RuntimeErrorKind, compile, run};

/// Random programs in [`random_ir_never_breaks_compiler_or_vm`].
const RANDOM_PROGRAMS: u64 = 4_000;

/// Base seed of the random programs. Changing it changes the inputs, never the rules.
const SEED: u64 = 0x6f73_7472_656c_2044;

/// Collects output and counts bytes, so a test can bound what a program printed.
#[derive(Default)]
struct Capture {
    lines: Vec<String>,
}

impl Host for Capture {
    fn print(&mut self, text: &str) {
        self.lines.push(text.to_string());
    }
}

/// SplitMix64, the generator of `tests/hostile/gen`: small and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    fn index(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    /// True with probability `percent` / 100.
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.index(items.len())]
    }
}

const VALUE_TYPES: [Ty; 3] = [Ty::Int, Ty::Text, Ty::Bool];

const INTS: [i64; 10] = [
    0,
    1,
    -1,
    2,
    64,
    INT_MAX,
    INT_MIN,
    INT_MAX - 1,
    INT_MAX + 1,
    i64::MIN,
];

const TEXTS: [&str; 7] = ["", "a", "{x}", "\u{0}", "\u{202e}", "é😀", "\n"];

const BIN_OPS: [BinOp; 11] = [
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Rem,
    BinOp::Eq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
];

/// Builds one random program. Most operands are chosen with the right type, so a good
/// part of the programs compiles and runs; the rest probes the compiler's validation with
/// wrong types, indexes out of range and constants outside the `Int` range.
struct Gen<'a> {
    rng: &'a mut Rng,
    /// Signature of every function: parameter types, result.
    sigs: Vec<(Vec<Ty>, Ty)>,
    next_span: u32,
}

impl Gen<'_> {
    fn span(&mut self) -> Span {
        self.next_span += 2;
        Span {
            file: 0,
            start: self.next_span,
            end: self.next_span + 1,
        }
    }

    /// A local of type `ty` from `locals` most of the time, otherwise any index,
    /// sometimes one past the end.
    fn local(&mut self, locals: &[Ty], ty: Ty) -> Local {
        let typed: Vec<usize> = (0..locals.len()).filter(|&i| locals[i] == ty).collect();
        let index = if !typed.is_empty() && self.rng.chance(98) {
            typed[self.rng.index(typed.len())]
        } else {
            self.rng.index(locals.len() + 1)
        };
        Local(index as u32)
    }

    fn any_local(&mut self, locals: &[Ty]) -> Local {
        let ty = self.rng.pick(&VALUE_TYPES);
        self.local(locals, ty)
    }

    fn block_id(&mut self, blocks: usize) -> BlockId {
        let n = if self.rng.chance(98) {
            blocks
        } else {
            blocks + 1
        };
        BlockId(self.rng.index(n) as u32)
    }

    fn constant(&mut self) -> Const {
        match self.rng.below(3) {
            0 => {
                let v = self.rng.pick(&INTS);
                // Constants outside the Int range are malformed IR (G9); keep them rare.
                if (INT_MIN..=INT_MAX).contains(&v) || self.rng.chance(10) {
                    Const::Int(v)
                } else {
                    Const::Int(0)
                }
            }
            1 => Const::Text(self.rng.pick(&TEXTS).to_string()),
            _ => Const::Bool(self.rng.chance(50)),
        }
    }

    fn inst(&mut self, locals: &[Ty], funcs: usize) -> InstKind {
        match self.rng.below(8) {
            0 => {
                let value = self.constant();
                InstKind::Const {
                    dst: self.local(locals, value.ty()),
                    value,
                }
            }
            1 => {
                let ty = self.rng.pick(&VALUE_TYPES);
                InstKind::Copy {
                    dst: self.local(locals, ty),
                    src: self.local(locals, ty),
                }
            }
            2 => {
                let (op, ty) = if self.rng.chance(50) {
                    (UnOp::Neg, Ty::Int)
                } else {
                    (UnOp::Not, Ty::Bool)
                };
                InstKind::Unary {
                    dst: self.local(locals, ty),
                    op,
                    arg: self.local(locals, ty),
                }
            }
            3 => {
                let op = self.rng.pick(&BIN_OPS);
                let operand = if matches!(op, BinOp::Eq | BinOp::Ne) {
                    self.rng.pick(&VALUE_TYPES)
                } else {
                    Ty::Int
                };
                InstKind::Binary {
                    dst: self.local(locals, op.result_ty()),
                    op,
                    lhs: self.local(locals, operand),
                    rhs: self.local(locals, operand),
                }
            }
            4 => InstKind::ToText {
                dst: self.local(locals, Ty::Text),
                src: self.any_local(locals),
            },
            5 => {
                let parts = (0..self.rng.below(5))
                    .map(|_| self.local(locals, Ty::Text))
                    .collect();
                InstKind::Concat {
                    dst: self.local(locals, Ty::Text),
                    parts,
                }
            }
            6 => {
                let n = if self.rng.chance(98) {
                    funcs
                } else {
                    funcs + 1
                };
                let func = self.rng.index(n);
                let (params, result) = self
                    .sigs
                    .get(func)
                    .cloned()
                    .unwrap_or((Vec::new(), Ty::Unit));
                let mut args: Vec<Local> = params.iter().map(|&t| self.local(locals, t)).collect();
                if self.rng.chance(2) {
                    args.pop();
                }
                let dst = if result == Ty::Unit && self.rng.chance(95) {
                    None
                } else {
                    Some(self.local(locals, result))
                };
                InstKind::Call {
                    dst,
                    func: FuncId(func as u32),
                    args,
                }
            }
            _ => InstKind::Print {
                arg: self.any_local(locals),
            },
        }
    }

    fn function(&mut self, index: usize, funcs: usize) -> Function {
        let (params, result) = self.sigs[index].clone();
        // One local of every value type, so most operands can be chosen well typed.
        let mut locals = params.clone();
        locals.extend(VALUE_TYPES);
        for _ in 0..self.rng.below(7) {
            let ty = if self.rng.chance(1) {
                Ty::Unit
            } else {
                self.rng.pick(&VALUE_TYPES)
            };
            locals.push(ty);
        }
        let blocks = 1 + self.rng.index(4);
        let mut out = Vec::with_capacity(blocks);
        for _ in 0..blocks {
            let insts = (0..self.rng.below(7))
                .map(|_| {
                    let kind = self.inst(&locals, funcs);
                    Inst {
                        kind,
                        span: self.span(),
                    }
                })
                .collect();
            let term = match self.rng.below(4) {
                0 => Terminator::Jump(self.block_id(blocks)),
                1 => Terminator::Branch {
                    cond: self.local(&locals, Ty::Bool),
                    then: self.block_id(blocks),
                    otherwise: self.block_id(blocks),
                },
                _ if result == Ty::Unit => Terminator::Return(None),
                _ => Terminator::Return(Some(self.local(&locals, result))),
            };
            out.push(Block { insts, term });
        }
        // Random operands rarely meet the `Int` bounds on a reachable path, so some entry
        // blocks start with `x = INT_MAX; x = x op x` (or `INT_MIN`), which leaves the
        // range of D24.
        let int_local = Local(params.len() as u32);
        if self.rng.chance(15)
            && let Some(entry) = out.first_mut()
        {
            let op = self.rng.pick(&[BinOp::Add, BinOp::Mul]);
            let value = self.rng.pick(&[INT_MAX, INT_MIN]);
            let probe = [
                InstKind::Const {
                    dst: int_local,
                    value: Const::Int(value),
                },
                InstKind::Binary {
                    dst: int_local,
                    op,
                    lhs: int_local,
                    rhs: int_local,
                },
            ];
            for (offset, kind) in probe.into_iter().enumerate() {
                let span = self.span();
                entry.insts.insert(offset, Inst { kind, span });
            }
        }
        let mut params_count = params.len() as u32;
        if self.rng.chance(1) {
            params_count += 1;
        }
        Function {
            name: format!("f{index}"),
            params: params_count,
            locals,
            result,
            blocks: out,
            span: self.span(),
        }
    }

    fn program(rng: &mut Rng) -> Program {
        let funcs = 1 + rng.index(4);
        let mut sigs = Vec::with_capacity(funcs);
        for index in 0..funcs {
            let main_shape = index == 0 && rng.chance(95);
            let params = if main_shape {
                Vec::new()
            } else {
                (0..rng.below(3)).map(|_| rng.pick(&VALUE_TYPES)).collect()
            };
            let result = if main_shape || rng.chance(30) {
                Ty::Unit
            } else {
                rng.pick(&VALUE_TYPES)
            };
            sigs.push((params, result));
        }
        let main = if rng.chance(97) { 0 } else { funcs as u32 };
        let mut g = Gen {
            rng,
            sigs,
            next_span: 0,
        };
        let functions = (0..funcs).map(|i| g.function(i, funcs)).collect();
        Program {
            functions,
            main: FuncId(main),
        }
    }
}

/// Every span a runtime error may name: the instructions and the functions.
fn spans(program: &Program) -> HashSet<Span> {
    let mut out = HashSet::new();
    for f in &program.functions {
        out.insert(f.span);
        for b in &f.blocks {
            out.extend(b.insts.iter().map(|i| i.span));
        }
    }
    out
}

/// Runs `image` and returns output and result, or `None` if the VM panicked.
fn guarded_run(image: &Image, limits: Limits) -> Option<(Vec<String>, Result<(), RuntimeError>)> {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let mut host = Capture::default();
        let result = run(image, &mut host, limits);
        (host.lines, result)
    }))
    .ok()
}

/// Limits from degenerate (zero) to roomy; the roomy values are more likely, so most
/// runs get far enough to reach the arithmetic and the calls.
fn small_limits(rng: &mut Rng) -> Limits {
    Limits {
        max_steps: rng.pick(&[0, 1, 64, 20_000, 20_000, 20_000]),
        max_frames: rng.pick(&[0, 1, 64, 64, 64]),
        max_heap_bytes: rng.pick(&[0, 64, 1 << 20, 1 << 20, 1 << 20]),
        max_text_bytes: rng.pick(&[0, 16, 1 << 16, 1 << 16]),
    }
}

#[derive(Debug, Default)]
struct Tally {
    rejected: usize,
    completed: usize,
    errors: HashSet<String>,
}

#[test]
fn random_ir_never_breaks_compiler_or_vm() {
    let mut faults = Vec::new();
    let mut tally = Tally::default();
    for case in 0..RANDOM_PROGRAMS {
        let mut rng = Rng(SEED ^ case.wrapping_mul(0x1000_0000_01b3));
        let program = Gen::program(&mut rng);
        let image = match panic::catch_unwind(AssertUnwindSafe(|| compile(&program))) {
            Err(_) => {
                faults.push(format!("case {case}: compile panicked on {program:?}"));
                continue;
            }
            Ok(Err(e)) => {
                if e.to_string().is_empty() {
                    faults.push(format!("case {case}: compile error without a message"));
                }
                tally.rejected += 1;
                continue;
            }
            Ok(Ok(image)) => image,
        };
        let known = spans(&program);
        for _ in 0..3 {
            let limits = small_limits(&mut rng);
            let Some(first) = guarded_run(&image, limits) else {
                faults.push(format!("case {case}: run panicked with {limits:?}"));
                break;
            };
            match &first.1 {
                Ok(()) => tally.completed += 1,
                Err(e) => {
                    tally.errors.insert(e.kind.name().to_string());
                    if !known.contains(&e.span) {
                        faults.push(format!(
                            "case {case}: {:?} at {:?}, a span the program does not have",
                            e.kind, e.span
                        ));
                    }
                }
            }
            if guarded_run(&image, limits).as_ref() != Some(&first) {
                faults.push(format!("case {case}: two runs disagree with {limits:?}"));
            }
        }
    }
    assert!(faults.is_empty(), "VM faults:\n{}", faults.join("\n"));
    // The generator must keep reaching both stages and every limit, or this test has
    // silently stopped testing the VM.
    let runs = RANDOM_PROGRAMS as usize - tally.rejected;
    assert!(
        runs >= 400,
        "only {runs} random programs compiled: {tally:?}"
    );
    assert!(
        tally.rejected >= 400,
        "too few malformed programs: {tally:?}"
    );
    assert!(tally.completed >= 100, "too few completed runs: {tally:?}");
    for kind in [
        "StepLimit",
        "CallDepth",
        "HeapLimit",
        "TextLimit",
        "IntOverflow",
    ] {
        assert!(
            tally.errors.contains(kind),
            "no random run hit {kind}: {tally:?}"
        );
    }
}

// Programs with the shape of the `run_` corpus cases, built by hand.

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

fn func(name: &str, params: u32, locals: &[Ty], result: Ty, blocks: Vec<Block>) -> Function {
    Function {
        name: name.to_string(),
        params,
        locals: locals.to_vec(),
        result,
        blocks,
        span: sp(9_000),
    }
}

/// `fn main()` calling function 1 once with no arguments, at span 100.
fn main_calls(dst: Option<(Ty, u32)>) -> Function {
    let mut insts = vec![at(
        100,
        InstKind::Call {
            dst: dst.map(|(_, l)| Local(l)),
            func: FuncId(1),
            args: vec![],
        },
    )];
    if let Some((_, l)) = dst {
        insts.push(at(110, InstKind::Print { arg: Local(l) }));
    }
    let locals: Vec<Ty> = dst.map(|(t, _)| vec![t]).unwrap_or_default();
    func(
        "main",
        0,
        &locals,
        Ty::Unit,
        vec![Block {
            insts,
            term: Terminator::Return(None),
        }],
    )
}

/// `fn spin()` that jumps to itself forever: `run_long_loop_by_recursion` without frames.
fn spin_forever() -> Program {
    let spin = func(
        "spin",
        0,
        &[],
        Ty::Unit,
        vec![Block {
            insts: vec![],
            term: Terminator::Jump(BlockId(0)),
        }],
    );
    Program {
        functions: vec![main_calls(None), spin],
        main: FuncId(0),
    }
}

/// `fn grow(s: Text) -> Text { return grow("{s}{s}") }`: `run_text_growth` without a base
/// case, and with `hold` set, every frame keeps its text alive (`run_heap_growth`).
fn text_growth(hold: bool) -> Program {
    let seed = vec![
        at(
            10,
            InstKind::Const {
                dst: Local(0),
                value: Const::Text("abcdefgh".into()),
            },
        ),
        at(
            20,
            InstKind::Call {
                dst: Some(Local(1)),
                func: FuncId(2),
                args: vec![Local(0)],
            },
        ),
    ];
    let starter = func(
        "start",
        0,
        &[Ty::Text, Ty::Text],
        Ty::Text,
        vec![Block {
            insts: seed,
            term: Terminator::Return(Some(Local(1))),
        }],
    );
    let mut body = vec![at(
        30,
        InstKind::Concat {
            dst: Local(1),
            parts: if hold {
                vec![Local(0), Local(0)]
            } else {
                vec![Local(0), Local(0), Local(0), Local(0)]
            },
        },
    )];
    body.push(at(
        40,
        InstKind::Call {
            dst: Some(Local(2)),
            func: FuncId(2),
            args: vec![Local(1)],
        },
    ));
    let grow = func(
        "grow",
        1,
        &[Ty::Text, Ty::Text, Ty::Text],
        Ty::Text,
        vec![Block {
            insts: body,
            term: Terminator::Return(Some(Local(2))),
        }],
    );
    Program {
        functions: vec![main_calls(Some((Ty::Text, 0))), starter, grow],
        main: FuncId(0),
    }
}

/// `fn ping() { pong() }`, `fn pong() { ping() }`: `run_mutual_recursion`.
fn mutual_recursion() -> Program {
    let call = |to: u32, start: u32| {
        func(
            "pp",
            0,
            &[],
            Ty::Unit,
            vec![Block {
                insts: vec![at(
                    start,
                    InstKind::Call {
                        dst: None,
                        func: FuncId(to),
                        args: vec![],
                    },
                )],
                term: Terminator::Return(None),
            }],
        )
    };
    Program {
        functions: vec![main_calls(None), call(2, 200), call(1, 300)],
        main: FuncId(0),
    }
}

/// A function with `n` locals of `Text` calling itself: frames are large, so the heap
/// limit must stop the recursion before the frame limit or the native memory does.
fn wide_frames(n: usize) -> Program {
    let locals = vec![Ty::Text; n];
    let rec = func(
        "rec",
        0,
        &locals,
        Ty::Unit,
        vec![Block {
            insts: vec![at(
                500,
                InstKind::Call {
                    dst: None,
                    func: FuncId(1),
                    args: vec![],
                },
            )],
            term: Terminator::Return(None),
        }],
    );
    Program {
        functions: vec![main_calls(None), rec],
        main: FuncId(0),
    }
}

/// `main` printing the result of an `Int` operation on the bounds of D24.
fn overflow(op: BinOp, lhs: i64, rhs: i64) -> Program {
    let insts = vec![
        at(
            10,
            InstKind::Const {
                dst: Local(0),
                value: Const::Int(lhs),
            },
        ),
        at(
            20,
            InstKind::Const {
                dst: Local(1),
                value: Const::Int(rhs),
            },
        ),
        at(
            30,
            InstKind::Binary {
                dst: Local(2),
                op,
                lhs: Local(0),
                rhs: Local(1),
            },
        ),
        at(40, InstKind::Print { arg: Local(2) }),
    ];
    Program {
        functions: vec![func(
            "main",
            0,
            &[Ty::Int, Ty::Int, Ty::Int],
            Ty::Unit,
            vec![Block {
                insts,
                term: Terminator::Return(None),
            }],
        )],
        main: FuncId(0),
    }
}

fn image(program: &Program) -> Image {
    compile(program).unwrap_or_else(|e| panic!("a hand built program must compile: {e}"))
}

/// Extreme limits, including zero and the largest values. The step limit bounds every
/// run, so even unbounded frames and heap end.
fn extreme_limits() -> Vec<Limits> {
    let d = Limits::default();
    vec![
        Limits {
            max_steps: 0,
            max_frames: 0,
            max_heap_bytes: 0,
            max_text_bytes: 0,
        },
        Limits { max_steps: 1, ..d },
        Limits { max_frames: 1, ..d },
        Limits {
            max_heap_bytes: 1,
            ..d
        },
        Limits {
            max_text_bytes: 1,
            ..d
        },
        Limits {
            max_steps: 2_000_000,
            max_frames: u32::MAX,
            max_heap_bytes: 256 * 1024 * 1024,
            max_text_bytes: u64::MAX,
        },
        Limits {
            max_steps: 2_000_000,
            max_frames: 10_000,
            max_heap_bytes: u64::MAX,
            max_text_bytes: 16 * 1024 * 1024,
        },
    ]
}

#[test]
fn corpus_shapes_end_in_a_runtime_error_under_every_limit() {
    let cases = [
        ("spin_forever", spin_forever()),
        ("text_growth", text_growth(false)),
        ("heap_growth", text_growth(true)),
        ("mutual_recursion", mutual_recursion()),
        ("wide_frames", wide_frames(1_024)),
    ];
    for (name, program) in &cases {
        let image = image(program);
        let known = spans(program);
        for limits in extreme_limits() {
            let Some((lines, result)) = guarded_run(&image, limits) else {
                panic!("{name}: the VM panicked with {limits:?}");
            };
            let Err(e) = result else {
                panic!("{name}: an endless program ended with Ok under {limits:?}");
            };
            assert!(
                known.contains(&e.span),
                "{name}: {:?} at {:?}, a span the program does not have",
                e.kind,
                e.span
            );
            assert!(lines.len() <= 1, "{name}: printed {} lines", lines.len());
        }
    }
}

#[test]
fn endless_programs_stop_at_the_call_in_main() {
    // SPEC 12.4 (D83): `StepLimit` names the call in `main` that is running, so the
    // position does not depend on the limit. `spin` never returns and never allocates.
    let program = spin_forever();
    let image = image(&program);
    for max_steps in [1, 2, 3, 1_000, 1_000_001] {
        let limits = Limits {
            max_steps,
            ..Limits::default()
        };
        let Some((_, result)) = guarded_run(&image, limits) else {
            panic!("the VM panicked with {max_steps} steps");
        };
        let e = result.err();
        assert_eq!(
            e.map(|e| (e.kind, e.span)),
            Some((RuntimeErrorKind::StepLimit, sp(100))),
            "max_steps {max_steps}"
        );
    }
}

#[test]
fn unbounded_recursion_is_stopped_by_a_limit_not_by_memory() {
    // 4 096 `Text` registers per frame and no frame limit: only the heap limit of 256 MiB
    // (D44) stops the recursion, long before the native memory would.
    let image = image(&wide_frames(4_096));
    let limits = Limits {
        max_steps: u64::MAX,
        max_frames: u32::MAX,
        ..Limits::default()
    };
    let Some((_, result)) = guarded_run(&image, limits) else {
        panic!("the VM panicked");
    };
    assert_eq!(
        result.err().map(|e| e.kind),
        Some(RuntimeErrorKind::HeapLimit)
    );
    // The same program with the default limits ends in a limit as well.
    let Some((_, result)) = guarded_run(&image, Limits::default()) else {
        panic!("the VM panicked");
    };
    let kind = result.err().map(|e| e.kind);
    assert!(
        matches!(
            kind,
            Some(RuntimeErrorKind::CallDepth | RuntimeErrorKind::HeapLimit)
        ),
        "{kind:?}"
    );
}

#[test]
fn text_doubling_is_stopped_before_the_allocation() {
    // `run_text_growth`: without the text limit the heap limit must still stop it, and
    // without both, the step limit (weighted by bytes, D94) must.
    let image = image(&text_growth(false));
    let d = Limits::default();
    let cases = [
        (d, RuntimeErrorKind::TextLimit),
        (
            Limits {
                max_text_bytes: u64::MAX,
                ..d
            },
            RuntimeErrorKind::HeapLimit,
        ),
        (
            Limits {
                max_text_bytes: u64::MAX,
                max_heap_bytes: u64::MAX,
                max_steps: 1_000_000,
                ..d
            },
            RuntimeErrorKind::StepLimit,
        ),
    ];
    for (limits, want) in cases {
        let Some((lines, result)) = guarded_run(&image, limits) else {
            panic!("the VM panicked with {limits:?}");
        };
        assert!(lines.is_empty());
        assert_eq!(result.err().map(|e| e.kind), Some(want), "{limits:?}");
    }
}

#[test]
fn int_bounds_overflow_instead_of_wrapping() {
    let cases = [
        (BinOp::Add, INT_MAX, 1),
        (BinOp::Sub, INT_MIN, 1),
        (BinOp::Mul, INT_MAX, 2),
        (BinOp::Mul, INT_MIN, 2),
        (BinOp::Mul, INT_MAX, INT_MAX),
        (BinOp::Mul, INT_MIN, INT_MIN),
    ];
    for (op, lhs, rhs) in cases {
        let Some((lines, result)) = guarded_run(&image(&overflow(op, lhs, rhs)), Limits::default())
        else {
            panic!("the VM panicked on {lhs} {op:?} {rhs}");
        };
        assert!(lines.is_empty(), "{lhs} {op:?} {rhs} printed {lines:?}");
        assert_eq!(
            result.err().map(|e| (e.kind, e.span)),
            Some((RuntimeErrorKind::IntOverflow, sp(30))),
            "{lhs} {op:?} {rhs}"
        );
    }
    let quiet = [
        (BinOp::Div, INT_MIN, -1, INT_MAX),
        (BinOp::Rem, INT_MIN, -1, 0),
        (BinOp::Add, INT_MAX, INT_MIN, 0),
        (BinOp::Sub, INT_MIN, INT_MIN, 0),
    ];
    for (op, lhs, rhs, want) in quiet {
        let Some((lines, result)) = guarded_run(&image(&overflow(op, lhs, rhs)), Limits::default())
        else {
            panic!("the VM panicked on {lhs} {op:?} {rhs}");
        };
        assert_eq!(result, Ok(()), "{lhs} {op:?} {rhs}");
        assert_eq!(lines, [want.to_string()], "{lhs} {op:?} {rhs}");
    }
    for (op, zero) in [(BinOp::Div, 0), (BinOp::Rem, 0)] {
        let Some((_, result)) =
            guarded_run(&image(&overflow(op, INT_MIN, zero)), Limits::default())
        else {
            panic!("the VM panicked on {op:?} by zero");
        };
        assert_eq!(
            result.err().map(|e| e.kind),
            Some(RuntimeErrorKind::DivisionByZero)
        );
    }
}

#[test]
fn malformed_entry_and_huge_tables_are_compile_errors_or_run() {
    // Shapes a corrupted or hostile IR could take that the enumerated cases of `vm.rs`
    // do not name: an entry index far out of range, a function with a million locals,
    // a block that jumps to the largest block id, and a call with u32::MAX as callee.
    let ret = |insts: Vec<Inst>| Block {
        insts,
        term: Terminator::Return(None),
    };
    let far_main = Program {
        functions: vec![func("main", 0, &[], Ty::Unit, vec![ret(vec![])])],
        main: FuncId(u32::MAX),
    };
    let jump_far = Program {
        functions: vec![func(
            "main",
            0,
            &[],
            Ty::Unit,
            vec![Block {
                insts: vec![],
                term: Terminator::Jump(BlockId(u32::MAX)),
            }],
        )],
        main: FuncId(0),
    };
    let call_far = Program {
        functions: vec![func(
            "main",
            0,
            &[],
            Ty::Unit,
            vec![ret(vec![at(
                1,
                InstKind::Call {
                    dst: None,
                    func: FuncId(u32::MAX),
                    args: vec![],
                },
            )])],
        )],
        main: FuncId(0),
    };
    let local_far = Program {
        functions: vec![func(
            "main",
            0,
            &[Ty::Int],
            Ty::Unit,
            vec![ret(vec![at(
                1,
                InstKind::Print {
                    arg: Local(u32::MAX),
                },
            )])],
        )],
        main: FuncId(0),
    };
    let params_far = Program {
        functions: vec![
            func("main", 0, &[], Ty::Unit, vec![ret(vec![])]),
            func("f", u32::MAX, &[Ty::Int], Ty::Unit, vec![ret(vec![])]),
        ],
        main: FuncId(0),
    };
    for (name, p) in [
        ("far_main", far_main),
        ("jump_far", jump_far),
        ("call_far", call_far),
        ("local_far", local_far),
        ("params_far", params_far),
    ] {
        match panic::catch_unwind(AssertUnwindSafe(|| compile(&p))) {
            Ok(Err(e)) => assert!(!e.to_string().is_empty(), "{name}"),
            Ok(Ok(_)) => panic!("{name}: malformed IR compiled"),
            Err(_) => panic!("{name}: compile panicked"),
        }
    }
    // A million `Int` locals in `main` is well formed; the frame is charged to the heap
    // and either fits or ends in `HeapLimit`, never in an abort.
    let huge = Program {
        functions: vec![func(
            "main",
            0,
            &vec![Ty::Int; 1_000_000],
            Ty::Unit,
            vec![ret(vec![at(
                1,
                InstKind::Print {
                    arg: Local(999_999),
                },
            )])],
        )],
        main: FuncId(0),
    };
    let image = match panic::catch_unwind(AssertUnwindSafe(|| compile(&huge))) {
        Ok(Ok(image)) => image,
        Ok(Err(e)) => panic!("a million locals are well formed: {e}"),
        Err(_) => panic!("compile panicked on a million locals"),
    };
    for limits in extreme_limits() {
        let Some((lines, result)) = guarded_run(&image, limits) else {
            panic!("the VM panicked on a million locals with {limits:?}");
        };
        match result {
            Ok(()) => assert_eq!(lines, ["0"]),
            Err(e) => assert!(
                matches!(
                    e.kind,
                    RuntimeErrorKind::HeapLimit
                        | RuntimeErrorKind::StepLimit
                        | RuntimeErrorKind::CallDepth
                ),
                "{e:?} with {limits:?}"
            ),
        }
    }
}
