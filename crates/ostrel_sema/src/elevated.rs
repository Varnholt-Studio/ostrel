//! Elevated functions: the list behind `ostrel check --list-elevated` and the
//! warning for results that leave the `see` rules (AC-32, AC-54, D34, D99,
//! ARCHITECTURE 5.7).
//!
//! A function marked `as system` runs without rule checks. Its result is not
//! filtered by `see` either, which is the purpose of `as system` (D99), so a
//! result type that contains a model hands rows to the caller that the
//! caller's `see` rule never looked at. [`list`] reports every elevated
//! function with the client reachable paths into it, and [`warnings`] gives
//! one warning per elevated function whose result type contains a model.
//!
//! A client enters the server only by a `Call` of a function with a `call`
//! rule (default deny, D8, D147). A client reachable path therefore starts at
//! such a function and follows the functions named in bodies and `call`
//! rules down to the elevated function. Each entry contributes its shortest
//! path; among paths of equal length the one through earlier declarations
//! wins, so the list is stable.
//!
//! Both functions read a program that `hir::check` accepted. On any other
//! program they still terminate and never panic, but the list may be
//! incomplete.

use std::collections::{BTreeSet, VecDeque};

use ostrel_core::{Code, Diagnostic, Span};

use crate::hir::{
    BlockId, DefId, DefKind, ExprId, ExprKind, FnDef, Program, Res, StmtKind, StrPart, Ty,
};

/// Warning: an elevated function returns rows without `see` (D99). The code
/// is an assumption until SPEC 12.6 reserves one (question #2462).
pub const UNFILTERED_ROWS: Code = Code::new(311);

/// One elevated function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elevated {
    /// The function.
    pub def: DefId,
    /// Its name.
    pub name: Box<str>,
    /// The whole declaration.
    pub span: Span,
    /// The span of `as system`.
    pub as_system: Span,
    /// The declared result type, [`Ty::Unit`] without `-> T`.
    pub result: Ty,
    /// The first model the result type contains, in reading order; set when
    /// the function returns rows without `see`.
    pub unfiltered: Option<DefId>,
    /// One path per client entry that reaches the function, each from the
    /// entry to the function itself, in declaration order of the entries.
    pub paths: Vec<Vec<DefId>>,
}

/// Every elevated function of `program` in declaration order, with its client
/// reachable paths and whether it returns rows without `see`.
pub fn list(program: &Program) -> Vec<Elevated> {
    let graph = CallGraph::new(program);
    let mut out = Vec::new();
    for (index, def) in program.defs.iter().enumerate() {
        let DefKind::Fn(f) = &def.kind else {
            continue;
        };
        let Some(as_system) = f.elevated else {
            continue;
        };
        let id = DefId(u32::try_from(index).unwrap_or(u32::MAX));
        let paths = graph
            .entries
            .iter()
            .filter_map(|&entry| graph.shortest_path(entry, id))
            .collect();
        out.push(Elevated {
            def: id,
            name: def.name.clone(),
            span: def.span,
            as_system,
            result: f.result.clone(),
            unfiltered: first_model(program, &f.result),
            paths,
        });
    }
    out
}

/// One warning [`UNFILTERED_ROWS`] at `as system` for every elevated function
/// whose result type contains a model, in declaration order.
pub fn warnings(program: &Program) -> Vec<Diagnostic> {
    list(program)
        .iter()
        .filter_map(|e| {
            let model = e.unfiltered?;
            let message = format!(
                "`{}` returns rows without see: `as system` does not filter its result `{}` \
                 by the `see` rule of `{}`",
                e.name,
                type_name(program, &e.result),
                def_name(program, model)
            );
            Some(Diagnostic::warning(UNFILTERED_ROWS, e.as_system, message))
        })
        .collect()
}

