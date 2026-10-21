//! Tests of the v0.2 declarations (T119), with the real lexer and the real
//! expression parser.
//!
//! The goldens in `goldens/` pair a source with the text form of its syntax
//! tree written by [`dump`](super::dump). Each golden of the `fn_*` group
//! shows one criterion of the choice between `Item::Fn` and `Item::PlacedFn`
//! (D95): a plain function stays `Fn`; placement `server` or `client`,
//! `as system`, a `call` rule, an optional type and a type with arguments
//! each make it `PlacedFn` on their own.

use super::dump;
use crate::ast::{
    ElementChild, ExprKind, Item, Limits, MAX_HEIGHT, MAX_NODES, MemberKind, Module, Placement,
    StmtKind, ViewNodeKind,
};
use crate::lex::lex;
use crate::parse::expr::Exprs;
use crate::parse::{MAX_DEPTH, ParseLimits, Parser, parse_with};
use ostrel_core::{Diagnostic, FileId};

const F: FileId = FileId::from_raw(0);

type R = Result<(), String>;

trait Must<T> {
    fn must(self) -> Result<T, String>;
}

impl<T> Must<T> for Option<T> {
    fn must(self) -> Result<T, String> {
        self.ok_or_else(|| "missing value".to_string())
    }
}

/// Lexes and parses `src`; returns the module, the lexer and the parser
/// diagnostics.
fn run(src: &str, limits: ParseLimits) -> (Module, Vec<Diagnostic>, Vec<Diagnostic>) {
    let (tokens, lexed) = lex(src, F);
    let (module, diags) = parse_with(src, F, &tokens, &lexed, limits, &Exprs);
    (module, lexed, diags)
}

/// Parses `src` that must lex and parse cleanly; returns the text form.
fn tree(src: &str) -> String {
    let (m, lexed, diags) = run(src, ParseLimits::DEFAULT);
    assert!(lexed.is_empty(), "lexer diagnostics: {lexed:?}");
    assert!(diags.is_empty(), "parser diagnostics: {diags:?}");
    dump::module(&m)
}

/// Code, start offset and message of every parser diagnostic of `src`,
/// which must lex cleanly.
fn diags(src: &str) -> Vec<(u16, u32, String)> {
    let (_, lexed, diags) = run(src, ParseLimits::DEFAULT);
    assert!(lexed.is_empty(), "lexer diagnostics: {lexed:?}");
    diags
        .into_iter()
        .map(|d| (d.code.number(), d.span.start, d.message))
        .collect()
}

/// Code and start offset of every parser diagnostic of `src`.
fn codes_at(src: &str) -> Vec<(u16, u32)> {
    diags(src).into_iter().map(|(c, at, _)| (c, at)).collect()
}

