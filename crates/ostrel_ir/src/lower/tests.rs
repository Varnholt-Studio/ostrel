// Test code may panic on a broken invariant.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use ostrel_core::FileId;
use ostrel_syntax::ast::{
    AstError, BinaryOp, ExprId, ExprKind, Module, StrPart, TextRange, UnaryOp,
};

use super::*;

type R = Result<(), AstError>;

/// A value of the evaluator below.
#[derive(Debug, Clone, PartialEq, Eq)]
enum V {
    Int(i64),
    Text(String),
    Bool(bool),
}

impl V {
    fn text(&self) -> String {
        match self {
            V::Int(n) => n.to_string(),
            V::Text(t) => t.clone(),
            V::Bool(b) => b.to_string(),
        }
    }
}

/// Reference evaluator for lowered programs: enough to observe evaluation
/// order and control flow. It has no limits; the cases are small.
fn eval(program: &Program, func: FuncId, args: Vec<V>, out: &mut Vec<String>) -> Option<V> {
    let f = program.function(func).unwrap();
    assert_eq!(args.len(), f.params as usize);
    let mut regs: Vec<Option<V>> = vec![None; f.locals.len()];
    for (i, a) in args.into_iter().enumerate() {
        regs[i] = Some(a);
    }
    let get = |regs: &Vec<Option<V>>, l: Local| regs[l.0 as usize].clone().unwrap();
    let mut block = BlockId(0);
    loop {
        let b = f.block(block).unwrap();
        for inst in &b.insts {
            match &inst.kind {
                InstKind::Const { dst, value } => {
                    regs[dst.0 as usize] = Some(match value {
                        Const::Int(n) => V::Int(*n),
                        Const::Text(t) => V::Text(t.clone()),
                        Const::Bool(b) => V::Bool(*b),
                    })
                }
                InstKind::Copy { dst, src } => regs[dst.0 as usize] = Some(get(&regs, *src)),
                InstKind::Unary { dst, op, arg } => {
                    regs[dst.0 as usize] = Some(match (op, get(&regs, *arg)) {
                        (UnOp::Neg, V::Int(n)) => V::Int(-n),
                        (UnOp::Not, V::Bool(b)) => V::Bool(!b),
                        other => panic!("bad unary {other:?}"),
                    })
                }
                InstKind::Binary { dst, op, lhs, rhs } => {
                    let (l, r) = (get(&regs, *lhs), get(&regs, *rhs));
                    let v = match (op, &l, &r) {
                        (BinOp::Eq, ..) => V::Bool(l == r),
                        (BinOp::Ne, ..) => V::Bool(l != r),
                        (op, V::Int(a), V::Int(b)) => match op {
                            BinOp::Add => V::Int(a + b),
                            BinOp::Sub => V::Int(a - b),
                            BinOp::Mul => V::Int(a * b),
                            BinOp::Div => V::Int(a / b),
                            BinOp::Rem => V::Int(a % b),
                            BinOp::Lt => V::Bool(a < b),
                            BinOp::Le => V::Bool(a <= b),
                            BinOp::Gt => V::Bool(a > b),
                            BinOp::Ge => V::Bool(a >= b),
                            BinOp::Eq | BinOp::Ne => unreachable!(),
                        },
                        other => panic!("bad binary {other:?}"),
                    };
                    assert_eq!(f.local_ty(*dst), Some(op.result_ty()));
                    regs[dst.0 as usize] = Some(v);
                }
                InstKind::ToText { dst, src } => {
                    regs[dst.0 as usize] = Some(V::Text(get(&regs, *src).text()))
                }
                InstKind::Concat { dst, parts } => {
                    let mut s = String::new();
                    for p in parts {
                        let V::Text(t) = get(&regs, *p) else {
                            panic!("concat of non text")
                        };
                        s.push_str(&t);
                    }
                    regs[dst.0 as usize] = Some(V::Text(s));
                }
                InstKind::Call { dst, func, args } => {
                    let args = args.iter().map(|a| get(&regs, *a)).collect();
                    let r = eval(program, *func, args, out);
                    if let Some(dst) = dst {
                        regs[dst.0 as usize] = r;
                    }
                }
                InstKind::Print { arg } => out.push(get(&regs, *arg).text()),
            }
        }
        match &b.term {
            Terminator::Jump(next) => block = *next,
            Terminator::Branch {
                cond,
                then,
                otherwise,
            } => {
                assert_eq!(f.local_ty(*cond), Some(Ty::Bool));
                block = if get(&regs, *cond) == V::Bool(true) {
                    *then
                } else {
                    *otherwise
                };
            }
            Terminator::Return(value) => {
                assert_eq!(value.is_none(), f.result == Ty::Unit);
                return value.map(|v| get(&regs, v));
            }
        }
    }
}

fn file() -> FileId {
    FileId::from_raw(3)
}

/// Checks, lowers and runs `module`; returns the printed lines.
fn run(module: &Module) -> (Program, Vec<String>) {
    let (checked, diags) = ostrel_sema::check(module, file(), "");
    assert!(diags.is_empty(), "{diags:?}");
    let program = lower(module, &checked, file()).unwrap();
    let mut out = Vec::new();
    eval(&program, program.main, Vec::new(), &mut out);
    (program, out)
}