/// The type as the source writes it, for messages and the list.
pub fn type_name(program: &Program, ty: &Ty) -> String {
    match ty {
        Ty::Bool => "Bool".into(),
        Ty::Int => "Int".into(),
        Ty::Float => "Float".into(),
        Ty::Text => "Text".into(),
        Ty::Time => "Time".into(),
        Ty::Bytes => "Bytes".into(),
        Ty::Rank => "Rank".into(),
        Ty::Url => "Url".into(),
        Ty::Enum(d) | Ty::Ref(d) => def_name(program, *d),
        Ty::Optional(t) => format!("{}?", type_name(program, t)),
        Ty::Set(t) => format!("Set[{}]", type_name(program, t)),
        Ty::List(t) => format!("List[{}]", type_name(program, t)),
        Ty::Map(k, v) => format!("Map[{}, {}]", type_name(program, k), type_name(program, v)),
        Ty::Remote(t) => format!("Remote {}", type_name(program, t)),
        Ty::Rows(d) => format!("rows of {}", def_name(program, *d)),
        Ty::Range(t) => format!("range of {}", type_name(program, t)),
        Ty::Unit => "nothing".into(),
        Ty::Error => "?".into(),
    }
}

fn def_name(program: &Program, def: DefId) -> String {
    program
        .def(def)
        .map_or_else(|| "?".to_string(), |d| d.name.to_string())
}

/// The first model in `ty`, reading the type from left to right. A reference
/// and the rows of a query both carry rows of their model; an enum does not.
fn first_model(program: &Program, ty: &Ty) -> Option<DefId> {
    match ty {
        Ty::Ref(d) | Ty::Rows(d) => {
            matches!(program.def(*d).map(|d| &d.kind), Some(DefKind::Model(_))).then_some(*d)
        }
        Ty::Optional(t) | Ty::Set(t) | Ty::List(t) | Ty::Remote(t) | Ty::Range(t) => {
            first_model(program, t)
        }
        Ty::Map(k, v) => first_model(program, k).or_else(|| first_model(program, v)),
        Ty::Bool
        | Ty::Int
        | Ty::Float
        | Ty::Text
        | Ty::Time
        | Ty::Bytes
        | Ty::Rank
        | Ty::Url
        | Ty::Enum(_)
        | Ty::Unit
        | Ty::Error => None,
    }
}

/// Which functions each function names, and where clients enter.
struct CallGraph {
    /// Per definition the functions its body and `call` rule name, in id
    /// order; empty for every definition that is not a function.
    callees: Vec<BTreeSet<DefId>>,
    /// Functions with a `call` rule, in id order.
    entries: Vec<DefId>,
}

impl CallGraph {
    fn new(program: &Program) -> Self {
        let mut callees = Vec::with_capacity(program.defs.len());
        let mut entries = Vec::new();
        for (index, def) in program.defs.iter().enumerate() {
            let mut named = BTreeSet::new();
            if let DefKind::Fn(f) = &def.kind {
                if f.call_rule.is_some() {
                    entries.push(DefId(u32::try_from(index).unwrap_or(u32::MAX)));
                }
                named = named_fns(program, f);
            }
            callees.push(named);
        }
        CallGraph { callees, entries }
    }

    /// The shortest path from `from` to `to` (both included), preferring
    /// lower ids among paths of equal length; `None` if `to` is unreachable.
    fn shortest_path(&self, from: DefId, to: DefId) -> Option<Vec<DefId>> {
        let mut parent: Vec<Option<DefId>> = vec![None; self.callees.len()];
        let mut seen = vec![false; self.callees.len()];
        *seen.get_mut(from.index())? = true;
        let mut queue = VecDeque::from([from]);
        while let Some(at) = queue.pop_front() {
            if at == to {
                let mut path = vec![to];
                let mut cur = to;
                while let Some(prev) = parent.get(cur.index()).copied().flatten() {
                    path.push(prev);
                    cur = prev;
                }
                path.reverse();
                return Some(path);
            }
            for &next in self.callees.get(at.index())? {
                if let Some(flag) = seen.get_mut(next.index())
                    && !*flag
                {
                    *flag = true;
                    if let Some(slot) = parent.get_mut(next.index()) {
                        *slot = Some(at);
                    }
                    queue.push_back(next);
                }
            }
        }
        None
    }
}