/// Byte offset of the `n`th occurrence (from 0) of `needle` in `src`.
fn at(src: &str, needle: &str, n: usize) -> u32 {
    let offset = src
        .match_indices(needle)
        .nth(n)
        .map_or(usize::MAX, |(i, _)| i);
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// The source text of `range`.
fn text(src: &str, range: crate::ast::TextRange) -> &str {
    src.get(range.start() as usize..range.end() as usize)
        .unwrap_or("<bad range>")
}

// ----- goldens -----

macro_rules! goldens {
    ($($name:literal => $src:expr),* $(,)?) => {
        &[$(($name, $src, include_str!(concat!("goldens/", $name, ".ast"))),)*]
    };
}

macro_rules! golden_source {
    ($name:literal) => {
        include_str!(concat!("goldens/", $name, ".ostl"))
    };
}

/// Every golden parses without a parser diagnostic and gives exactly the
/// tree of its `.ast` file.
#[test]
fn goldens_match_their_trees() {
    let cases: &[(&str, &str, &str)] = goldens!(
        "fn_plain_stays_fn" => golden_source!("fn_plain_stays_fn"),
        "fn_server_is_placed" => golden_source!("fn_server_is_placed"),
        "fn_client_is_placed" => golden_source!("fn_client_is_placed"),
        "fn_as_system_is_placed" => golden_source!("fn_as_system_is_placed"),
        "fn_call_rule_is_placed" => golden_source!("fn_call_rule_is_placed"),
        "fn_optional_param_is_placed" => golden_source!("fn_optional_param_is_placed"),
        "fn_type_args_result_is_placed" => golden_source!("fn_type_args_result_is_placed"),
        "fn_optional_result_is_placed" => golden_source!("fn_optional_result_is_placed"),
        "fn_type_args_param_is_placed" => golden_source!("fn_type_args_param_is_placed"),
        "tracker_excerpt" => golden_source!("tracker_excerpt"),
        "examples_notes" => include_str!("../../../../../examples/v0_2/notes/notes.ostl"),
        "examples_chat" => include_str!("../../../../../examples/chat/chat.ostl"),
        "examples_chat_nostd" => include_str!("../../../../../examples/chat-nostd/chat.ostl"),
    );
    for (name, src, expected) in cases {
        let (m, _, diags) = run(src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{name}: {diags:?}");
        assert_eq!(dump::module(&m), *expected, "{name}");
    }
}

/// The `fn_*` goldens hold what their names say, checked on the tree
/// itself and not only on its text form.
#[test]
fn each_criterion_alone_makes_a_placed_function() -> R {
    let cases = [
        (golden_source!("fn_plain_stays_fn"), None),
        (
            golden_source!("fn_server_is_placed"),
            Some(Placement::Server),
        ),
        (
            golden_source!("fn_client_is_placed"),
            Some(Placement::Client),
        ),
        (
            golden_source!("fn_as_system_is_placed"),
            Some(Placement::Inferred),
        ),
        (
            golden_source!("fn_call_rule_is_placed"),
            Some(Placement::Inferred),
        ),
        (
            golden_source!("fn_optional_param_is_placed"),
            Some(Placement::Inferred),
        ),
        (
            golden_source!("fn_type_args_result_is_placed"),
            Some(Placement::Inferred),
        ),
        (
            golden_source!("fn_optional_result_is_placed"),
            Some(Placement::Inferred),
        ),
        (
            golden_source!("fn_type_args_param_is_placed"),
            Some(Placement::Inferred),
        ),
    ];
    for (src, placed) in cases {
        let (m, _, diags) = run(src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        assert_eq!(m.items().len(), 1, "{src}");
        match (m.items().first().must()?, placed) {
            (Item::Fn(decl), None) => {
                assert_eq!(text(src, decl.range), src.trim_end(), "{src}");
                assert_eq!(decl.params.len(), 2);
            }
            (Item::PlacedFn(decl), Some(placement)) => {
                assert_eq!(decl.placement, placement, "{src}");
                assert_eq!(text(src, decl.range), src.trim_end(), "{src}");
                assert_eq!(decl.params.len(), 2);
            }
            (item, _) => return Err(format!("{src}: unexpected {item:?}")),
        }
    }
    Ok(())
}

/// `examples/v0_2/notes` (AC-09) and the chat parse without a parser
/// diagnostic. The chat's `style` body still has lexer faults until the
/// lexer gets the CSS mode of SYNTAX 3; all of them lie inside that body.
#[test]
fn the_examples_parse() -> R {
    let notes = include_str!("../../../../../examples/v0_2/notes/notes.ostl");
    let (_, lexed, diags) = run(notes, ParseLimits::DEFAULT);
    assert!(lexed.is_empty() && diags.is_empty(), "{lexed:?} {diags:?}");
    for src in [
        include_str!("../../../../../examples/chat/chat.ostl"),
        include_str!("../../../../../examples/chat-nostd/chat.ostl"),
    ] {
        let (m, lexed, diags) = run(src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{diags:?}");
        let Some(Item::Style(style)) = m.items().last() else {
            return Err("the chat ends with `style`".into());
        };
        let body = text(src, style.body);
        assert!(body.starts_with('.'), "{body}");
        assert!(body.ends_with('}'), "{body}");
        assert_eq!(style.body.end() as usize, src.trim_end().len());
        for diag in &lexed {
            assert!(
                style.body.start() <= diag.span.start && diag.span.start < style.body.end(),
                "lexer fault outside the style body: {diag:?}"
            );
        }
    }
    Ok(())
}

/// Every other example under `examples/v0_2` parses cleanly too.
#[test]
fn the_v0_2_examples_parse_cleanly() {
    let cases: &[(&str, &str)] = &[
        (
            "chat/membership",
            include_str!("../../../../../examples/v0_2/chat/membership.ostl"),
        ),
        (
            "queries/feed",
            include_str!("../../../../../examples/v0_2/queries/feed.ostl"),
        ),
        (
            "naming/c01",
            include_str!(
                "../../../../../examples/v0_2/queries/naming/c01_local_of_other_type.ostl"
            ),
        ),
        (
            "naming/c02",
            include_str!("../../../../../examples/v0_2/queries/naming/c02_no_where_no_sort.ostl"),
        ),
        (
            "naming/s01",
            include_str!("../../../../../examples/v0_2/queries/naming/s01_param_in_where.ostl"),
        ),
        (
            "naming/s02",
            include_str!("../../../../../examples/v0_2/queries/naming/s02_let_in_where.ostl"),
        ),
        (
            "naming/s03",
            include_str!("../../../../../examples/v0_2/queries/naming/s03_loop_var_in_where.ostl"),
        ),
        (
            "naming/s04",
            include_str!("../../../../../examples/v0_2/queries/naming/s04_loop_var_in_sort.ostl"),
        ),
        (
            "naming/s05",
            include_str!("../../../../../examples/v0_2/queries/naming/s05_implicit_field.ostl"),
        ),
        (
            "naming/s06",
            include_str!("../../../../../examples/v0_2/queries/naming/s06_unused_local.ostl"),
        ),
        (
            "rules/a",
            include_str!("../../../../../examples/v0_2/rules_fail_closed/a_optional_ordering.ostl"),
        ),
        (
            "rules/b",
            include_str!(
                "../../../../../examples/v0_2/rules_fail_closed/b_rule_runtime_error.ostl"
            ),
        ),
        (
            "rules/c",
            include_str!("../../../../../examples/v0_2/rules_fail_closed/c_set_directions.ostl"),
        ),
        (
            "rules/d",
            include_str!(
                "../../../../../examples/v0_2/rules_fail_closed/d_call_denied_fallback.ostl"
            ),
        ),
        (
            "rules/e",
            include_str!("../../../../../examples/v0_2/rules_fail_closed/e_limit_after_see.ostl"),
        ),
    ];
    for (name, src) in cases {
        let (m, lexed, diags) = run(src, ParseLimits::DEFAULT);
        assert!(
            lexed.is_empty() && diags.is_empty(),
            "{name}: {lexed:?} {diags:?}"
        );
        assert!(!m.items().is_empty(), "{name}");
    }
}

// ----- app, enum, var -----

#[test]
fn app_enum_and_var() {
    assert_eq!(tree("app Shop\n"), "app Shop\n");
    assert_eq!(
        tree("app Shop\n  auth paseto invite\n  auth paseto\n  home Start\n"),
        "app Shop\n  (auth paseto invite)\n  (auth paseto)\n  (home Start)\n"
    );
    assert_eq!(tree("enum One = only\n"), "enum One = only\n");
    assert_eq!(
        tree("enum Role = guest | member | admin\n"),
        "enum Role = guest | member | admin\n"
    );
    assert_eq!(tree("var current: Room?\n"), "var current: Room?\n");
    assert_eq!(
        tree("var picked: Set[Room] = {}\n"),
        "var picked: Set[Room] = (set)\n"
    );
    assert_eq!(tree("var draft = \"\"\n"), "var draft = (str )\n");
}

#[test]
fn faults_in_app_enum_and_var() {
    let src = "app\n  auth paseto\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 0),
            "expected a name after `app`, found end of line".into()
        )]
    );
    // One faulty line of the block; the others are kept.
    let src = "app Shop\n  auth\n  store sqlite\n  home Start\n";
    assert_eq!(
        diags(src),
        [
            (
                28,
                at(src, "\n", 1),
                "expected an auth scheme such as `paseto` after `auth`, found end of line".into()
            ),
            (
                28,
                at(src, "store", 0),
                "expected `auth` or `home` in an `app` declaration, found `store`".into()
            ),
        ]
    );
    let (m, _, _) = run(src, ParseLimits::DEFAULT);
    assert_eq!(dump::module(&m), "app Shop\n  (home Start)\n");

    let src = "enum Role guest\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "guest", 0),
            "expected `=` and the variants after `enum Role`, found `guest`".into()
        )]
    );
    let src = "enum Role = a |\n";
    assert_eq!(
        codes_at(src),
        [(28, at(src, "\n", 0))],
        "a trailing `|` wants a variant"
    );
    let src = "enum Role = a, b\n";
    assert_eq!(codes_at(src), [(28, at(src, ",", 0))]);
    let src = "var x\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 0),
            "expected `:` and a type, or `=` and a value, after `var x`, found end of line".into()
        )]
    );
    assert_eq!(codes_at("var x += 1\n"), [(20, 6)]);
}

