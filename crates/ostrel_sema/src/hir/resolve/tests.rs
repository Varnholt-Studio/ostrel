use ostrel_core::{Diagnostic, FileId};

use super::resolve;
use crate::hir::*;

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

fn run(src: &str) -> (Program, Vec<Diagnostic>) {
    let module = parse(src);
    resolve(&[Source {
        file: FILE,
        text: src,
        module: &module,
    }])
}

/// `E0204 3:5` per diagnostic, line and column 1 based (ASCII sources).
fn diags(src: &str) -> Vec<String> {
    run(src)
        .1
        .iter()
        .map(|d| {
            let before = &src[..d.span.start as usize];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
            format!("{} {line}:{column}", d.code)
        })
        .collect()
}

fn clean(src: &str) -> Program {
    let (program, diagnostics) = run(src);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    program
}

fn def(p: &Program, name: &str) -> DefId {
    DefId(p.defs.iter().position(|d| &*d.name == name).unwrap() as u32)
}

fn names(p: &Program) -> Vec<Res> {
    p.exprs
        .iter()
        .filter_map(|e| match e.kind {
            ExprKind::Name(res) => Some(res),
            _ => None,
        })
        .collect()
}

fn model<'p>(p: &'p Program, name: &str) -> &'p ModelDef {
    match &p.defs[def(p, name).index()].kind {
        DefKind::Model(m) => m,
        other => panic!("{other:?}"),
    }
}

const NOTES: &str = "\
app Notes

data Note
  title: Text
  body: Text = \"\"
  pinned: Bool = false
  check title.len in 1..200
  see if true
  edit title if not pinned
  drop if not pinned

fn deleteNote(n: Note)
  drop n

view Main
  col
    entry hint: \"New note\"
      on submit make Note { title: it, body: \"\", pinned: false }
    for n in Note sort title
      col
        text n.title
        if n.pinned
          button \"Unpin\" on click n.pinned = false
        else
          button \"Delete\" on click deleteNote(n)
";

#[test]
fn notes_app_resolves_without_diagnostics() {
    let p = clean(NOTES);
    let note = def(&p, "Note");
    let main = def(&p, "Main");
    let app = p.app.unwrap();
    assert!(
        matches!(&p.defs[app.index()].kind, DefKind::App(a) if a.home == Some(main) && a.auth.is_none())
    );
    let m = model(&p, "Note");
    assert_eq!(m.fields.len(), 3);
    assert!(m.fields[1].default.is_some() && m.fields[0].default.is_none());
    assert_eq!(m.checks.len(), 1);
    assert_eq!(m.rules.len(), 3);
    assert_eq!(m.rules[1].op, RuleOp::Edit);
    assert_eq!(m.rules[1].field, Some(FieldId(0)));
    let title = (note, FieldRef::Declared(FieldId(0)));
    let pinned = (note, FieldRef::Declared(FieldId(2)));
    // Bare names in rules and checks are fields of the model.
    assert!(names(&p).contains(&Res::Field(note, pinned.1)));
    // `n.title` and `n.pinned` in the view are fields through the loop variable.
    let fields: Vec<_> = p
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            ExprKind::Field { field, .. } => Some(field),
            _ => None,
        })
        .collect();
    assert!(fields.contains(&title) && fields.contains(&pinned));
    // `title.len` in the check is a property of a `Text`.
    assert!(
        p.exprs
            .iter()
            .any(|e| matches!(&e.kind, ExprKind::Property { name, .. } if &**name == "len"))
    );
    assert!(p.exprs.iter().any(|e| matches!(
        &e.kind,
        ExprKind::Query(q) if q.model == note && q.sort == Some((title.1, false))
    )));
    assert!(names(&p).contains(&Res::Context(ContextName::It)));
    assert!(names(&p).contains(&Res::Def(def(&p, "deleteNote"))));
    assert!(p.exprs.iter().any(|e| matches!(
        &e.kind,
        ExprKind::Make { model, fields } if *model == note && fields.len() == 3
    )));
}