fn print(m: &mut Module, arg: ExprId) -> Result<ostrel_syntax::ast::StmtId, AstError> {
    let call = m.call("print", vec![arg])?;
    m.expr_stmt(call)
}

#[test]
fn hello_world() -> R {
    let mut m = Module::new();
    let hello = m.text("hello")?;
    let s = print(&mut m, hello)?;
    let body = m.block_of(vec![s])?;
    m.fn_item("main", &[], None, body)?;
    let (program, out) = run(&m);
    assert_eq!(out, ["hello"]);
    let main = program.function(program.main).unwrap();
    assert_eq!(main.name, "main");
    assert_eq!(main.result, Ty::Unit);
    assert_eq!(main.blocks.len(), 1);
    Ok(())
}

#[test]
fn calls_arithmetic_and_implicit_result() -> R {
    let mut m = Module::new();
    // fn sq(n: Int) -> Int { n * n }
    let (a, b) = (m.name("n")?, m.name("n")?);
    let mul = m.binary(BinaryOp::Mul, a, b)?;
    let s = m.expr_stmt(mul)?;
    let body = m.block_of(vec![s])?;
    m.fn_item("sq", &[("n", "Int")], Some("Int"), body)?;
    // fn main() { print(sq(-7) / 2)  print(-7 % 3)  print(7 - 2 - 1) }
    let seven = m.int(7)?;
    let neg = m.unary(UnaryOp::Neg, seven)?;
    let call = m.call("sq", vec![neg])?;
    let two = m.int(2)?;
    let div = m.binary(BinaryOp::Div, call, two)?;
    let s1 = print(&mut m, div)?;
    let seven = m.int(7)?;
    let neg = m.unary(UnaryOp::Neg, seven)?;
    let three = m.int(3)?;
    let rem = m.binary(BinaryOp::Rem, neg, three)?;
    let s2 = print(&mut m, rem)?;
    let (x, y, z) = (m.int(7)?, m.int(2)?, m.int(1)?);
    let first = m.binary(BinaryOp::Sub, x, y)?;
    let chain = m.binary(BinaryOp::Sub, first, z)?;
    let s3 = print(&mut m, chain)?;
    let body = m.block_of(vec![s1, s2, s3])?;
    m.fn_item("main", &[], None, body)?;
    let (program, out) = run(&m);
    assert_eq!(out, ["24", "-1", "4"]);
    assert_eq!(program.main, FuncId(1));
    let sq = program.function(FuncId(0)).unwrap();
    assert_eq!((sq.params, sq.result), (1, Ty::Int));
    assert_eq!(sq.local_ty(Local(0)), Some(Ty::Int));
    Ok(())
}

#[test]
fn let_and_interpolation() -> R {
    let mut m = Module::new();
    // let x = 5   let t = "a"   print("x={x} b={x > 1} t={t}")
    let five = m.int(5)?;
    let let_x = m.let_stmt("x", five)?;
    let a = m.text("a")?;
    let let_t = m.let_stmt("t", a)?;
    let x1 = m.name("x")?;
    let x2 = m.name("x")?;
    let one = m.int(1)?;
    let gt = m.binary(BinaryOp::Gt, x2, one)?;
    let t = m.name("t")?;
    let parts = vec![
        StrPart::Text("x=".into()),
        StrPart::Interp(x1),
        StrPart::Text(" b=".into()),
        StrPart::Interp(gt),
        StrPart::Text(" t=".into()),
        StrPart::Interp(t),
    ];
    let s = m.string(parts)?;
    let p = print(&mut m, s)?;
    let empty = m.text("")?;
    let p2 = print(&mut m, empty)?;
    let body = m.block_of(vec![let_x, let_t, p, p2])?;
    m.fn_item("main", &[], None, body)?;
    let (program, out) = run(&m);
    assert_eq!(out, ["x=5 b=true t=a", ""]);
    // The `Text` interpolation needs no conversion: two `ToText`, not three.
    let main = program.function(program.main).unwrap();
    let to_text = main.blocks[0]
        .insts
        .iter()
        .filter(|i| matches!(i.kind, InstKind::ToText { .. }))
        .count();
    assert_eq!(to_text, 2);
    Ok(())
}

/// fn loud(b: Bool) -> Bool { print("called")  b }
fn loud(m: &mut Module) -> R {
    let called = m.text("called")?;
    let s = print(m, called)?;
    let b = m.name("b")?;
    let r = m.expr_stmt(b)?;
    let body = m.block_of(vec![s, r])?;
    m.fn_item("loud", &[("b", "Bool")], Some("Bool"), body)
}

#[test]
fn and_or_short_circuit() -> R {
    let mut m = Module::new();
    loud(&mut m)?;
    let mut stmts = Vec::new();
    for (op, left, right) in [
        (BinaryOp::And, false, true),
        (BinaryOp::Or, true, false),
        (BinaryOp::And, true, false),
        (BinaryOp::Or, false, true),
    ] {
        let l = m.bool(left)?;
        let r = m.bool(right)?;
        let call = m.call("loud", vec![r])?;
        let e = m.binary(op, l, call)?;
        stmts.push(print(&mut m, e)?);
    }
    let body = m.block_of(stmts)?;
    m.fn_item("main", &[], None, body)?;
    let (_, out) = run(&m);
    assert_eq!(out, ["false", "true", "called", "false", "called", "true"]);
    Ok(())
}