// ----- data -----

#[test]
fn data_fields_modes_rules_and_checks() {
    let src = "data Issue\n  team: Team fixed\n  owner: User server\n  number: Int? serial\n  \
               seq: Int? serial per team\n  body: Text = \"\" merge text\n  tags: Map[Text, Set[User]]\n  \
               check body.len < 10\n  see if true\n  make if signed\n  edit if me == owner\n  \
               edit tags if added == {me}\n  drop if false\n";
    assert_eq!(
        tree(src),
        "data Issue\n  (field team Team fixed)\n  (field owner User server)\n  \
         (field number Int? serial)\n  (field seq Int? (serial per team))\n  \
         (field body Text (= (str )) (merge text))\n  (field tags Map[Text, Set[User]])\n  \
         (check (< (. body len) 10))\n  (see true)\n  (make signed)\n  (edit (== me owner))\n  \
         (edit tags (== added (set me)))\n  (drop false)\n"
    );
}

#[test]
fn data_ranges_cover_each_line() -> R {
    let src = "data Room\n  name: Text = \"x\" fixed\n  // note\n  see if signed\n";
    let (m, _, diags) = run(src, ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let Some(Item::Data(data)) = m.items().first() else {
        return Err("no data".into());
    };
    assert_eq!(text(src, data.range), src.trim_end());
    let lines: Vec<&str> = data.members.iter().map(|mb| text(src, mb.range)).collect();
    assert_eq!(lines, ["name: Text = \"x\" fixed", "see if signed"]);
    let MemberKind::Field(field) = &data.members.first().must()?.kind else {
        return Err("no field".into());
    };
    assert_eq!(text(src, m.ty(field.ty).must()?.range), "Text");
    Ok(())
}

#[test]
fn faults_in_data() {
    let src = "data Room\nfn main()\n  print(1)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "fn", 0),
            "expected an indented block with the fields and rules of `Room`, found `fn`".into()
        )]
    );
    // A faulty head line: the members are still read, each fault once.
    let src = "data Room x\n  name Text\n  see signed\n  edit members signed\n  data: Int\n  \
               fn f()\n  check\n  text: Text\n";
    assert_eq!(
        diags(src),
        [
            (
                28,
                at(src, "x", 0),
                "expected end of line, found `x`".into()
            ),
            (
                28,
                at(src, "Text", 0),
                "expected `:` and a type after field `name`, found `Text`".into()
            ),
            (
                28,
                at(src, "signed", 0),
                "expected `if` and a condition after `see`, found `signed`".into()
            ),
            (
                28,
                at(src, "signed", 1),
                "expected `if` and a condition after `edit members`, found `signed`".into()
            ),
            (
                26,
                at(src, "data", 1),
                "`data` is a keyword and cannot be used as a name".into()
            ),
            (
                28,
                at(src, "fn", 0),
                "expected a field `name: Type`, a rule such as `see if ...` or `check`, found `fn`"
                    .into()
            ),
            (
                28,
                at(src, "\n", 6),
                "expected an expression, found end of line".into()
            ),
        ]
    );
    // A field line with a fault keeps the declaration and its good lines.
    let src = "data Room\n  name: Text fixed fixed\n  size: Int\n";
    assert_eq!(codes_at(src), [(28, at(src, "fixed", 1))]);
    let (m, _, _) = run(src, ParseLimits::DEFAULT);
    assert_eq!(dump::module(&m), "data Room\n  (field size Int)\n");
    // Types: arguments are bracketed and not empty.
    for (src, needle, n) in [
        ("data A\n  f: Set[]\n", "]", 0),
        ("data A\n  f: Set[Int\n", "\n", 1),
        ("data A\n  f: Map[Int Int]\n", "Int]", 0),
        ("data A\n  f: 1\n", "1", 0),
    ] {
        assert_eq!(codes_at(src), [(28, at(src, needle, n))], "{src:?}");
    }
}

