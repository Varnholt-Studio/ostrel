use ostrel_core::{FileId, SourceMap};
use ostrel_syntax::ast::{
    BinaryOp, BlockId, ExprId, ExprKind, FnDecl, Ident, Module, Param, StmtId, StmtKind, StrPart,
    TextRange, TypeRef, UnaryOp,
};

use super::*;

/// A piece of source text and which occurrence of it is meant.
type At<'a> = (&'a str, usize);

/// A unit test case: description, tree builder, expected codes.
type Case = (&'static str, fn(&mut Module) -> ExprId, Vec<u16>);

/// Builds a tree for a real source text; every node range is the `nth`
/// occurrence (counted from 0) of a piece of that text.
struct B {
    src: &'static str,
    m: Module,
}

impl B {
    fn new(src: &'static str) -> B {
        B {
            src,
            m: Module::new(),
        }
    }

    /// Occurrences count whole words only: a needle that starts or ends with
    /// a name character does not match inside a longer name.
    fn r(&self, needle: &str, nth: usize) -> TextRange {
        let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let (start, _) = self
            .src
            .match_indices(needle)
            .filter(|&(i, _)| {
                let before = self.src[..i].chars().next_back();
                let after = self.src[i + needle.len()..].chars().next();
                let cut_front = word(needle.chars().next()) && word(before);
                let cut_back = word(needle.chars().next_back()) && word(after);
                !cut_front && !cut_back
            })
            .nth(nth)
            .unwrap();
        TextRange::new(start as u32, (start + needle.len()) as u32).unwrap()
    }

    fn ident(&self, name: &str, nth: usize) -> Ident {
        Ident::new(name, self.r(name, nth))
    }

    fn e(&mut self, kind: ExprKind, needle: &str, nth: usize) -> ExprId {
        let range = self.r(needle, nth);
        self.m.add_expr(kind, range).unwrap()
    }

    fn int(&mut self, text: &str, nth: usize) -> ExprId {
        self.e(ExprKind::Int(text.parse().unwrap()), text, nth)
    }

    fn name(&mut self, name: &str, nth: usize) -> ExprId {
        let ident = self.ident(name, nth);
        self.e(ExprKind::Name(ident), name, nth)
    }

    fn text(&mut self, value: &str, nth: usize) -> ExprId {
        let quoted = format!("\"{value}\"");
        let parts = Box::new([StrPart::Text(value.into())]);
        self.e(ExprKind::Str(parts), &quoted, nth)
    }

    fn call(&mut self, callee: &str, nth: usize, args: Vec<ExprId>, whole: &str) -> ExprId {
        let callee = self.name(callee, nth);
        let kind = ExprKind::Call {
            callee,
            args: args.into_boxed_slice(),
        };
        let range = self.r(whole, 0);
        self.m.add_expr(kind, range).unwrap()
    }

    fn bin(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, whole: &str) -> ExprId {
        self.e(ExprKind::Binary { op, lhs, rhs }, whole, 0)
    }

    fn stmt(&mut self, kind: StmtKind) -> StmtId {
        self.m.add_stmt(kind, TextRange::default()).unwrap()
    }

    fn block(&mut self, stmts: Vec<StmtId>) -> BlockId {
        self.m.block_of(stmts).unwrap()
    }

    /// `params` and `result` name the text and occurrence of each name.
    fn func(
        &mut self,
        name: At<'_>,
        params: &[(At<'_>, At<'_>)],
        result: Option<At<'_>>,
        body: BlockId,
    ) {
        let params = params
            .iter()
            .map(|&((n, i), (t, j))| Param::new(self.ident(n, i), TypeRef::named(self.ident(t, j))))
            .collect();
        let decl = FnDecl {
            name: self.ident(name.0, name.1),
            params,
            result: result.map(|(t, j)| TypeRef::named(self.ident(t, j))),
            body,
            range: TextRange::default(),
        };
        self.m.add_fn(decl).unwrap();
    }

    /// `fn main()` whose body is `print("ok")`; `nth` counts `main` and `print`.
    fn main_print_ok(&mut self, nth: usize) {
        let ok = self.text("ok", 0);
        let call = self.call("print", nth, vec![ok], "print(\"ok\")");
        let s = self.stmt(StmtKind::Expr(call));
        let body = self.block(vec![s]);
        self.func(("main", 0), &[], None, body);
    }

    /// Checks the tree and renders the diagnostics like the CLI does.
    fn render(self, path: &str) -> String {
        let mut map = SourceMap::new();
        let file = map.add(path, self.src).unwrap();
        let (_, diags) = check(&self.m, file, self.src);
        diags.iter().map(|d| d.render(&map) + "\n").collect()
    }
}

macro_rules! golden {
    ($name:literal, $build:expr) => {{
        let src = include_str!(concat!("../../../tests/errors/", $name, ".ostl"));
        let expected = include_str!(concat!("../../../tests/errors/", $name, ".expected_err"));
        let mut b = B::new(src);
        let build: fn(&mut B) = $build;
        build(&mut b);
        assert_eq!(b.render(concat!("tests/errors/", $name, ".ostl")), expected);
    }};
}

fn ret(b: &mut B, value: ExprId) -> BlockId {
    let s = b.stmt(StmtKind::Return { value });
    b.block(vec![s])
}

#[test]
fn ac_03_golden_check_float_type_v0_1() {
    golden!("check_float_type_v0_1", |b| {
        let one = b.int("1", 0);
        let body = ret(b, one);
        b.func(
            ("unused", 0),
            &[(("n", 0), ("Float", 0))],
            Some(("Int", 0)),
            body,
        );
        b.main_print_ok(0);
    });
}

#[test]
fn ac_03_golden_entry_main_params() {
    golden!("entry_main_params", |b| {
        let x = b.name("x", 1);
        let x = b.call("print", 0, vec![x], "print(x)");
        let s = b.stmt(StmtKind::Expr(x));
        let body = b.block(vec![s]);
        b.func(("main", 0), &[(("x", 0), ("Int", 0))], None, body);
    });
}

#[test]
fn ac_03_golden_entry_main_result() {
    golden!("entry_main_result", |b| {
        let one = b.int("1", 0);
        let body = ret(b, one);
        b.func(("main", 0), &[], Some(("Int", 0)), body);
    });
}

#[test]
fn ac_03_golden_entry_missing_main() {
    golden!("entry_missing_main", |b| {
        let one = b.int("1", 0);
        let body = ret(b, one);
        b.func(("helper", 0), &[], Some(("Int", 0)), body);
    });
}

#[test]
fn ac_03_golden_name_duplicate_fn() {
    golden!("name_duplicate_fn", |b| {
        for (n, v) in [(0, "1"), (1, "2")] {
            let value = b.int(v, 0);
            let body = ret(b, value);
            b.func(("f", n), &[], Some(("Int", n)), body);
        }
        b.main_print_ok(0);
    });
}

#[test]
fn ac_03_golden_name_duplicate_param() {
    golden!("name_duplicate_param", |b| {
        let a = b.name("a", 2);
        let body = ret(b, a);
        b.func(
            ("add", 0),
            &[(("a", 0), ("Int", 0)), (("a", 1), ("Int", 1))],
            Some(("Int", 2)),
            body,
        );
        b.main_print_ok(0);
    });
}

#[test]
fn ac_03_golden_name_unknown() {
    golden!("name_unknown", |b| {
        let one = b.int("1", 0);
        let x = b.ident("x", 0);
        let l = b.stmt(StmtKind::Let {
            name: x,
            value: one,
        });
        let y = b.name("y", 0);
        let p = b.call("print", 0, vec![y], "print(y)");
        let p = b.stmt(StmtKind::Expr(p));
        let body = b.block(vec![l, p]);
        b.func(("main", 0), &[], None, body);
    });
}

#[test]
fn ac_03_golden_name_unknown_type() {
    golden!("name_unknown_type", |b| {
        let n = b.name("n", 1);
        let two = b.int("2", 0);
        let mul = b.bin(BinaryOp::Mul, n, two, "n * 2");
        let body = ret(b, mul);
        b.func(
            ("twice", 0),
            &[(("n", 0), ("Integer", 0))],
            Some(("Int", 0)),
            body,
        );
        b.main_print_ok(0);
    });
}

#[test]
fn ac_03_golden_type_arity() {
    golden!("type_arity", |b| {
        let n = b.name("n", 1);
        let one = b.int("1", 0);
        let add = b.bin(BinaryOp::Add, n, one, "n + 1");
        let body = ret(b, add);
        b.func(
            ("inc", 0),
            &[(("n", 0), ("Int", 0))],
            Some(("Int", 1)),
            body,
        );
        let one = b.int("1", 1);
        let two = b.int("2", 0);
        let inner = b.call("inc", 1, vec![one, two], "inc(1, 2)");
        let p = b.call("print", 0, vec![inner], "print(inc(1, 2))");
        let s = b.stmt(StmtKind::Expr(p));
        let body = b.block(vec![s]);
        b.func(("main", 0), &[], None, body);
    });
}

#[test]
fn ac_03_golden_type_condition_not_bool() {
    golden!("type_condition_not_bool", |b| {
        let cond = b.int("1", 0);
        let one = b.int("1", 1);
        let p = b.call("print", 0, vec![one], "print(1)");
        let p = b.stmt(StmtKind::Expr(p));
        let then_block = b.block(vec![p]);
        let s = b.stmt(StmtKind::If {
            cond,
            then_block,
            else_block: None,
        });
        let body = b.block(vec![s]);
        b.func(("main", 0), &[], None, body);
    });
}

#[test]
fn ac_03_golden_type_implicit_return_in_branch() {
    golden!("type_implicit_return_in_branch", |b| {
        let n = b.name("n", 1);
        let zero = b.int("0", 0);
        let cond = b.bin(BinaryOp::Lt, n, zero, "n < 0");
        let zero = b.int("0", 1);
        let n = b.name("n", 2);
        let neg = b.bin(BinaryOp::Sub, zero, n, "0 - n");
        let then_block = ret(b, neg);
        let n = b.name("n", 3);
        let n = b.stmt(StmtKind::Expr(n));
        let else_block = b.block(vec![n]);
        let s = b.stmt(StmtKind::If {
            cond,
            then_block,
            else_block: Some(else_block),
        });
        let body = b.block(vec![s]);
        b.func(
            ("abs", 0),
            &[(("n", 0), ("Int", 0))],
            Some(("Int", 1)),
            body,
        );
        let three = b.int("3", 0);
        let inner = b.call("abs", 1, vec![three], "abs(3)");
        let p = b.call("print", 0, vec![inner], "print(abs(3))");
        let s = b.stmt(StmtKind::Expr(p));
        let body = b.block(vec![s]);
        b.func(("main", 0), &[], None, body);
    });
}

#[test]
fn ac_03_golden_type_text_plus() {
    golden!("type_text_plus", |b| {
        let a = b.text("a", 0);
        let bb = b.text("b", 0);
        let plus = b.bin(BinaryOp::Add, a, bb, "\"a\" + \"b\"");
        let p = b.call("print", 0, vec![plus], "print(\"a\" + \"b\")");
        let s = b.stmt(StmtKind::Expr(p));
        let body = b.block(vec![s]);
        b.func(("main", 0), &[], None, body);
    });
}

// Trees built with the plain constructors: all ranges are empty, so these
// tests look at codes and tables, not at positions.

fn run(m: &Module) -> (Checked, Vec<u16>) {
    let (checked, diags) = check(m, FileId::from_raw(0), "");
    let codes = diags.iter().map(|d| d.code.number()).collect();
    (checked, codes)
}

fn main_with(m: &mut Module, stmts: Vec<StmtId>) {
    let body = m.block_of(stmts).unwrap();
    m.fn_item("main", &[], None, body).unwrap();
}

fn print_stmt(m: &mut Module, value: ExprId) -> StmtId {
    let call = m.call("print", vec![value]).unwrap();
    m.expr_stmt(call).unwrap()
}

/// `fn fib(n: Int) -> Int` as in SYNTAX 4.11, declared after `main`.
fn fib(m: &mut Module) {
    let n = m.name("n").unwrap();
    let two = m.int(2).unwrap();
    let cond = m.binary(BinaryOp::Lt, n, two).unwrap();
    let n = m.name("n").unwrap();
    let r = m.return_stmt(n).unwrap();
    let then_block = m.block_of(vec![r]).unwrap();
    let s1 = m.if_stmt(cond, then_block, None).unwrap();
    let mut calls = Vec::new();
    for k in [1, 2] {
        let n = m.name("n").unwrap();
        let k = m.int(k).unwrap();
        let arg = m.binary(BinaryOp::Sub, n, k).unwrap();
        calls.push(m.call("fib", vec![arg]).unwrap());
    }
    let sum = m.binary(BinaryOp::Add, calls[0], calls[1]).unwrap();
    let s2 = m.return_stmt(sum).unwrap();
    let body = m.block_of(vec![s1, s2]).unwrap();
    m.fn_item("fib", &[("n", "Int")], Some("Int"), body)
        .unwrap();
}

#[test]
fn ac_02_fib_program_checks_and_fills_tables() {
    let mut m = Module::new();
    let twenty = m.int(20).unwrap();
    let call = m.call("fib", vec![twenty]).unwrap();
    let let_x = m.let_stmt("x", call).unwrap();
    let x = m.name("x").unwrap();
    let msg = m
        .string(vec![StrPart::Text("fib(20) = ".into()), StrPart::Interp(x)])
        .unwrap();
    let p = print_stmt(&mut m, msg);
    main_with(&mut m, vec![let_x, p]);
    fib(&mut m);

    let (checked, codes) = run(&m);
    assert_eq!(codes, Vec::<u16>::new());
    assert_eq!(checked.main, Some(FnId(0)));
    assert_eq!(checked.lets.get(&let_x), Some(&LocalId(0)));
    assert_eq!(checked.names.get(&x), Some(&Res::Local(LocalId(0))));
    assert_eq!(checked.expr_types.get(&call), Some(&Ty::Int));
    assert_eq!(checked.expr_types.get(&msg), Some(&Ty::Text));
    let main = checked.fns[0].as_ref().unwrap();
    assert_eq!(main.locals, vec![Ty::Int]);
    assert_eq!(main.result, Ty::Unit);
    let fib = checked.fns[1].as_ref().unwrap();
    assert_eq!((fib.params.clone(), fib.result), (vec![Ty::Int], Ty::Int));
    assert_eq!(fib.implicit_return, None);
    let callees: Vec<_> = checked
        .names
        .values()
        .filter(|r| matches!(r, Res::Fn(_) | Res::Print))
        .copied()
        .collect();
    assert_eq!(
        callees.iter().filter(|r| **r == Res::Fn(FnId(1))).count(),
        3
    );
    assert_eq!(callees.iter().filter(|r| **r == Res::Print).count(), 1);
}

#[test]
fn trailing_expression_is_the_result_without_branches() {
    let mut m = Module::new();
    let n = m.name("n").unwrap();
    let one = m.int(1).unwrap();
    let sum = m.binary(BinaryOp::Add, n, one).unwrap();
    let s = m.expr_stmt(sum).unwrap();
    let body = m.block_of(vec![s]).unwrap();
    m.fn_item("inc", &[("n", "Int")], Some("Int"), body)
        .unwrap();
    main_with(&mut m, vec![]);
    let (checked, codes) = run(&m);
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(checked.fns[0].as_ref().unwrap().implicit_return, Some(sum));
}

#[test]
fn let_is_visible_only_after_it_and_inside_its_block() {
    // let x = x + 1
    let mut m = Module::new();
    let x = m.name("x").unwrap();
    let one = m.int(1).unwrap();
    let sum = m.binary(BinaryOp::Add, x, one).unwrap();
    let l = m.let_stmt("x", sum).unwrap();
    main_with(&mut m, vec![l]);
    assert_eq!(run(&m).1, vec![200]);

    // if true / let y = 1 / then print(y) after the block
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let l = m.let_stmt("y", one).unwrap();
    let then_block = m.block_of(vec![l]).unwrap();
    let t = m.bool(true).unwrap();
    let s = m.if_stmt(t, then_block, None).unwrap();
    let y = m.name("y").unwrap();
    let p = print_stmt(&mut m, y);
    main_with(&mut m, vec![s, p]);
    assert_eq!(run(&m).1, vec![200]);
}

#[test]
fn locals_shadow_functions_and_get_fresh_ids() {
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let l1 = m.let_stmt("print", one).unwrap();
    let t = m.text("a").unwrap();
    let l2 = m.let_stmt("print", t).unwrap();
    let use_ = m.name("print").unwrap();
    let l3 = m.let_stmt("z", use_).unwrap();
    main_with(&mut m, vec![l1, l2, l3]);
    let (checked, codes) = run(&m);
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(checked.names.get(&use_), Some(&Res::Local(LocalId(1))));
    let locals = &checked.fns[0].as_ref().unwrap().locals;
    assert_eq!(locals, &vec![Ty::Int, Ty::Text, Ty::Text]);
}

#[test]
fn type_errors_report_once() {
    let cases: Vec<Case> = vec![
        (
            "unknown name inside arithmetic",
            |m| {
                let y = m.name("y").unwrap();
                let one = m.int(1).unwrap();
                let s = m.binary(BinaryOp::Mul, y, one).unwrap();
                let two = m.int(2).unwrap();
                m.binary(BinaryOp::Add, s, two).unwrap()
            },
            vec![200],
        ),
        (
            "not on Int",
            |m| {
                let one = m.int(1).unwrap();
                m.unary(UnaryOp::Not, one).unwrap()
            },
            vec![304],
        ),
        (
            "negating text",
            |m| {
                let t = m.text("t").unwrap();
                m.unary(UnaryOp::Neg, t).unwrap()
            },
            vec![304],
        ),
        (
            "comparing text with <",
            |m| {
                let a = m.text("a").unwrap();
                let b = m.text("b").unwrap();
                m.binary(BinaryOp::Lt, a, b).unwrap()
            },
            vec![304],
        ),
        (
            "Int equals Bool",
            |m| {
                let a = m.int(1).unwrap();
                let b = m.bool(true).unwrap();
                m.binary(BinaryOp::Eq, a, b).unwrap()
            },
            vec![304],
        ),
        (
            "and on Int",
            |m| {
                let a = m.int(1).unwrap();
                let b = m.bool(true).unwrap();
                m.binary(BinaryOp::And, a, b).unwrap()
            },
            vec![304],
        ),
        (
            "Int plus Text",
            |m| {
                let a = m.int(1).unwrap();
                let b = m.text("b").unwrap();
                m.binary(BinaryOp::Add, a, b).unwrap()
            },
            vec![300],
        ),
        (
            "print without argument",
            |m| m.call("print", vec![]).unwrap(),
            vec![302],
        ),
        (
            "print of nothing",
            |m| {
                let one = m.int(1).unwrap();
                let inner = m.call("print", vec![one]).unwrap();
                m.call("print", vec![inner]).unwrap()
            },
            vec![304],
        ),
        (
            "function as a value",
            |m| m.name("main").unwrap(),
            vec![100],
        ),
        (
            "calling a parenthesized name",
            |m| {
                let f = m.name("print").unwrap();
                let p = m.paren(f).unwrap();
                m.add_expr(
                    ExprKind::Call {
                        callee: p,
                        args: Box::new([]),
                    },
                    TextRange::default(),
                )
                .unwrap()
            },
            vec![100],
        ),
    ];
    for (what, build, expected) in cases {
        let mut m = Module::new();
        let e = build(&mut m);
        let s = m.expr_stmt(e).unwrap();
        main_with(&mut m, vec![s]);
        assert_eq!(run(&m).1, expected, "{what}");
    }
}

#[test]
fn calling_a_local_is_a_mismatch() {
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let l = m.let_stmt("x", one).unwrap();
    let c = m.call("x", vec![]).unwrap();
    let s = m.expr_stmt(c).unwrap();
    main_with(&mut m, vec![l, s]);
    assert_eq!(run(&m).1, vec![304]);
}

#[test]
fn argument_and_return_types_are_checked() {
    let mut m = Module::new();
    let t = m.text("no").unwrap();
    let r = m.return_stmt(t).unwrap();
    let body = m.block_of(vec![r]).unwrap();
    m.fn_item("f", &[("n", "Int")], Some("Int"), body).unwrap();
    let b = m.bool(false).unwrap();
    let c = m.call("f", vec![b]).unwrap();
    let s = m.expr_stmt(c).unwrap();
    main_with(&mut m, vec![s]);
    assert_eq!(run(&m).1, vec![304, 304]);
}

#[test]
fn returns_are_required_where_a_result_is_declared() {
    // fn f() -> Int with an empty body.
    let mut m = Module::new();
    let body = m.block_of(vec![]).unwrap();
    m.fn_item("f", &[], Some("Int"), body).unwrap();
    main_with(&mut m, vec![]);
    assert_eq!(run(&m).1, vec![305]);

    // if c / return 1, then nothing: the else path falls off.
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let r = m.return_stmt(one).unwrap();
    let then_block = m.block_of(vec![r]).unwrap();
    let c = m.bool(true).unwrap();
    let s = m.if_stmt(c, then_block, None).unwrap();
    let body = m.block_of(vec![s]).unwrap();
    m.fn_item("f", &[], Some("Int"), body).unwrap();
    main_with(&mut m, vec![]);
    assert_eq!(run(&m).1, vec![305]);

    // Both branches return: fine.
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let r1 = m.return_stmt(one).unwrap();
    let two = m.int(2).unwrap();
    let r2 = m.return_stmt(two).unwrap();
    let then_block = m.block_of(vec![r1]).unwrap();
    let else_block = m.block_of(vec![r2]).unwrap();
    let c = m.bool(true).unwrap();
    let s = m.if_stmt(c, then_block, Some(else_block)).unwrap();
    let body = m.block_of(vec![s]).unwrap();
    m.fn_item("f", &[], Some("Int"), body).unwrap();
    main_with(&mut m, vec![]);
    assert_eq!(run(&m).1, Vec::<u16>::new());
}

#[test]
fn main_cannot_return_a_value() {
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let r = m.return_stmt(one).unwrap();
    main_with(&mut m, vec![r]);
    assert_eq!(run(&m).1, vec![304]);
}

#[test]
fn let_of_nothing_reports_once() {
    let mut m = Module::new();
    let one = m.int(1).unwrap();
    let p = m.call("print", vec![one]).unwrap();
    let l = m.let_stmt("u", p).unwrap();
    let u = m.name("u").unwrap();
    let s = print_stmt(&mut m, u);
    main_with(&mut m, vec![l, s]);
    assert_eq!(run(&m).1, vec![304]);
}

#[test]
fn unknown_parameter_type_reports_once() {
    let mut m = Module::new();
    let n = m.name("n").unwrap();
    let one = m.int(1).unwrap();
    let sum = m.binary(BinaryOp::Add, n, one).unwrap();
    let r = m.return_stmt(sum).unwrap();
    let body = m.block_of(vec![r]).unwrap();
    m.fn_item("f", &[("n", "Float")], Some("Integer"), body)
        .unwrap();
    let t = m.text("x").unwrap();
    let c = m.call("f", vec![t]).unwrap();
    let s = m.expr_stmt(c).unwrap();
    main_with(&mut m, vec![s]);
    assert_eq!(run(&m).1, vec![100, 203]);
}

#[test]
fn chain_of_2000_operands_checks_on_a_test_thread() {
    // tests/hostile `chain_below`: the tree height stays below the parser
    // limit, so plain recursion must fit the default 2 MiB test stack.
    let mut m = Module::new();
    let mut acc = m.int(1).unwrap();
    for _ in 1..2000 {
        let one = m.int(1).unwrap();
        acc = m.binary(BinaryOp::Add, acc, one).unwrap();
    }
    let l = m.let_stmt("x", acc).unwrap();
    main_with(&mut m, vec![l]);
    assert_eq!(run(&m).1, Vec::<u16>::new());
}

#[test]
fn operator_and_arrow_positions_fall_back_without_source() {
    let mut m = Module::new();
    let a = m.text("a").unwrap();
    let b = m.text("b").unwrap();
    let plus = m.binary(BinaryOp::Add, a, b).unwrap();
    let s = m.expr_stmt(plus).unwrap();
    let body = m.block_of(vec![s]).unwrap();
    m.fn_item("main", &[], Some("Int"), body).unwrap();
    let (_, diags) = check(&m, FileId::from_raw(0), "");
    let codes: Vec<_> = diags.iter().map(|d| d.code.number()).collect();
    assert_eq!(codes, vec![402, 300]);
    assert!(diags.iter().all(|d| d.span.start == 0));
}
