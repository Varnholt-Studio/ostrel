use ostrel_core::{Diagnostic, FileId};
use ostrel_syntax::ast;

use crate::hir::{self, DefKind, ExprKind, Program, Res, Source, Ty};

const FILE: FileId = FileId::from_raw(1);

fn parse(src: &str) -> ast::Module {
    let (tokens, lexed) = ostrel_syntax::lex::lex(src, FILE);
    let (module, parsed) = ostrel_syntax::parse::parse(src, FILE, &tokens, &lexed);
    assert!(
        lexed.is_empty() && parsed.is_empty(),
        "{lexed:?} {parsed:?}"
    );
    module
}

/// Resolves (which must succeed) and checks.
fn run(src: &str) -> (Program, Vec<Diagnostic>) {
    let module = parse(src);
    let (mut program, resolved) = hir::resolve(&[Source {
        file: FILE,
        text: src,
        module: &module,
    }]);
    assert!(resolved.is_empty(), "{resolved:?}");
    let checked = hir::check(&mut program);
    (program, checked)
}

fn at(src: &str, d: &Diagnostic) -> String {
    let before = &src[..d.span.start as usize];
    let line = before.matches('\n').count() + 1;
    let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    format!("{} {line}:{column}", d.code)
}

/// `E0304 3:5` per diagnostic (ASCII sources).
fn diags(src: &str) -> Vec<String> {
    run(src).1.iter().map(|d| at(src, d)).collect()
}

fn messages(src: &str) -> Vec<String> {
    run(src).1.into_iter().map(|d| d.message).collect()
}

fn clean(src: &str) -> Program {
    let (program, diagnostics) = run(src);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    program
}

/// Resolve diagnostics of a source, for the codes `resolve` reports.
fn resolve_diags(src: &str) -> Vec<String> {
    let module = parse(src);
    let (_, d) = hir::resolve(&[Source {
        file: FILE,
        text: src,
        module: &module,
    }]);
    d.iter().map(|d| at(src, d)).collect()
}

const NOTE: &str = "\
app Notes

data Note
  title: Text
  pinned: Bool = false
  see if true
";

fn with_note(rest: &str) -> String {
    format!("{NOTE}{rest}")
}

#[test]
fn examples_check_clean() {
    for src in [
        include_str!("../../../../examples/v0_2/notes/notes.ostl"),
        include_str!("../../../../examples/v0_2/chat/membership.ostl"),
        include_str!("../../../../examples/v0_2/queries/feed.ostl"),
        include_str!("../../../../examples/v0_2/queries/naming/c01_local_of_other_type.ostl"),
        include_str!("../../../../examples/v0_2/queries/naming/c02_no_where_no_sort.ostl"),
        include_str!("../../../../examples/v0_2/rules_fail_closed/b_rule_runtime_error.ostl"),
    ] {
        clean(src);
    }
}

#[test]
fn every_expression_and_local_gets_a_type() {
    let p = clean(include_str!("../../../../examples/v0_2/notes/notes.ostl"));
    assert!(p.exprs.iter().all(|e| e.ty.is_some()));
    assert!(p.locals.iter().all(|l| l.ty.is_some()));
    // `for n in Note sort title` binds a reference to `Note`.
    let note = p.defs.iter().position(|d| &*d.name == "Note").unwrap() as u32;
    let n = p
        .locals
        .iter()
        .find(|l| &*l.name == "n" && l.ty == Some(Ty::Ref(hir::DefId(note))));
    assert!(n.is_some(), "{:?}", p.locals);
}

#[test]
fn types_of_literals_fields_and_queries() {
    let src = with_note(
        "fn f(n: Note) -> Int\n  let a = n.title.len\n  let b = {1, 2}\n  for c in Note where pinned\n    drop c\n  let d = n.made\n  let e = Url.parse(\"/x\")\n  return a\n",
    );
    let p = clean(&src);
    let note = hir::DefId(1);
    let ty = |name: &str| {
        p.locals
            .iter()
            .find(|l| &*l.name == name)
            .unwrap()
            .ty
            .clone()
    };
    assert_eq!(ty("a"), Some(Ty::Int));
    assert_eq!(ty("b"), Some(Ty::Set(Box::new(Ty::Int))));
    assert_eq!(ty("c"), Some(Ty::Ref(note)));
    assert_eq!(ty("d"), Some(Ty::Time));
    assert_eq!(ty("e"), Some(Ty::Optional(Box::new(Ty::Url))));
}