// ----- functions -----

#[test]
fn call_rule_only_on_the_first_body_line() {
    let src = "server fn f(t: Team)\n  print(1)\n  call if t.open\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "call", 0),
            "expected a statement (a `call` rule stands only on the first line of a function \
             body), found `call`"
                .into()
        )]
    );
    let src = "fn f()\n  call t.open\n  print(1)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "t", 0),
            "expected `if` and a condition after `call`, found `t`".into()
        )]
    );
    // A faulty call rule is skipped; the function is built without it.
    let (m, _, _) = run(src, ParseLimits::DEFAULT);
    assert_eq!(dump::module(&m), "fn f()\n  (call print 1)\n");
    // Blank and comment lines before the rule do not count as lines.
    assert_eq!(
        tree("fn f()\n\n  // why\n  call if signed\n"),
        "placed-fn [inferred] f()\n  (call-rule signed)\n"
    );
}

/// A body that holds only the `call` rule has an empty block; the range of
/// the function still ends at the end of the rule, also with blank and
/// comment lines before it.
#[test]
fn a_call_rule_alone_ends_the_function_range() -> R {
    for src in [
        "fn f()\n  call if signed\n",
        "fn f()\n  // why\n  call if signed\n",
        "fn f()\n\n  call if signed\n",
        "fn f()\n  call if signed\nfn g()\n  print(1)\n",
    ] {
        let (m, _, diags) = run(src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let Some(Item::PlacedFn(decl)) = m.items().first() else {
            return Err(format!("{src}: no placed function"));
        };
        let rule = decl.call_rule.as_ref().must()?;
        assert_eq!(text(src, rule.range), "call if signed", "{src}");
        let end = at(src, "signed", 0) + 6;
        assert_eq!(decl.range.end(), end, "{src}");
        assert_eq!(decl.range.start(), 0, "{src}");
    }
    // With statements after the rule the block still decides the end.
    let src = "fn f()\n  call if signed\n  print(1)\n";
    let (m, _, _) = run(src, ParseLimits::DEFAULT);
    let Some(Item::PlacedFn(decl)) = m.items().first() else {
        return Err("no placed function".into());
    };
    assert_eq!(text(src, decl.range), src.trim_end());
    Ok(())
}

#[test]
fn faults_in_function_heads() {
    let src = "fn f() as\n  call if signed\n  print(1)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 0),
            "expected `system` after `as`, found end of line".into()
        )]
    );
    // The body of a faulty head is parsed with its `call` rule: no
    // diagnostic for the rule, one for a fault below it.
    let src = "server fn (t: Team)\n  call if signed\n  let = 1\n";
    assert_eq!(
        codes_at(src),
        [(28, at(src, "(", 0)), (28, at(src, "=", 0))]
    );
    let src = "fn f(a: Set[Int)\n  print(1)\n";
    assert_eq!(codes_at(src), [(28, at(src, ")", 0))]);
    let src = "client fn f()\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 0) + 1,
            "expected an indented block as the body of `f`, found end of file".into()
        )]
    );
    // `client` is a name everywhere except before `fn`.
    assert_eq!(tree("fn f()\n  client = 1\n"), "fn f()\n  (= client 1)\n");
}