/// fn fact(n: Int) -> Int { if n <= 1 { return 1 } else { return n * fact(n - 1) } }
fn fact(m: &mut Module) -> R {
    let n = m.name("n")?;
    let one = m.int(1)?;
    let cond = m.binary(BinaryOp::Le, n, one)?;
    let one = m.int(1)?;
    let ret1 = m.return_stmt(one)?;
    let then_block = m.block_of(vec![ret1])?;
    let n = m.name("n")?;
    let n2 = m.name("n")?;
    let one = m.int(1)?;
    let sub = m.binary(BinaryOp::Sub, n2, one)?;
    let rec = m.call("fact", vec![sub])?;
    let mul = m.binary(BinaryOp::Mul, n, rec)?;
    let ret2 = m.return_stmt(mul)?;
    let else_block = m.block_of(vec![ret2])?;
    let s = m.if_stmt(cond, then_block, Some(else_block))?;
    let body = m.block_of(vec![s])?;
    m.fn_item("fact", &[("n", "Int")], Some("Int"), body)
}

#[test]
fn if_else_with_returns_has_no_join_block() -> R {
    let mut m = Module::new();
    // main is declared first: functions are visible regardless of order.
    let ten = m.int(10)?;
    let call = m.call("fact", vec![ten])?;
    let s = print(&mut m, call)?;
    let body = m.block_of(vec![s])?;
    m.fn_item("main", &[], None, body)?;
    fact(&mut m)?;
    let (program, out) = run(&m);
    assert_eq!(out, ["3628800"]);
    let fact = program.function(FuncId(1)).unwrap();
    assert_eq!(fact.blocks.len(), 3);
    Ok(())
}

#[test]
fn if_without_else_joins() -> R {
    let mut m = Module::new();
    // fn main() { let x = 3  if x > 2 { print("big") }  if not (x > 2) { print("small") }  print("end") }
    let three = m.int(3)?;
    let let_x = m.let_stmt("x", three)?;
    let mut stmts = vec![let_x];
    for (negate, word) in [(false, "big"), (true, "small")] {
        let x = m.name("x")?;
        let two = m.int(2)?;
        let mut cond = m.binary(BinaryOp::Gt, x, two)?;
        if negate {
            let paren = m.paren(cond)?;
            cond = m.unary(UnaryOp::Not, paren)?;
        }
        let text = m.text(word)?;
        let p = print(&mut m, text)?;
        let then_block = m.block_of(vec![p])?;
        stmts.push(m.if_stmt(cond, then_block, None)?);
    }
    let end = m.text("end")?;
    stmts.push(print(&mut m, end)?);
    let body = m.block_of(stmts)?;
    m.fn_item("main", &[], None, body)?;
    let (_, out) = run(&m);
    assert_eq!(out, ["big", "end"]);
    Ok(())
}

#[test]
fn spans_carry_file_and_range() -> R {
    let mut m = Module::new();
    let a = m.add_expr(ExprKind::Int(1), TextRange::new(16, 17).unwrap())?;
    let b = m.add_expr(ExprKind::Int(0), TextRange::new(20, 21).unwrap())?;
    let kind = ExprKind::Binary {
        op: BinaryOp::Div,
        lhs: a,
        rhs: b,
    };
    let div = m.add_expr(kind, TextRange::new(16, 21).unwrap())?;
    let s = print(&mut m, div)?;
    let body = m.block_of(vec![s])?;
    m.fn_item("main", &[], None, body)?;
    let (checked, diags) = ostrel_sema::check(&m, file(), "");
    assert!(diags.is_empty(), "{diags:?}");
    let program = lower(&m, &checked, file()).unwrap();
    let main = program.function(program.main).unwrap();
    let div = main.blocks[0]
        .insts
        .iter()
        .find(|i| matches!(i.kind, InstKind::Binary { .. }))
        .unwrap();
    let expected = Span {
        file: 3,
        start: 16,
        end: 21,
    };
    assert_eq!(div.span, expected);
    Ok(())
}

#[test]
fn unchecked_input_is_an_error_not_a_panic() -> R {
    let mut m = Module::new();
    let n = m.name("nowhere")?;
    let s = print(&mut m, n)?;
    let body = m.block_of(vec![s])?;
    m.fn_item("main", &[], None, body)?;
    // No tables at all.
    let empty = ostrel_sema::Checked::default();
    assert_eq!(lower(&m, &empty, file()), Err(LowerError::NoMain));
    // Tables of a checker run that reported an error.
    let (checked, diags) = ostrel_sema::check(&m, file(), "");
    assert!(!diags.is_empty());
    assert!(matches!(
        lower(&m, &checked, file()),
        Err(LowerError::Unchecked(_))
    ));
    Ok(())
}