#[test]
fn var_without_type_takes_the_type_of_its_value() {
    let p = clean("app A\nvar count = 0\nfn f() -> Int\n  return count\n");
    let var = p.defs.iter().find_map(|d| match &d.kind {
        DefKind::Var(v) => Some(v.ty.clone()),
        _ => None,
    });
    assert_eq!(var, Some(Some(Ty::Int)));
}

#[test]
fn mismatch_in_make_field() {
    let src = with_note("fn f()\n  make Note { title: 1 }\n");
    assert_eq!(diags(&src), ["E0304 8:22"]);
    assert_eq!(messages(&src), ["expected `Text`, found `Int`"]);
}

#[test]
fn mismatch_in_field_default() {
    assert_eq!(diags("app A\ndata T\n  n: Int = \"x\"\n"), ["E0304 3:12"]);
}

#[test]
fn mismatch_in_call_argument_and_arity() {
    let src = with_note("fn g(n: Int) -> Int\n  return n\nfn f() -> Int\n  return g(true)\n");
    assert_eq!(diags(&src), ["E0304 10:12"]);
    let src = with_note("fn g(n: Int) -> Int\n  return n\nfn f() -> Int\n  return g(1, 2)\n");
    assert_eq!(diags(&src), ["E0302 10:10"]);
    assert_eq!(messages(&src), ["`g` takes 1 argument, found 2"]);
}

#[test]
fn mismatch_in_return() {
    let src = with_note("fn f(n: Note) -> Int\n  return n.title\n");
    assert_eq!(diags(&src), ["E0304 8:10"]);
    // `return x` in a function without a result: at `return`.
    let src = with_note("fn f()\n  return 1\n");
    assert_eq!(diags(&src), ["E0304 8:3"]);
    assert_eq!(
        messages(&src),
        ["this function returns nothing, found `Int`"]
    );
    // The last expression of a body without branches is the result.
    let src = with_note("fn f(n: Note) -> Bool\n  n.title\n");
    assert_eq!(diags(&src), ["E0304 8:3"]);
}

#[test]
fn mismatch_in_equality() {
    let src = with_note("fn f(n: Note) -> Bool\n  return n.title == 1\n");
    assert_eq!(diags(&src), ["E0304 8:21"]);
    // Optionals compare with `none` and with their inner type.
    clean(&with_note(
        "data P\n  n: Int?\n  see if n == none or n == 1\n",
    ));
}

#[test]
fn mismatch_in_assignment() {
    let src = with_note("fn f(n: Note)\n  n.pinned = \"yes\"\n");
    assert_eq!(diags(&src), ["E0304 8:14"]);
    let src = with_note("view Main\n  for n in Note\n    button \"x\" on click n.title = false\n");
    assert_eq!(diags(&src), ["E0304 9:35"]);
}

#[test]
fn mismatch_in_set_literal_and_in() {
    let src = with_note("fn f() -> Bool\n  return 1 in {2, \"x\"}\n");
    assert_eq!(diags(&src), ["E0304 8:19"]);
    let src = with_note("fn f() -> Bool\n  return \"a\" in {1}\n");
    assert_eq!(diags(&src), ["E0304 8:10"]);
}

// SYNTAX 4.4: `x in c` with `x` none is false, so an optional element fits.
#[test]
fn optional_field_in_set_literal_is_valid() {
    clean("app A\ndata Post\n  tag: Text?\n  see if tag in {\"a\", \"b\"}\n");
}

#[test]
fn optional_field_in_set_parameter_is_valid() {
    clean(
        "app A\ndata Post\n  tag: Text?\n  see if true\nfn f(p: Post, tags: Set[Text]) -> Bool\n  return p.tag in tags\n",
    );
}

#[test]
fn none_in_set_is_valid() {
    clean("app A\nfn f(tags: Set[Text]) -> Bool\n  return none in tags\n");
}