// ----- statements -----

#[test]
fn v0_2_statements() {
    let src = "fn f(r: Room)\n  if let x = r.owner\n    x.name = \"a\"\n  else\n    drop r\n  \
               for m in Message where room == r sort made desc last n + 1\n    drop m\n  \
               for x in r.members\n    print(x)\n  drop Issue where team == r\n";
    assert_eq!(
        tree(src),
        "fn f(r: Room)\n  if let x (. r owner)\n    (= (. x name) (str \"a\"))\n  else\n    \
         (drop (query r))\n  for m (query Message (where (== room r)) (sort made desc) \
         (last (+ n 1)))\n    (drop (query m))\n  for x (. r members)\n    (call print x)\n  \
         (drop (query Issue (where (== team r))))\n"
    );
}

#[test]
fn query_heads_and_clauses() {
    let src = "fn f()\n  for s in Status\n    print(s)\n  for a in A limit 0\n    print(a)\n  \
               for b in B sort x\n    print(b)\n  for c in c(1)\n    print(c)\n";
    assert_eq!(
        tree(src),
        "fn f()\n  for s (query Status)\n    (call print s)\n  for a (query A (limit 0))\n    \
         (call print a)\n  for b (query B (sort x))\n    (call print b)\n  \
         for c (call c 1)\n    (call print c)\n"
    );
    let order = "expected the end of the query (its clauses come in the order `where`, `sort`, \
                 then `limit` or `last`, each at most once), found ";
    for (line, needle, found) in [
        ("for m in M sort a where b", "where", "`where`"),
        ("for m in M limit 1 sort a", "sort", "`sort`"),
        ("for m in M limit 1 last 2", "last", "`last`"),
        ("for m in M where a where b", "where", "`where`"),
    ] {
        let src = format!("fn f()\n  {line}\n    print(m)\n");
        let n = usize::from(needle == "where" && line.matches("where").count() == 2);
        assert_eq!(
            diags(&src),
            [(28, at(&src, needle, n), format!("{order}{found}"))],
            "{line}"
        );
    }
    let src = "fn f()\n  drop r.items where x\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "where", 0),
            "expected the end of the line (query clauses follow only a bare type name), \
             found `where`"
                .into()
        )]
    );
    // D133 (2): a clause after any expression other than a bare name.
    let src = "fn f()\n  for x in r.members where x\n    print(x)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "where", 0),
            "expected the end of the line (query clauses follow only a bare type name), \
             found `where`"
                .into()
        )]
    );
    let src = "fn f()\n  for m in M sort\n    print(m)\n";
    assert_eq!(codes_at(src), [(28, at(src, "\n", 1))]);
    let src = "fn f()\n  for m in M sort desc\n    print(m)\n";
    assert_eq!(codes_at(src), [(26, at(src, "desc", 0))]);
}