/// Every function named in the body or the `call` rule of `f`. A name counts
/// whether it is called or not, because a function value can be called later.
fn named_fns(program: &Program, f: &FnDef) -> BTreeSet<DefId> {
    enum Node {
        Block(BlockId),
        Expr(ExprId),
    }
    let mut out = BTreeSet::new();
    let mut stack = vec![Node::Block(f.body)];
    if let Some(rule) = &f.call_rule {
        stack.push(Node::Expr(rule.cond));
    }
    while let Some(node) = stack.pop() {
        match node {
            Node::Block(id) => {
                let Some(block) = program.block(id) else {
                    continue;
                };
                for stmt in block.stmts.iter().filter_map(|s| program.stmt(*s)) {
                    match &stmt.kind {
                        StmtKind::Let { value: e, .. }
                        | StmtKind::Return(e)
                        | StmtKind::Expr(e)
                        | StmtKind::Drop(e) => stack.push(Node::Expr(*e)),
                        StmtKind::If {
                            cond,
                            then_block,
                            else_block,
                        } => {
                            stack.push(Node::Expr(*cond));
                            stack.push(Node::Block(*then_block));
                            stack.extend(else_block.map(Node::Block));
                        }
                        StmtKind::IfLet {
                            value,
                            then_block,
                            else_block,
                            ..
                        } => {
                            stack.push(Node::Expr(*value));
                            stack.push(Node::Block(*then_block));
                            stack.extend(else_block.map(Node::Block));
                        }
                        StmtKind::For { source, body, .. } => {
                            stack.push(Node::Expr(*source));
                            stack.push(Node::Block(*body));
                        }
                        StmtKind::Assign { target, value } => {
                            stack.push(Node::Expr(*target));
                            stack.push(Node::Expr(*value));
                        }
                    }
                }
            }
            Node::Expr(id) => {
                let Some(expr) = program.expr(id) else {
                    continue;
                };
                let mut push = |e: &ExprId| stack.push(Node::Expr(*e));
                match &expr.kind {
                    ExprKind::Name(Res::Def(d))
                    | ExprKind::Word {
                        lexical: Res::Def(d),
                        ..
                    } => {
                        if matches!(program.def(*d).map(|d| &d.kind), Some(DefKind::Fn(_))) {
                            out.insert(*d);
                        }
                    }
                    ExprKind::Int(_)
                    | ExprKind::Bool(_)
                    | ExprKind::Name(_)
                    | ExprKind::Word { .. }
                    | ExprKind::NoneValue
                    | ExprKind::Me
                    | ExprKind::Now
                    | ExprKind::Signed => {}
                    ExprKind::Str(parts) => {
                        for part in parts.iter() {
                            if let StrPart::Interp(e) = part {
                                push(e);
                            }
                        }
                    }
                    ExprKind::Unary { operand: e, .. }
                    | ExprKind::Field { base: e, .. }
                    | ExprKind::Property { base: e, .. }
                    | ExprKind::Catch { value: e, .. } => push(e),
                    ExprKind::Binary { lhs: a, rhs: b, .. }
                    | ExprKind::Index { base: a, index: b }
                    | ExprKind::In {
                        element: a,
                        collection: b,
                        ..
                    }
                    | ExprKind::Range { lo: a, hi: b }
                    | ExprKind::Fallback {
                        value: a,
                        fallback: b,
                    } => {
                        push(a);
                        push(b);
                    }
                    ExprKind::Call { callee, args } => {
                        push(callee);
                        args.iter().for_each(&mut push);
                    }
                    ExprKind::Set(items) | ExprKind::List(items) => {
                        items.iter().for_each(&mut push);
                    }
                    ExprKind::Map(pairs) => {
                        for (k, v) in pairs.iter() {
                            push(k);
                            push(v);
                        }
                    }
                    ExprKind::Make { fields, .. } => {
                        fields.iter().for_each(|(_, e)| push(e));
                    }
                    ExprKind::Query(q) => {
                        if let Some(e) = &q.filter {
                            push(e);
                        }
                        if let Some((_, e)) = &q.take {
                            push(e);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ostrel_core::{FileId, SourceMap};

    use super::*;
    use crate::hir;

    const TEAMS: &str = "\
app Teams
  auth paseto open

data Team
  name: Text
  owners: Set[User]
  see if me in owners

data Note
  team: Team fixed
  text: Text
  see if me in team.owners
";

    /// Resolves and checks `src` after the `TEAMS` models; panics on an error,
    /// because these tests are about accepted programs.
    fn program(src: &str) -> (Program, FileId, SourceMap) {
        let text = format!("{TEAMS}\n{src}");
        let mut map = SourceMap::new();
        let file = map.add("t.ostl", text.clone()).unwrap();
        let (tokens, mut diags) = ostrel_syntax::lex::lex(&text, file);
        let (module, parsed) = ostrel_syntax::parse::parse(&text, file, &tokens, &diags);
        diags.extend(parsed);
        assert!(diags.is_empty(), "{diags:?}");
        let sources = [hir::Source {
            file,
            text: &text,
            module: &module,
        }];
        let (mut program, diags) = hir::resolve(&sources);
        assert!(diags.is_empty(), "{diags:?}");
        let diags = hir::check(&mut program);
        assert!(!diags.iter().any(Diagnostic::is_error), "{diags:?}");
        (program, file, map)
    }

    fn names(program: &Program, path: &[DefId]) -> String {
        path.iter()
            .map(|d| def_name(program, *d))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    fn paths(program: &Program, e: &Elevated) -> Vec<String> {
        e.paths.iter().map(|p| names(program, p)).collect()
    }

    fn rendered(map: &SourceMap, program: &Program) -> Vec<String> {
        warnings(program).iter().map(|d| d.render(map)).collect()
    }

    #[test]
    fn a_program_without_elevated_functions_lists_nothing() {
        let (p, _, map) =
            program("server fn rename(t: Team, n: Text)\n  call if me in t.owners\n  t.name = n\n");
        assert!(list(&p).is_empty());
        assert!(rendered(&map, &p).is_empty());
    }

    #[test]
    fn model_results_in_every_shape_are_marked() {
        for (result, model) in [
            ("Note", "Note"),
            ("Note?", "Note"),
            ("List[Note]", "Note"),
            ("Set[Team]", "Team"),
            ("Map[Text, Note]", "Note"),
            ("Map[Note, Team]", "Note"),
            ("List[Note?]", "Note"),
        ] {
            let src = format!(
                "server fn get(t: Team) -> {result} as system\n  call if me in t.owners\n  return get(t)\n"
            );
            let (p, _, map) = program(&src);
            let all = list(&p);
            assert_eq!(all.len(), 1, "{result}");
            let e = &all[0];
            assert_eq!(
                e.unfiltered.map(|d| def_name(&p, d)).as_deref(),
                Some(model),
                "{result}"
            );
            assert_eq!(type_name(&p, &e.result), result);
            let warns = rendered(&map, &p);
            assert_eq!(warns.len(), 1, "{result}");
            assert!(warns[0].starts_with("t.ostl:14:"), "{result}: {}", warns[0]);
            assert!(
                warns[0].contains(": warning[E0311]: `get` returns rows without see"),
                "{}",
                warns[0]
            );
            assert!(warns[0].contains(&format!("`{result}`")), "{}", warns[0]);
            assert!(
                warns[0].ends_with(&format!("the `see` rule of `{model}`")),
                "{}",
                warns[0]
            );
        }
    }

    #[test]
    fn results_without_a_model_are_not_marked() {
        for result in [
            "",
            " -> Int",
            " -> Text?",
            " -> List[Text]",
            " -> Map[Text, Int]",
            " -> Bool",
        ] {
            let body = if result.is_empty() {
                "  t.name = \"x\""
            } else {
                "  return count(t)"
            };
            let src = format!(
                "server fn count(t: Team){result} as system\n  call if me in t.owners\n{body}\n"
            );
            let (p, _, map) = program(&src);
            let all = list(&p);
            assert_eq!(all.len(), 1, "{result}");
            assert_eq!(all[0].unfiltered, None, "{result}");
            assert!(rendered(&map, &p).is_empty(), "{result}");
        }
    }

    #[test]
    fn an_enum_result_is_not_a_model() {
        let src = "enum Level = low | high\n\nserver fn level(t: Team) -> Level as system\n  call if me in t.owners\n  return low\n";
        let (p, _, map) = program(src);
        assert_eq!(list(&p)[0].unfiltered, None);
        assert!(rendered(&map, &p).is_empty());
    }

    #[test]
    fn a_plain_server_fn_returning_rows_is_not_warned() {
        // Without `as system` the caller's `see` filters every query (D99).
        let src =
            "server fn notes(t: Team) -> List[Note]\n  call if me in t.owners\n  return notes(t)\n";
        let (p, _, map) = program(src);
        assert!(list(&p).is_empty());
        assert!(rendered(&map, &p).is_empty());
    }

    #[test]
    fn paths_start_at_every_entry_and_take_the_shortest_route() {
        let src = "\
server fn purge(t: Team) as system
  call if me in t.owners
  t.name = \"\"

fn helper(t: Team)
  purge(t)

server fn cleanup(t: Team)
  call if me in t.owners
  helper(t)

server fn direct(t: Team)
  call if me in t.owners
  helper(t)
  purge(t)

server fn internal(t: Team)
  purge(t)

server fn unrelated(t: Team)
  call if me in t.owners
  internal(t)
";
        let (p, _, _) = program(src);
        let all = list(&p);
        assert_eq!(all.len(), 1);
        assert_eq!(
            paths(&p, &all[0]),
            [
                "purge",
                "cleanup -> helper -> purge",
                "direct -> purge",
                "unrelated -> internal -> purge",
            ]
        );
    }

    #[test]
    fn a_function_without_call_rule_is_no_entry() {
        // `internal` has no `call` rule, so a client `Call` of it is `Denied`
        // (D147); it appears only inside paths that start elsewhere.
        let src = "\
server fn purge(t: Team) as system
  call if me in t.owners
  t.name = \"\"

server fn internal(t: Team)
  purge(t)
";
        let (p, _, _) = program(src);
        assert_eq!(paths(&p, &list(&p)[0]), ["purge"]);
    }

    #[test]
    fn names_in_call_rules_conditions_and_nested_expressions_count() {
        let src = "\
server fn allowed(t: Team) -> Bool as system
  call if me in t.owners
  return true

server fn viaRule(t: Team)
  call if me in t.owners and allowed(t)
  t.name = \"a\"

server fn viaLoop(t: Team)
  call if me in t.owners
  for n in Note where team == t
    if n.text == \"\"
      let ok = [allowed(t)]
";
        let (p, _, _) = program(src);
        assert_eq!(
            paths(&p, &list(&p)[0]),
            ["allowed", "viaRule -> allowed", "viaLoop -> allowed"]
        );
    }

    #[test]
    fn cycles_end_and_every_elevated_function_is_listed() {
        let src = "\
server fn a(t: Team) as system
  call if me in t.owners
  b(t)

server fn b(t: Team) -> List[Note] as system
  call if me in t.owners
  a(t)
  return b(t)
";
        let (p, _, map) = program(src);
        let all = list(&p);
        let listed: Vec<_> = all.iter().map(|e| (&*e.name, paths(&p, e))).collect();
        assert_eq!(
            listed,
            [
                ("a", vec!["a".to_string(), "b -> a".to_string()]),
                ("b", vec!["a -> b".to_string(), "b".to_string()]),
            ]
        );
        let warns = rendered(&map, &p);
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("`b` returns rows without see"),
            "{}",
            warns[0]
        );
    }
}
