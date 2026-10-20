use super::*;

fn file() -> FileId {
    FileId::from_raw(3)
}

fn model_program() -> (Program, DefId) {
    let span = Span::point(file(), 0);
    let room = DefId(0);
    let field = |name: &str, ty: Ty| FieldDef {
        name: name.into(),
        span,
        ty,
        default: None,
        mode: FieldMode::Plain,
        merge: Merge::Default,
    };
    let program = Program {
        defs: vec![Def {
            name: "Room".into(),
            span,
            kind: DefKind::Model(ModelDef {
                fields: vec![
                    field("name", Ty::Text),
                    field("members", Ty::Set(Box::new(Ty::Ref(room)))),
                ],
                rules: Vec::new(),
                checks: Vec::new(),
            }),
        }],
        ..Program::default()
    };
    (program, room)
}

#[test]
fn resolve_of_no_files_is_an_empty_program() {
    let (program, diagnostics) = resolve(&[]);
    assert_eq!(program, Program::default());
    assert!(diagnostics.is_empty());
}

#[test]
fn resolve_stub_reports_one_error_in_the_first_non_empty_file() {
    let empty = ast::Module::new();
    let mut module = ast::Module::new();
    let body = module.block_of(Vec::new()).unwrap();
    module.fn_item("main", &[], None, body).unwrap();
    let other = FileId::from_raw(1);
    let sources = [
        Source {
            file: other,
            text: "",
            module: &empty,
        },
        Source {
            file: file(),
            text: "fn main()\n",
            module: &module,
        },
        Source {
            file: file(),
            text: "fn main()\n",
            module: &module,
        },
    ];
    let (program, diagnostics) = resolve(&sources);
    assert_eq!(program, Program::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].is_error());
    assert_eq!(diagnostics[0].code, crate::codes::NOT_IN_V0_1);
    assert_eq!(diagnostics[0].span, Span::point(file(), 0));
}

#[test]
fn check_stub_reports_nothing_and_changes_nothing() {
    let (mut program, _) = model_program();
    let before = program.clone();
    assert!(check(&mut program).is_empty());
    assert_eq!(program, before);
}

#[test]
fn accessors_return_none_for_unknown_ids() {
    let (program, room) = model_program();
    assert_eq!(program.def(room).map(|d| &*d.name), Some("Room"));
    assert!(program.def(DefId(1)).is_none());
    assert!(program.local(LocalId(0)).is_none());
    assert!(program.expr(ExprId(0)).is_none());
    assert!(program.stmt(StmtId(0)).is_none());
    assert!(program.block(BlockId(0)).is_none());
    assert!(program.view_node(ViewNodeId(0)).is_none());
}

#[test]
fn field_finds_model_fields_only() {
    let (mut program, room) = model_program();
    assert_eq!(
        program.field(room, FieldId(1)).map(|f| &f.ty),
        Some(&Ty::Set(Box::new(Ty::Ref(room))))
    );
    assert!(program.field(room, FieldId(2)).is_none());
    program.defs.push(Def {
        name: "Status".into(),
        span: Span::point(file(), 0),
        kind: DefKind::Enum(vec!["open".into()].into()),
    });
    assert!(program.field(DefId(1), FieldId(0)).is_none());
    assert!(program.field(DefId(9), FieldId(0)).is_none());
}

#[test]
fn field_types_are_the_rows_of_the_type_table() {
    let model = DefId(0);
    let boxed = Box::new;
    for ty in [
        Ty::Bool,
        Ty::Int,
        Ty::Float,
        Ty::Text,
        Ty::Time,
        Ty::Bytes,
        Ty::Rank,
        Ty::Enum(model),
        Ty::Ref(model),
        Ty::Optional(boxed(Ty::Int)),
        Ty::Set(boxed(Ty::Ref(model))),
        Ty::Map(boxed(Ty::Ref(model)), boxed(Ty::Enum(model))),
        Ty::List(boxed(Ty::Text)),
    ] {
        assert!(ty.is_field_type(), "{ty:?}");
    }
    for ty in [
        Ty::Remote(boxed(Ty::Int)),
        Ty::Rows(model),
        Ty::Range(boxed(Ty::Int)),
        Ty::Unit,
        Ty::Error,
        Ty::Set(boxed(Ty::Unit)),
        Ty::Map(boxed(Ty::Int), boxed(Ty::Rows(model))),
        Ty::Optional(boxed(Ty::Optional(boxed(Ty::Int)))),
    ] {
        assert!(!ty.is_field_type(), "{ty:?}");
    }
}

#[test]
fn sort_by_an_implicit_field_is_representable() {
    // `Message where room == r sort made last 200` (SYNTAX 4.5).
    let span = Span::point(file(), 0);
    let message = DefId(0);
    let mut program = Program::default();
    let expr = |program: &mut Program, kind| {
        program.exprs.push(Expr {
            kind,
            span,
            ty: None,
        });
        ExprId(program.exprs.len() as u32 - 1)
    };
    let room = expr(
        &mut program,
        ExprKind::Name(Res::Field(message, FieldRef::Declared(FieldId(0)))),
    );
    let r = expr(&mut program, ExprKind::Name(Res::Local(LocalId(0))));
    let filter = expr(
        &mut program,
        ExprKind::Binary {
            op: ast::BinaryOp::Eq,
            lhs: room,
            rhs: r,
        },
    );
    let n = expr(&mut program, ExprKind::Int(200));
    let query = Query {
        model: message,
        filter: Some(filter),
        sort: Some((FieldRef::Implicit(ImplicitField::Made), false)),
        take: Some((ast::TakeKind::Last, n)),
    };
    assert_eq!(
        query.sort,
        Some((FieldRef::Implicit(ImplicitField::Made), false))
    );
    // `m.author` on a loop variable `m`.
    let m = expr(&mut program, ExprKind::Name(Res::Local(LocalId(1))));
    let author = ExprKind::Field {
        base: m,
        field: (message, FieldRef::Implicit(ImplicitField::Author)),
    };
    assert!(matches!(
        author,
        ExprKind::Field {
            field: (_, FieldRef::Implicit(ImplicitField::Author)),
            ..
        }
    ));
}