#[test]
fn faults_in_v0_2_statements() {
    let src = "fn f()\n  for x of xs\n    print(x)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "of", 0),
            "expected `in` after the loop variable (Ostrel writes `for x in xs`), found `of`"
                .into()
        )]
    );
    let src = "fn f()\n  drop\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 1),
            "expected a record or a query after `drop`, found end of line".into()
        )]
    );
    let src = "fn f()\n  if let = x\n    print(1)\n";
    assert_eq!(codes_at(src), [(28, at(src, "=", 0))]);
    let src = "fn f()\n  if let y x\n    print(1)\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "x", 0),
            "expected `=` after `if let y`, found `x`".into()
        )]
    );
    assert_eq!(
        codes_at("fn f()\n  if let y := x\n    print(1)\n"),
        [(20, 18)]
    );
    let src = "fn f()\n  x = y = z\n";
    assert_eq!(codes_at(src), [(28, at(src, "=", 1))]);
    // A faulty `for` head without a block is one diagnostic, not two.
    let src = "fn f()\n  for x of xs\n  print(1)\n";
    assert_eq!(codes_at(src), [(28, at(src, "of", 0))]);
    let src = "fn f()\n  if 1 2\n  print(1)\n";
    assert_eq!(codes_at(src), [(28, at(src, "2", 0))]);
}

// ----- views -----

#[test]
fn elements_values_attributes_and_handlers() {
    let src = "view Main\n  row\n  text a look: b\n  col scroll: end\n  text x ~t ~u(v) k: 1\n  \
               button \"Go\" on click go(1)\n  input bind: d on enter d = \"\"\n  entry\n    \
               on submit save(it)\n    text it\n  text {1, 2}\n  img - n\n";
    assert_eq!(
        tree(src),
        "view Main\n  <row>\n  <text a (look: b)>\n  <col (scroll: end)>\n  \
         <text x ~t (~u v) (k: 1)>\n  <button (str \"Go\") (on click (call go 1))>\n  \
         <input (bind: d) (on enter (= d (str )))>\n  <entry>\n    (on submit (call save it))\n    \
         <text it>\n  <text (set 1 2)>\n  <img (neg n)>\n"
    );
    assert_eq!(
        tree("view Card(t: Team, n: Int?)\n  text n\nview Empty()\n  row\n"),
        "view Card(t: Team, n: Int?)\n  <text n>\nview Empty()\n  <row>\n"
    );
}

#[test]
fn view_if_and_for() {
    let src = "view Main\n  if let r = current\n    text r\n  else\n    text \"none\"\n  \
               for m in Message where room == r last 5\n    if m.pending\n      text m.text\n";
    assert_eq!(
        tree(src),
        "view Main\n  if let r current\n    <text r>\n  else\n    <text (str \"none\")>\n  \
         for m (query Message (where (== room r)) (last 5))\n    if (. m pending)\n      \
         <text (. m text)>\n"
    );
    let src = "view Main\n  if a\n    text 1\n  else if b\n    text 2\n  else\n    text 3\n";
    assert_eq!(codes_at(src), [(24, at(src, "if", 1))]);
    let (m, _, _) = run(src, ParseLimits::DEFAULT);
    assert_eq!(
        dump::module(&m),
        "view Main\n  if a\n    <text 1>\n  else\n    if b\n      <text 2>\n    else\n      \
         <text 3>\n"
    );
}