// SYNTAX 4.4: a literal with a `none` element has optional elements, in
// whatever order its elements stand.
#[test]
fn none_in_set_literal_is_valid_in_either_order() {
    clean("app A\nvar tags: Set[Text?] = {\"a\", none}\n");
    clean("app A\nvar tags: Set[Text?] = {none, \"a\"}\n");
    clean("app A\nfn f() -> Set[Text?]\n  return {\"a\", none}\n");
    clean("app A\nfn f() -> Set[Text?]\n  return {none, \"a\"}\n");
}

#[test]
fn none_in_list_literal_is_valid_in_either_order() {
    clean("app A\nvar xs: List[Int?] = [1, none]\n");
    clean("app A\nvar xs: List[Int?] = [none, 1]\n");
    clean("app A\nvar xs: List[Int?] = [1, none, 2]\n");
}

#[test]
fn optional_element_makes_literal_optional_in_either_order() {
    let tail = "fn f(p: Post) -> List[Text?]\n";
    let head = "app A\ndata Post\n  tag: Text?\n  see if true\n";
    clean(&format!("{head}{tail}  return [\"a\", p.tag]\n"));
    clean(&format!("{head}{tail}  return [p.tag, \"a\"]\n"));
}

#[test]
fn literal_with_none_does_not_fit_plain_elements() {
    // With an expected type the finding is at `none`, wherever it stands.
    assert_eq!(
        diags("app A\nvar xs: List[Int] = [1, none]\n"),
        ["E0304 2:25"]
    );
    assert_eq!(
        diags("app A\nvar xs: List[Int] = [none, 1]\n"),
        ["E0304 2:22"]
    );
    assert_eq!(
        messages("app A\nvar tags: Set[Text] = {\"a\", none}\n"),
        ["expected `Text`, found `none`"]
    );
    // Without one, the literal's type does not fit.
    assert_eq!(
        diags("app A\nvar xs = [1, none]\nvar ys: List[Int] = xs\n"),
        ["E0304 3:21"]
    );
}

#[test]
fn literal_elements_still_must_agree() {
    assert_eq!(diags("app A\nvar xs = [1, none, \"a\"]\n"), ["E0304 2:20"]);
    assert_eq!(diags("app A\nvar xs = [none, 1, \"a\"]\n"), ["E0304 2:20"]);
}

#[test]
fn none_is_named_in_messages() {
    assert_eq!(
        messages("app A\nvar x: Int = none\n"),
        ["expected `Int`, found `none`"]
    );
}

#[test]
fn optional_element_must_fit_inside() {
    assert_eq!(
        diags("app A\ndata Post\n  n: Int?\n  see if n in {\"a\"}\n"),
        ["E0304 4:10"]
    );
}

#[test]
fn conditions_must_be_bool() {
    // Rule, check, `if`, `where`, view `if`, style condition.
    assert_eq!(
        diags("app A\ndata T\n  n: Int\n  see if n\n"),
        ["E0301 4:10"]
    );
    assert_eq!(diags("app A\ndata T\n  n: Int\n  check n\n"), ["E0301 4:9"]);
    let src = with_note("fn f()\n  if 1\n    return\n");
    // A bare `return` is a parser error; use a statement instead.
    let src = src.replace("    return\n", "    make Note { title: \"a\" }\n");
    assert_eq!(diags(&src), ["E0301 8:6"]);
    let src = with_note("fn f()\n  for n in Note where title\n    drop n\n");
    assert_eq!(diags(&src), ["E0301 8:23"]);
    let src = with_note("view Main\n  if 1\n    text \"x\"\n");
    assert_eq!(diags(&src), ["E0301 8:6"]);
    let src = with_note("view Main\n  text \"x\" ~big(1)\n");
    assert_eq!(diags(&src), ["E0301 8:17"]);
    assert_eq!(
        messages("app A\ndata T\n  n: Int\n  see if n\n"),
        ["condition must be `Bool`, found `Int`"]
    );
}

#[test]
fn query_count_is_int() {
    let src = with_note("fn f()\n  for n in Note sort title limit \"3\"\n    drop n\n");
    assert_eq!(diags(&src), ["E0304 8:34"]);
}