#[test]
fn functions_have_one_node_and_see_later_declarations() {
    let p = clean(
        "fn main()\n  print(twice(2))\n\
         fn twice(x: Int) -> Int\n  x * 2\n\
         server fn rename(t: Topic, name: Text)\n  call if t.owner == me\n  t.name = name\n\
         data Topic\n  name: Text\n  owner: Text fixed\n",
    );
    let DefKind::Fn(twice) = &p.defs[def(&p, "twice").index()].kind else {
        panic!()
    };
    assert_eq!(twice.result, Ty::Int);
    assert_eq!(twice.declared, None);
    assert_eq!(p.locals[twice.params[0].index()].ty, Some(Ty::Int));
    let DefKind::Fn(rename) = &p.defs[def(&p, "rename").index()].kind else {
        panic!()
    };
    assert_eq!(rename.declared, Some(Placement::Server));
    let topic = def(&p, "Topic");
    assert_eq!(p.locals[rename.params[0].index()].ty, Some(Ty::Ref(topic)));
    let rule = rename.call_rule.as_ref().unwrap();
    assert_eq!(rule.op, RuleOp::Call);
    assert!(matches!(
        p.exprs[rule.cond.index()].kind,
        ExprKind::Binary { lhs, .. }
            if matches!(p.exprs[lhs.index()].kind, ExprKind::Field { field: (m, FieldRef::Declared(FieldId(1))), .. } if m == topic)
    ));
    assert!(names(&p).contains(&Res::Std(StdName::Print)));
    assert!(names(&p).contains(&Res::Local(twice.params[0])));
}

#[test]
fn types_resolve_to_the_type_table() {
    let p = clean(
        "enum Role = member | admin\n\
         data Team\n  name: Text?\n  roles: Map[Team, Role]\n  tags: Set[Text]\n  order: List[Rank]\n  at: Time\n  raw: Bytes\n  score: Float\n",
    );
    let team = def(&p, "Team");
    let role = def(&p, "Role");
    let tys: Vec<_> = model(&p, "Team")
        .fields
        .iter()
        .map(|f| f.ty.clone())
        .collect();
    assert_eq!(
        tys,
        vec![
            Ty::Optional(Box::new(Ty::Text)),
            Ty::Map(Box::new(Ty::Ref(team)), Box::new(Ty::Enum(role))),
            Ty::Set(Box::new(Ty::Text)),
            Ty::List(Box::new(Ty::Rank)),
            Ty::Time,
            Ty::Bytes,
            Ty::Float,
        ]
    );
}

#[test]
fn bad_types_are_e0203_once() {
    assert_eq!(diags("data A\n  b: Nope\n"), ["E0203 2:6"]);
    assert_eq!(diags("data A\n  b: Set\n"), ["E0203 2:6"]);
    assert_eq!(diags("data A\n  b: Int[Text]\n"), ["E0203 2:6"]);
    assert_eq!(diags("data A\n  b: Set[Nope]?\n"), ["E0203 2:10"]);
    assert_eq!(diags("fn f() -> Nope\n  1\n"), ["E0203 1:11"]);
    assert_eq!(diags("fn g()\n  1\ndata A\n  b: g\n"), ["E0203 4:6"]);
}

#[test]
fn unknown_and_duplicate_names() {
    assert_eq!(diags("fn main()\n  print(nope)\n"), ["E0200 2:9"]);
    assert_eq!(diags("fn f()\n  1\ndata f\n  a: Int\n"), ["E0201 3:6"]);
    assert_eq!(diags("fn f(a: Int, a: Int)\n  1\n"), ["E0202 1:14"]);
    assert_eq!(diags("data A\n  a: Int\n  see if b\n"), ["E0200 3:10"]);
    // The loop variable is bound in the body only.
    assert_eq!(
        diags("fn f(xs: List[Int])\n  for x in xs\n    print(x)\n  print(x)\n"),
        ["E0200 4:9"]
    );
    // `let` is visible after its statement only (SPEC 12.3).
    assert_eq!(diags("fn f()\n  let a = a\n"), ["E0200 2:11"]);
}

#[test]
fn unknown_field_is_e0204_at_the_field_name() {
    let data = "data Note\n  title: Text\n  see if true\n";
    let src = format!("{data}fn f(n: Note) -> Text\n  n.titel\n");
    assert_eq!(diags(&src), ["E0204 5:5"]);
    let src = format!("{data}fn f()\n  make Note {{ titel: \"x\" }}\n");
    assert_eq!(diags(&src), ["E0204 5:15"]);
    let src = format!("{data}fn f()\n  make Note {{ id: \"x\" }}\n");
    assert_eq!(diags(&src), ["E0204 5:15"]);
    let src = format!("{data}fn f()\n  for n in Note sort titel\n    print(1)\n");
    assert_eq!(diags(&src), ["E0204 5:22"]);
    assert_eq!(diags("data A\n  a: Int\n  edit b if true\n"), ["E0204 3:8"]);
    assert_eq!(
        diags("data A\n  a: Int\n  edit made if true\n"),
        ["E0204 3:8"]
    );
    // Implicit fields resolve; a field of an unknown base is left to `check`.
    clean(&format!("{data}fn f(n: Note) -> Text\n  n.id\n"));
    let p = clean("fn f(t: Text) -> Int\n  t.len\n");
    assert!(
        p.exprs
            .iter()
            .any(|e| matches!(&e.kind, ExprKind::Property { .. }))
    );
}