#[test]
fn view_ranges() -> R {
    let src = "view Main\n  col\n    text a ~t(x) k: 1 on click f()\n      on hover g()\n  row\n";
    let (m, _, diags) = run(src, ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let Some(Item::View(view)) = m.items().first() else {
        return Err("no view".into());
    };
    assert_eq!(text(src, view.range), src.trim_end());
    let col = m.view_node(*view.nodes.first().must()?).must()?;
    assert_eq!(
        text(src, col.range),
        "col\n    text a ~t(x) k: 1 on click f()\n      on hover g()"
    );
    let ViewNodeKind::Element(col) = &col.kind else {
        return Err("no element".into());
    };
    let Some(ElementChild::Node(id)) = col.children.first() else {
        return Err("no child".into());
    };
    let ViewNodeKind::Element(element) = &m.view_node(*id).must()?.kind else {
        return Err("no element".into());
    };
    let attrs: Vec<&str> = element.attrs.iter().map(|a| text(src, a.range)).collect();
    assert_eq!(attrs, ["~t(x)", "k: 1"]);
    assert_eq!(
        text(src, element.handler.as_ref().must()?.range),
        "on click f()"
    );
    let Some(ElementChild::Handler(hover)) = element.children.first() else {
        return Err("no handler line".into());
    };
    assert_eq!(text(src, hover.range), "on hover g()");
    Ok(())
}

#[test]
fn faults_in_views() {
    let src = "view Main\n  on click f()\n  text a b\n  text a ~\n  text a on\n  button on click\n  \
               fn f()\n  row\n";
    assert_eq!(
        diags(src),
        [
            (
                28,
                at(src, "on", 0),
                "expected an element, `if` or `for` (an `on` line stands below an element), \
                 found `on`"
                    .into()
            ),
            (
                28,
                at(src, "b", 0),
                "expected end of line, found `b`".into()
            ),
            (
                28,
                at(src, "\n", 3),
                "expected a style tag name after `~`, found end of line".into()
            ),
            (
                28,
                at(src, "\n", 4),
                "expected an event name such as `click` after `on`, found end of line".into()
            ),
            (
                28,
                at(src, "\n", 5),
                "expected an action after `on click`, found end of line".into()
            ),
            (
                28,
                at(src, "fn", 0),
                "expected an element, `if` or `for`, found `fn`".into()
            ),
        ]
    );
    // A faulty element line still parses the lines below it.
    let src = "view Main\n  row x y\n    text a b\n    on click\n";
    assert_eq!(
        codes_at(src),
        [
            (28, at(src, "y", 0)),
            (28, at(src, "b", 0)),
            (28, at(src, "\n", 3))
        ]
    );
    let src = "view Main\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "\n", 0) + 1,
            "expected an indented block with the elements of `Main`, found end of file".into()
        )]
    );
    let src = "view Main\n  if a\n  text b\n";
    assert_eq!(codes_at(src), [(28, at(src, "text", 0))]);
    let src = "view Main\n  for x in\n    text x\n";
    assert_eq!(codes_at(src), [(28, at(src, "\n", 1))]);
    // Block heads ending in `:` or `{` are reported as in function bodies.
    let src = "view Main:\n  row:\n    text a\n  col {\n    text b\n  }\n";
    assert_eq!(
        codes_at(src),
        [
            (23, at(src, ":", 0)),
            (23, at(src, ":", 1)),
            (22, at(src, "{", 0))
        ]
    );
    assert_eq!(codes_at("view Main\n  text x += 1\n"), [(20, 19)]);
}

#[test]
fn style_keeps_its_body_as_a_range() -> R {
    let src = "style\n  .a {\n    x: y\n  }\n\nview Main\n  row\n";
    let (m, _, found) = run(src, ParseLimits::DEFAULT);
    assert!(found.is_empty(), "{found:?}");
    let Some(Item::Style(style)) = m.items().first() else {
        return Err("no style".into());
    };
    assert_eq!(text(src, style.body), ".a {\n    x: y\n  }");
    assert_eq!(text(src, style.range), "style\n  .a {\n    x: y\n  }");
    assert_eq!(m.items().len(), 2);
    let src = "style\nview Main\n  row\n";
    assert_eq!(
        diags(src),
        [(
            28,
            at(src, "view", 0),
            "expected an indented CSS block after `style`, found `view`".into()
        )]
    );
    // A faulty head is one diagnostic; its body is skipped.
    let src = "style x\n  .a {\n  }\n";
    assert_eq!(codes_at(src), [(28, at(src, "x", 0))]);
    Ok(())
}