#[test]
fn boolean_and_arithmetic_operands() {
    let src = with_note("fn f(n: Note) -> Bool\n  return n.pinned and 1\n");
    assert_eq!(diags(&src), ["E0304 8:23"]);
    let src = with_note("fn f() -> Int\n  return 1 * true\n");
    assert_eq!(diags(&src), ["E0304 8:14"]);
    let src = with_note("fn f() -> Bool\n  return not 1\n");
    assert_eq!(diags(&src), ["E0304 8:14"]);
}

#[test]
fn url_typing_follows_spec_12_13() {
    let app = "app Links\n\ndata Site\n  name: Text\n  site: Url\n  see if true\n";
    // `Url?` and `Text` where a `Url` is expected.
    let src = format!("{app}fn f(s: Site)\n  s.site = Url.parse(s.name)\n");
    assert_eq!(diags(&src), ["E0304 8:12"]);
    assert_eq!(messages(&src), ["expected `Url`, found `Url?`"]);
    let src = format!("{app}fn f()\n  make Site {{ name: \"a\", site: \"/x\" }}\n");
    assert_eq!(diags(&src), ["E0304 8:32"]);
    let src = format!(
        "{app}view Main\n  for s in Site\n    link s.name href: Url.parse(s.name)\n    img src: s.name\n"
    );
    assert_eq!(diags(&src), ["E0304 9:23", "E0304 10:14"]);
    // `Url.parse` takes `Text`.
    let src = format!("{app}fn f() -> Url?\n  return Url.parse(1)\n");
    assert_eq!(diags(&src), ["E0304 8:20"]);
    // Allowed: unwrapped with `??` or `if let`, and a `Url` field.
    clean(&format!(
        "{app}view Main\n  for s in Site\n    link s.name href: Url.parse(s.name) ?? s.site\n    img src: s.site\n    if let u = Url.parse(s.name)\n      link s.name href: u\n"
    ));
}

#[test]
fn one_fault_one_diagnostic() {
    // The unknown type of an argument causes nothing further.
    let src = with_note("fn f(n: Note) -> Int\n  let x = n.title.foo\n  return x + 1\n");
    clean(&src);
    // A wrong arity checks no argument types.
    let src =
        with_note("fn g(n: Int) -> Int\n  return n\nfn f() -> Int\n  return g(\"a\", \"b\")\n");
    assert_eq!(diags(&src), ["E0302 10:10"]);
}

#[test]
fn server_fn_called_from_a_view_is_remote() {
    let src = with_note(
        "server fn count() -> Int\n  call if true\n  return 1\nvar shown = 0\nview Main\n  button \"x\" on click shown = count() ?? 0\n",
    );
    clean(&src);
    let src = with_note(
        "server fn count() -> Int\n  call if true\n  return 1\nvar shown = 0\nview Main\n  button \"x\" on click shown = count()\n",
    );
    assert_eq!(diags(&src), ["E0304 12:31"]);
    assert_eq!(messages(&src), ["expected `Int`, found `Remote Int`"]);
}

#[test]
fn me_and_author_are_users_with_auth() {
    let src = "app Chat\n  auth paseto open\ndata Room\n  members: Set[User] = {me}\n  see if me in members and author == me\n";
    clean(src);
    let src = "app Chat\n  auth paseto open\ndata Room\n  name: Text\n  see if author == name\n";
    assert_eq!(diags(src), ["E0304 5:20"]);
}

#[test]
fn serial_field_reads_as_optional() {
    let src =
        "app A\ndata T\n  n: Int serial\n  see if n == none\nfn f(t: T) -> Int\n  return t.n\n";
    assert_eq!(diags(src), ["E0304 6:10"]);
    clean("app A\ndata T\n  n: Int serial\n  see if true\nfn f(t: T) -> Int\n  return t.n ?? 0\n");
}