#[test]
fn unknown_model_is_e0205_at_the_type_name() {
    assert_eq!(diags("fn f()\n  make Nope { a: 1 }\n"), ["E0205 2:8"]);
    assert_eq!(
        diags("enum E = a\nfn f()\n  make E { a: 1 }\n"),
        ["E0205 3:8"]
    );
    assert_eq!(
        diags("enum E = a | b\nfn f()\n  for x in E where x\n    print(1)\n"),
        ["E0205 3:12"]
    );
    assert_eq!(
        diags("fn f()\n  for x in Nope sort a\n    print(1)\n"),
        ["E0205 2:12"]
    );
}

#[test]
fn duplicate_member_is_e0206_at_the_second_name() {
    assert_eq!(diags("data A\n  a: Int\n  a: Text\n"), ["E0206 3:3"]);
    assert_eq!(diags("data A\n  id: Int\n"), ["E0206 2:3"]);
    assert_eq!(diags("enum E = a | b | a\n"), ["E0206 1:18"]);
    assert_eq!(
        diags("data A\n  a: Int\nfn f()\n  make A { a: 1, a: 2 }\n"),
        ["E0206 4:18"]
    );
    assert_eq!(
        diags("app X\n  home Main\n  home Main\nview Main\n  col\n"),
        ["E0206 3:3"]
    );
}

#[test]
fn query_heads_name_models_enums_or_values() {
    let p = clean(
        "enum Status = open | done\n\
         data Issue\n  status: Status\n  see if status == done\n\
         fn f(xs: List[Issue])\n  for s in Status\n    print(1)\n  for x in xs\n    x.status = open\n  drop Issue where status == done\n",
    );
    let status = def(&p, "Status");
    let issue = def(&p, "Issue");
    assert!(names(&p).contains(&Res::Def(status)));
    assert!(names(&p).contains(&Res::Variant(status, 1)));
    assert!(names(&p).contains(&Res::Variant(status, 0)));
    assert!(names(&p).contains(&Res::Field(issue, FieldRef::Declared(FieldId(0)))));
    // `x` iterates a list of rows, so `x.status` is a field.
    assert!(
        p.exprs
            .iter()
            .any(|e| matches!(e.kind, ExprKind::Field { field: (m, _), .. } if m == issue))
    );
    assert!(
        p.exprs
            .iter()
            .any(|e| matches!(&e.kind, ExprKind::Query(q) if q.filter.is_some()))
    );
    // A variant name shared by two enums is ambiguous.
    assert_eq!(
        diags("enum A = x\nenum B = x\nfn f()\n  print(x)\n"),
        ["E0200 4:9"]
    );
}

#[test]
fn contextual_names_are_bound_where_they_have_meaning() {
    let p = clean(
        "data Room\n  members: Set[Room]\n  name: Text\n  edit if new.name != \"\"\n  edit members if added == {} or removed == {}\n",
    );
    let room = def(&p, "Room");
    let res = names(&p);
    assert!(res.contains(&Res::Context(ContextName::New)));
    assert!(res.contains(&Res::Context(ContextName::Added)));
    assert!(res.contains(&Res::Context(ContextName::Removed)));
    // `new.name` is a field of the proposed record.
    assert!(p.exprs.iter().any(|e| matches!(e.kind, ExprKind::Field { field: (m, FieldRef::Declared(FieldId(1))), .. } if m == room)));
    assert_eq!(
        diags("data A\n  a: Int\n  see if new.a == 1\n"),
        ["E0200 3:10"]
    );
    assert_eq!(
        diags("data A\n  a: Int\n  edit a if added == 1\n"),
        ["E0200 3:13"]
    );
    assert_eq!(diags("fn f()\n  print(it)\n"), ["E0200 2:9"]);
    // A field type that did not resolve gives no follow up in its rules.
    assert_eq!(
        diags("data A\n  s: Set[Nope]\n  edit s if added == {}\n"),
        ["E0203 2:10"]
    );
}