// ----- limits and work -----

/// `view Main` with `levels` nested `col` elements around `text 1`.
fn nested_cols(levels: usize) -> String {
    let mut src = String::from("view Main\n");
    for level in 0..levels {
        src.push_str(&"  ".repeat(level + 1));
        src.push_str("col\n");
    }
    src.push_str(&"  ".repeat(levels + 1));
    src.push_str("text 1\n");
    src
}

fn limits(max_depth: u32, max_nodes: u32, max_height: u32) -> ParseLimits {
    ParseLimits {
        max_depth,
        tree: Limits {
            max_nodes,
            max_height,
        },
    }
}

#[test]
fn view_nesting_counts_against_the_depth_limit() {
    // The view body is level 1 and every element block one more.
    let src = nested_cols(9);
    let (_, _, diags) = run(&src, limits(10, MAX_NODES, MAX_HEIGHT));
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(&src, limits(9, MAX_NODES, MAX_HEIGHT));
    let found: Vec<u16> = diags.iter().map(|d| d.code.number()).collect();
    assert_eq!(found, [29]);
    // The default limit holds for deep views without exhausting the stack.
    let (_, _, diags) = run(&nested_cols(255), ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(&nested_cols(256), ParseLimits::DEFAULT);
    let found: Vec<u16> = diags.iter().map(|d| d.code.number()).collect();
    assert_eq!(found, [29]);
    assert_eq!(MAX_DEPTH, 256);
}

#[test]
fn node_and_height_limits_are_exact_for_v0_2_items() {
    let src = include_str!("goldens/tracker_excerpt.ostl");
    let (m, _, diags) = run(src, ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let (nodes, height) = (m.node_count(), m.height());
    let (_, _, diags) = run(src, limits(MAX_DEPTH, nodes, height));
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(src, limits(MAX_DEPTH, nodes - 1, MAX_HEIGHT));
    let found: Vec<u16> = diags.iter().map(|d| d.code.number()).collect();
    assert_eq!(found, [31]);
    let (_, _, diags) = run(src, limits(MAX_DEPTH, MAX_NODES, height - 1));
    let found: Vec<u16> = diags.iter().map(|d| d.code.number()).collect();
    assert_eq!(found, [30]);
}

/// Parser work for `src`, which must parse cleanly (D74: no wall clock).
fn steps_for(src: &str) -> u64 {
    let (tokens, lexed) = lex(src, F);
    let mut p = Parser::new(src, F, &tokens, &lexed, ParseLimits::DEFAULT, &Exprs);
    let _ = p.program();
    assert!(p.diags.is_empty(), "{:?}", p.diags);
    p.steps()
}

#[test]
fn v0_2_items_are_linear() {
    let file = |n: usize| {
        let mut src = String::new();
        for i in 0..n {
            src.push_str(&format!(
                "data D{i}\n  a: Set[Int] = {{1}} fixed\n  see if me in a\n\n\
                 view V{i}(x: Int?)\n  row ~t(x == none) k: [1: 2]\n    on click f(x ?? 1)\n  \
                 for r in D{i} where a == {{}} sort a last 2\n    text r.a\n\n"
            ));
        }
        src
    };
    let small = steps_for(&file(500));
    let large = steps_for(&file(1_000));
    assert!(large <= 2 * small + 64, "{small} steps, then {large}");
    let tokens = lex(&file(500), F).0.len();
    let per_token = small / u64::try_from(tokens).unwrap_or(u64::MAX);
    assert!(per_token <= 16, "{per_token} steps per token");
}

#[test]
fn every_assignment_kind_is_built() -> R {
    // Statement and handler assignments keep target and value apart.
    let src = "fn f()\n  a.b = c\n";
    let (m, _, diags) = run(src, ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let Some(Item::Fn(decl)) = m.items().first() else {
        return Err("no fn".into());
    };
    let block = m.block(decl.body).must()?;
    let stmt = m.stmt(*block.stmts.first().must()?).must()?;
    let StmtKind::Assign { target, value } = stmt.kind else {
        return Err("no assignment".into());
    };
    assert!(matches!(
        m.expr(target).must()?.kind,
        ExprKind::Field { .. }
    ));
    assert!(matches!(m.expr(value).must()?.kind, ExprKind::Name(_)));
    assert_eq!(text(src, stmt.range), "a.b = c");
    Ok(())
}