#[test]
fn serial_per_keys_e0307() {
    let msg = "serial per needs a fixed field of type Bool, Int, Text, Time, Bytes, an enum or a reference, or author";
    for (key, field) in [
        ("id", ""),
        ("made", ""),
        ("team", "  team: Text\n"),
        ("team", "  team: Text? fixed\n"),
        ("team", "  team: Text fixed merge text\n"),
        ("team", "  team: Set[Int] fixed\n"),
    ] {
        let src = format!("app A\ndata T\n{field}  n: Int serial per {key}\n");
        let line = 3 + usize::from(!field.is_empty());
        assert_eq!(resolve_diags(&src), [format!("E0307 {line}:21")], "{src}");
    }
    let module = parse("app A\ndata T\n  n: Int serial per id\n");
    let (_, d) = hir::resolve(&[Source {
        file: FILE,
        text: "app A\ndata T\n  n: Int serial per id\n",
        module: &module,
    }]);
    assert_eq!(d[0].message, msg);
    // Allowed: a fixed reference and `author` with `auth`; an unknown name
    // stays E0204, an unknown key type gives only its E0203.
    clean(
        "app A\ndata Team\n  see if true\ndata T\n  team: Team fixed\n  n: Int serial per team\n",
    );
    clean("app A\n  auth paseto open\ndata T\n  n: Int serial per author\n");
    assert_eq!(
        resolve_diags("app A\ndata T\n  n: Int serial per nope\n"),
        ["E0204 3:21"]
    );
    assert_eq!(
        resolve_diags("app A\ndata T\n  k: Nope fixed\n  n: Int serial per k\n"),
        ["E0203 3:6"]
    );
}

#[test]
fn std_user_rules_check_clean() {
    let p = clean("app Chat\n  auth paseto open\n");
    assert!(p.exprs.iter().all(|e| e.ty.is_some()));
    assert!(
        p.exprs
            .iter()
            .any(|e| matches!(e.kind, ExprKind::Name(Res::Field(..))))
    );
}

/// All orders of `items`.
fn permutations(items: &[&str]) -> Vec<Vec<String>> {
    if items.len() <= 1 {
        return vec![items.iter().map(|s| s.to_string()).collect()];
    }
    let mut all = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut tail in permutations(&rest) {
            tail.insert(0, first.to_string());
            all.push(tail);
        }
    }
    all
}

/// Brackets, annotation, elements and the expected type of `var xs`.
type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<Ty>,
);

/// The type of `var xs` and the codes of the diagnostics.
fn var_outcome(src: &str) -> (Option<Ty>, Vec<String>) {
    let (p, d) = run(src);
    let ty = p.defs.iter().find_map(|d| match &d.kind {
        DefKind::Var(v) => v.ty.clone(),
        _ => None,
    });
    (ty, d.into_iter().map(|d| d.code.to_string()).collect())
}

// SYNTAX 4.4, yannick #2408, ivo #2419: the type of a literal and its
// findings do not depend on the order of its elements, also nested, with
// and without an expected type.
#[test]
fn literal_types_do_not_depend_on_element_order() {
    let int = || Box::new(Ty::Int);
    let opt = |t: Box<Ty>| Box::new(Ty::Optional(t));
    let cases: [Case; 10] = [
        (
            "[",
            "]",
            "",
            &["[1]", "[none]", "[2, none]"],
            Some(Ty::List(Box::new(Ty::List(opt(int()))))),
        ),
        (
            "[",
            "]",
            ": List[List[Int?]]",
            &["[1]", "[none]", "[]"],
            Some(Ty::List(Box::new(Ty::List(opt(int()))))),
        ),
        (
            "{",
            "}",
            "",
            &["\"a\"", "none", "\"b\""],
            Some(Ty::Set(opt(Box::new(Ty::Text)))),
        ),
        (
            "{",
            "}",
            ": Set[Text?]",
            &["\"a\"", "none", "\"b\""],
            Some(Ty::Set(opt(Box::new(Ty::Text)))),
        ),
        (
            "[",
            "]",
            "",
            &["1: [none]", "2: [3]", "4: []"],
            Some(Ty::Map(int(), Box::new(Ty::List(opt(int()))))),
        ),
        (
            "[",
            "]",
            ": Map[Int, Int?]",
            &["1: none", "2: 3"],
            Some(Ty::Map(int(), opt(int()))),
        ),
        (
            "[",
            "]",
            "",
            &["{none}", "{1}", "none"],
            Some(Ty::List(opt(Box::new(Ty::Set(opt(int())))))),
        ),
        // Elements without a join: one finding, no element type, in every order.
        (
            "[",
            "]",
            "",
            &["[1]", "[\"a\"]", "[none]"],
            Some(Ty::List(Box::new(Ty::Error))),
        ),
        (
            "[",
            "]",
            "",
            &["1", "\"a\"", "none"],
            Some(Ty::List(Box::new(Ty::Error))),
        ),
        // With an expected type each element that does not fit is a finding.
        (
            "[",
            "]",
            ": List[List[Int]]",
            &["[1]", "[\"a\"]", "[none]"],
            Some(Ty::List(Box::new(Ty::List(int())))),
        ),
    ];
    for (open, close, annotation, items, want) in cases {
        let mut seen: Option<(Option<Ty>, Vec<String>)> = None;
        for order in permutations(items) {
            let src = format!(
                "app A\nvar xs{annotation} = {open}{}{close}\n",
                order.join(", ")
            );
            let got = var_outcome(&src);
            assert_eq!(got.0, want, "{src}");
            match &seen {
                Some(first) => assert_eq!(&got, first, "{src}"),
                None => seen = Some(got),
            }
        }
    }
    // The number of findings of the failing cases.
    assert_eq!(
        var_outcome("app A\nvar xs = [[\"a\"], [none], [1]]\n").1,
        ["E0304"]
    );
    assert_eq!(
        var_outcome("app A\nvar xs: List[List[Int]] = [[none], [\"a\"], [1]]\n").1,
        ["E0304", "E0304"]
    );
}