#[test]
fn serial_per_resolves_declared_and_implicit_fields() {
    let p = clean(
        "data T\n  team: Text fixed\n  n: Int serial per team\n  m: Int serial per author\n  k: Int serial\n",
    );
    let modes: Vec<_> = model(&p, "T").fields.iter().map(|f| f.mode).collect();
    assert_eq!(
        modes,
        vec![
            FieldMode::Fixed,
            FieldMode::Serial(Some(FieldRef::Declared(FieldId(0)))),
            FieldMode::Serial(Some(FieldRef::Implicit(ImplicitField::Author))),
            FieldMode::Serial(None),
        ]
    );
    assert_eq!(diags("data T\n  n: Int serial per nope\n"), ["E0204 2:21"]);
}

#[test]
fn locals_named_like_fields_are_e0207_at_their_first_use() {
    let data = "data Post\n  pos: Int\n  text: Text\n  see if true\n";
    // Used in `where`: at the use; the field wins.
    let src = format!("{data}fn f(pos: Int)\n  for p in Post where pos == 1\n    print(1)\n");
    assert_eq!(diags(&src), ["E0207 6:23"]);
    // Not mentioned: at the type name in the head (SPEC 12.10, S-06).
    let src = format!("{data}fn f(text: Text)\n  drop Post where pos == 1\n");
    assert_eq!(diags(&src), ["E0207 6:8"]);
    // Two clashing names: two faults in source order; `sort` and `n` count.
    let src = format!("{data}fn f(text: Text, pos: Int)\n  drop Post sort pos limit text.len\n");
    assert_eq!(diags(&src), ["E0207 6:18", "E0207 6:28"]);
    // The own loop variable, with a matching field and without one.
    let src = format!("{data}fn f()\n  for pos in Post where pos == 1\n    print(1)\n");
    assert_eq!(diags(&src), ["E0207 6:25"]);
    let src = format!("{data}fn f(q: Int)\n  for q in Post limit q\n    print(1)\n");
    assert_eq!(diags(&src), ["E0207 6:23"]);
    // A `var` counts as a local; a query without `where` and `sort` is free.
    let src = format!("{data}var pos = 1\nfn f()\n  drop Post sort text\n");
    assert_eq!(diags(&src), ["E0207 7:8"]);
    clean(&format!("{data}fn f(pos: Int)\n  drop Post limit pos\n"));
}

#[test]
fn files_share_one_program_in_file_order() {
    let a = parse("fn main()\n  print(helper())\n");
    let b = parse("fn helper() -> Int\n  1\nfn main()\n  print(2)\n");
    let (p, d) = resolve(&[
        Source {
            file: FILE,
            text: "",
            module: &a,
        },
        Source {
            file: FileId::from_raw(2),
            text: "",
            module: &b,
        },
    ]);
    assert_eq!(
        p.defs.iter().map(|d| &*d.name).collect::<Vec<_>>(),
        ["main", "helper", "main"]
    );
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].code.number(), d[0].span.file),
        (201, FileId::from_raw(2))
    );
    assert!(names(&p).contains(&Res::Def(DefId(1))));
}

#[test]
fn app_lines_are_validated() {
    let p = clean("app A\n  auth paseto invite\n  home Start\nview Start\n  col\n");
    assert!(matches!(
        &p.defs[0].kind,
        DefKind::App(a) if a.auth == Some(Registration::Invite) && a.home == Some(DefId(1))
    ));
    assert_eq!(diags("app A\n  auth jwt open\n"), ["E0200 2:8"]);
    assert_eq!(diags("app A\n  auth paseto closed\n"), ["E0200 2:15"]);
    assert_eq!(diags("app A\n  home Nope\n"), ["E0200 2:8"]);
    assert_eq!(diags("app A\napp B\n"), ["E0201 2:5"]);
}

#[test]
fn every_example_resolves_without_panic() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let mut stack = vec![std::path::PathBuf::from(root)];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ostl") {
                let src = std::fs::read_to_string(&path).unwrap();
                let (tokens, lexed) = ostrel_syntax::lex::lex(&src, FILE);
                let (module, _) = ostrel_syntax::parse::parse(&src, FILE, &tokens, &lexed);
                let _ = resolve(&[Source {
                    file: FILE,
                    text: &src,
                    module: &module,
                }]);
                seen += 1;
            }
        }
    }
    assert!(seen > 10);
}