#[test]
fn nested_literal_from_review_is_valid_in_either_order() {
    clean("app A\nvar xs: List[List[Int?]] = [[1], [none]]\n");
    clean("app A\nvar xs: List[List[Int?]] = [[none], [1]]\n");
    clean("app A\nvar xs = [[1], [none]]\nvar ys: List[List[Int?]] = xs\n");
    clean("app A\nvar xs = [[none], [1]]\nvar ys: List[List[Int?]] = xs\n");
    // Parameters and fields pass their type into the literal as well.
    clean(
        "app A\nfn g(xs: List[List[Int?]]) -> Int\n  return 1\nfn f() -> Int\n  return g([[1], [none]])\n",
    );
    clean(
        "app A\ndata T\n  m: Map[Text, List[Int?]] = [\"a\": [1], \"b\": [none]]\n  see if true\n",
    );
}

#[test]
fn mixed_literal_reports_the_element_against_the_expected_type() {
    // ivo #2417: [1, 'a'] and ['a', 1] at List[Text] give the same finding.
    assert_eq!(
        diags("app A\nvar xs: List[Text] = [1, \"a\"]\n"),
        ["E0304 2:23"]
    );
    assert_eq!(
        diags("app A\nvar xs: List[Text] = [\"a\", 1]\n"),
        ["E0304 2:28"]
    );
    assert_eq!(
        messages("app A\nvar xs: List[Text] = [\"a\", 1]\n"),
        ["expected `Text`, found `Int`"]
    );
}

// T204: the rest of the v0.2 type check (SPEC 12.6, 12.10, 12.11, D124, D142).

#[test]
fn ac_29_missing_return_is_e0305_at_the_name() {
    let src = with_note("fn sign(n: Int) -> Int\n  if n > 0\n    return 1\n");
    assert_eq!(resolve_diags(&src), ["E0305 7:4"]);
    let src = with_note("server fn sign(n: Int) -> Int\n  for x in Note\n    return 1\n");
    assert_eq!(resolve_diags(&src), ["E0305 7:11"]);
    let src = with_note("fn sign(n: Int) -> Int\n  let m = n\n");
    assert_eq!(resolve_diags(&src), ["E0305 7:4"]);
}

#[test]
fn ac_29_trailing_value_in_a_body_with_branches_is_e0303() {
    let src = with_note("fn sign(n: Int) -> Int\n  if n > 0\n    return 1\n  0\n");
    assert_eq!(resolve_diags(&src), ["E0303 10:3"]);
    let src = with_note("fn sign(n: Int) -> Int\n  if n > 0\n    1\n  else\n    return 0\n");
    assert_eq!(resolve_diags(&src), ["E0303 9:5"]);
}

#[test]
fn ac_29_bodies_that_always_return_are_clean() {
    clean(&with_note(
        "fn sign(n: Int) -> Int\n  if n > 0\n    return 1\n  else\n    return 0\n",
    ));
    clean(&with_note("fn one() -> Int\n  1\n"));
    clean(&with_note(
        "fn act()\n  if true\n    make Note { title: \"a\" }\n",
    ));
    clean(&with_note(
        "fn f(t: Int?) -> Int\n  if let x = t\n    return x\n  else\n    return 0\n",
    ));
}

#[test]
fn ac_29_constant_empty_range_is_e0311_at_the_lower_bound() {
    assert_eq!(
        diags("app A\ndata T\n  n: Int\n  check n in 5..1\n"),
        ["E0311 4:14"]
    );
    assert_eq!(
        diags("app A\ndata T\n  n: Int\n  check n in -1..-5\n"),
        ["E0311 4:14"]
    );
    let src = with_note("fn f()\n  for n in Note where title.len in 3..2\n    drop n\n");
    assert_eq!(diags(&src), ["E0311 8:36"]);
    assert_eq!(
        messages("app A\ndata T\n  n: Int\n  check n in 5..1\n"),
        ["the range 5..1 holds no value: its lower bound is above its upper bound"]
    );
    // Equal bounds, bounds given at run time.
    clean("app A\ndata T\n  n: Int\n  check n in 5..5\n  check n in -5..-1\n");
    clean(&with_note(
        "fn f(a: Int)\n  for n in Note where title.len in 5..a\n    drop n\n",
    ));
}

#[test]
fn ac_29_constant_negative_count_is_e0312() {
    let src = with_note("fn f()\n  for n in Note sort title limit -1\n    drop n\n");
    assert_eq!(diags(&src), ["E0312 8:34"]);
    let src = with_note("fn f()\n  for n in Note sort title last -3\n    drop n\n");
    assert_eq!(diags(&src), ["E0312 8:33"]);
    assert_eq!(
        messages(&src),
        ["the count of `last` is -3; a query count is 0 or more"]
    );
    clean(&with_note(
        "fn f(k: Int)\n  for n in Note sort title last 0\n    drop n\n  for n in Note limit k\n    drop n\n",
    ));
}

const ELEVATED: &str = "\
app Teams
  auth paseto open

data Team
  name: Text
  owner: User
  admins: Set[User]
  see if true
";

#[test]
fn ac_29_call_rule_of_an_elevated_function_must_bind_me() {
    // D124 goldens ac_54_call_rule_me_eq_me and ac_54_call_rule_me_handle_const.
    let src =
        format!("{ELEVATED}server fn wipe(t: Team) as system\n  call if me == me\n  drop t\n");
    assert_eq!(diags(&src), ["E0306 10:11"]);
    let src = format!(
        "{ELEVATED}server fn wipe(t: Team) as system\n  call if signed and me.handle == \"root\"\n  drop t\n"
    );
    assert_eq!(diags(&src), ["E0306 10:11"]);
    assert_eq!(
        messages(&src),
        [
            "the call rule of an as system function must compare me with a parameter or with row data"
        ]
    );
    // An atom with `me` and a parameter, a hop or `in` binds it.
    for rule in [
        "me == t.owner",
        "signed and me in t.admins",
        "not (t.owner != me) or me.handle == \"x\"",
    ] {
        let src =
            format!("{ELEVATED}server fn wipe(t: Team) as system\n  call if {rule}\n  drop t\n");
        clean(&src);
    }
    // Only elevated functions need it.
    clean(&format!(
        "{ELEVATED}server fn look(t: Team) -> Text\n  call if me == me\n  t.name\n"
    ));
}

const WORDS: &str = "\
app Board

enum Phase = open | done
enum Door = open | shut

data Card
  phase: Phase
  door: Door
  see if phase == open
";

#[test]
fn ac_29_words_take_the_expected_type() {
    let (program, diagnostics) = run(&format!(
        "{WORDS}fn f(c: Card) -> Door\n  c.phase = open\n  return open\n"
    ));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let words: Vec<_> = program
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            ExprKind::Name(Res::Variant(d, v)) => {
                Some((program.def(d).map(|d| &*d.name), v, e.ty.clone()))
            }
            ExprKind::Word { .. } => panic!("a word is left after check"),
            _ => None,
        })
        .collect();
    let phase = program
        .defs
        .iter()
        .position(|d| &*d.name == "Phase")
        .unwrap();
    let door = program
        .defs
        .iter()
        .position(|d| &*d.name == "Door")
        .unwrap();
    let (phase, door) = (
        Ty::Enum(crate::hir::DefId(phase as u32)),
        Ty::Enum(crate::hir::DefId(door as u32)),
    );
    assert_eq!(
        words,
        [
            (Some("Phase"), 0, Some(phase.clone())),
            (Some("Phase"), 0, Some(phase)),
            (Some("Door"), 0, Some(door)),
        ]
    );
}

#[test]
fn ac_29_words_without_a_deciding_type_are_e0200() {
    let src = format!("{WORDS}fn f()\n  let x = open\n");
    assert_eq!(diags(&src), ["E0200 11:11"]);
    assert_eq!(
        messages(&src),
        ["`open` is a variant of `Phase` and `Door`; the expected type does not say which"]
    );
    let src = format!("{WORDS}fn f() -> Int\n  return open\n");
    assert_eq!(diags(&src), ["E0200 11:10"]);
    assert_eq!(
        messages(&src),
        ["`open` is no value of `Int`; it is a variant of `Phase`, `Door`"]
    );
    // `in` takes the element type of the collection.
    clean(&format!(
        "{WORDS}fn f(c: Card) -> Bool\n  return open in {{c.door}}\n"
    ));
}

#[test]
fn ac_29_in_a_map_checks_the_key() {
    let src = with_note("fn f(m: Map[Text, Int]) -> Bool\n  return 1 in m\n");
    assert_eq!(diags(&src), ["E0304 8:10"]);
    clean(&with_note(
        "fn f(m: Map[Text, Int]) -> Bool\n  return \"a\" in m\n",
    ));
}

#[test]
fn ac_29_fallback_literal_is_checked_per_element() {
    // ivo #2443: the finding stands at the element, not at the literal.
    let src =
        with_note("fn f(m: Map[Text, List[Int]]) -> List[Int]\n  return m[\"a\"] ?? [1, \"b\"]\n");
    assert_eq!(diags(&src), ["E0304 8:24"]);
}

#[test]
fn ac_29_each_none_against_a_plain_list_is_a_finding() {
    // yannick #2439 F1: two faults, two findings.
    let src = with_note("fn f()\n  let xs: List[Int] = [none, none]\n");
    let src = src.replace("let xs: List[Int] = [none, none]", "g([none, none])");
    let src = format!("{src}fn g(xs: List[Int])\n  make Note {{ title: \"a\" }}\n");
    assert_eq!(diags(&src), ["E0304 8:6", "E0304 8:12"]);
}

#[test]
fn ac_29_inner_literals_get_the_joined_type() {
    // yannick #2439 F4.
    let (program, d) = run(&with_note("var xs = [[1], [none]]\n"));
    assert!(d.is_empty(), "{d:?}");
    let want = Ty::List(Box::new(Ty::Optional(Box::new(Ty::Int))));
    let inner: Vec<_> = program
        .exprs
        .iter()
        .filter(|e| matches!(e.kind, ExprKind::List(ref items) if items.len() == 1))
        .map(|e| e.ty.clone())
        .collect();
    assert_eq!(inner, [Some(want.clone()), Some(want)]);
}

#[test]
fn ac_29_ranges_and_remote_values_join() {
    // ivo #2443: `join` of ranges.
    let (program, d) = run(&with_note("var rs = [1..2, 3..4]\n"));
    assert!(d.is_empty(), "{d:?}");
    let var = program.defs.iter().find_map(|d| match &d.kind {
        DefKind::Var(v) => v.ty.clone(),
        _ => None,
    });
    assert_eq!(var, Some(Ty::List(Box::new(Ty::Range(Box::new(Ty::Int))))));
}
