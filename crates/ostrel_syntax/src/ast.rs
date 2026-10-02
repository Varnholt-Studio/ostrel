//! Syntax tree of the v0.1 language slice (SYNTAX 4.11, ARCHITECTURE 3.4).
//!
//! The tree covers `fn` declarations with typed parameters and an optional
//! result type, the statements `let`, `if`/`else`, `return` and expression
//! statements, and the expressions of the v0.1 subset: integer, boolean and
//! string literals with interpolation, names, unary and binary operators,
//! calls and parentheses. It grows by addition; the node enums are
//! `#[non_exhaustive]`, so passes in other crates keep a fallback arm that
//! reports `E0100 not available in v0.1` for constructs they do not know yet.
//!
//! # Storage
//!
//! All nodes of one file live in a [`Module`]. Expressions, statements and
//! blocks are stored in arenas and referenced by `u32` ids ([`ExprId`],
//! [`StmtId`], [`BlockId`]). The arena enforces three invariants when a node
//! is added, so every later pass can rely on them:
//!
//! * The structure is a tree: a child id must already exist in this module
//!   and may be attached to exactly one parent. Children are always added
//!   before their parent, so the tree has no cycles. Ids are not tied to
//!   their module: an id from another module is only detected when its index
//!   does not exist here, so never mix ids of two modules.
//! * The number of nodes stays at or below [`Limits::max_nodes`].
//! * The height stays at or below [`Limits::max_height`], so recursive passes
//!   over the tree are bounded (D54).
//!
//! # Counting
//!
//! Height is the number of nodes on the longest path from the module root to
//! a leaf; a leaf has height 1. The module root itself is part of that path.
//! The node count includes the module root, every item, parameter, type
//! reference, block, statement and expression. Tokens and trivia never count.
//!
//! # Ranges
//!
//! Nodes carry a [`TextRange`] of byte offsets into the source text of their
//! file. The file itself is known by the caller that owns the module.
//!
//! # Constructors for tests
//!
//! Besides the general `add_*` methods used by the parser, [`Module`] offers
//! short constructors such as [`Module::int`] or [`Module::binary`]. They give
//! every node an empty range at offset 0 and let other teams build trees
//! without the parser.

use std::fmt;

/// Default upper bound for the number of nodes in one file (D54).
pub const MAX_NODES: u32 = 1_000_000;

/// Default upper bound for the height of the syntax tree of one file (D54).
pub const MAX_HEIGHT: u32 = 2_048;

/// Limits a [`Module`] enforces while nodes are added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Highest allowed number of nodes, module root included.
    pub max_nodes: u32,
    /// Highest allowed tree height, module root included.
    pub max_height: u32,
}

impl Limits {
    /// The limits of D54: [`MAX_NODES`] and [`MAX_HEIGHT`].
    pub const DEFAULT: Limits = Limits {
        max_nodes: MAX_NODES,
        max_height: MAX_HEIGHT,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

/// Why a node could not be added to a [`Module`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AstError {
    /// The node would raise the node count above [`Limits::max_nodes`].
    NodeLimit,
    /// The node would raise the tree height above [`Limits::max_height`].
    HeightLimit,
    /// A child id does not belong to this module.
    UnknownId,
    /// A child is already attached to another parent.
    AlreadyAttached,
}

impl fmt::Display for AstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            AstError::NodeLimit => "syntax tree node limit exceeded",
            AstError::HeightLimit => "syntax tree height limit exceeded",
            AstError::UnknownId => "node id does not belong to this module",
            AstError::AlreadyAttached => "node is already attached to a parent",
        };
        f.write_str(text)
    }
}

impl std::error::Error for AstError {}

/// A half open range `start..end` of byte offsets into one source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    /// Creates the range `start..end`. Returns `None` if `end < start`.
    pub fn new(start: u32, end: u32) -> Option<TextRange> {
        (start <= end).then_some(TextRange { start, end })
    }

    /// Creates an empty range at `offset`.
    pub fn empty(offset: u32) -> TextRange {
        TextRange {
            start: offset,
            end: offset,
        }
    }

    /// First byte offset of the range.
    pub fn start(self) -> u32 {
        self.start
    }

    /// Byte offset just after the range.
    pub fn end(self) -> u32 {
        self.end
    }

    /// Length in bytes.
    pub fn len(self) -> u32 {
        self.end - self.start
    }

    /// True if the range covers no bytes.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The smallest range that covers both `self` and `other`.
    pub fn cover(self, other: TextRange) -> TextRange {
        TextRange {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            /// Position in the arena, usable as an index into side tables.
            pub fn index(self) -> usize {
                self.0 as usize
            }

            /// The raw id.
            pub fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

id_type!(
    /// Id of an [`Expr`] in its [`Module`].
    ExprId
);
id_type!(
    /// Id of a [`Stmt`] in its [`Module`].
    StmtId
);
id_type!(
    /// Id of a [`Block`] in its [`Module`].
    BlockId
);

/// A name as written in the source, for example `fib` or `Int`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    /// The characters of the name.
    pub text: Box<str>,
    /// Where the name stands in the source.
    pub range: TextRange,
}

impl Ident {
    /// Creates a name at `range`.
    pub fn new(text: &str, range: TextRange) -> Ident {
        Ident {
            text: text.into(),
            range,
        }
    }

    /// Creates a name with an empty range at offset 0, for tests.
    pub fn bare(text: &str) -> Ident {
        Ident::new(text, TextRange::default())
    }
}

/// A type as written in the source. In v0.1 a type is a single name such as
/// `Int`, `Text` or `Bool`; whether the name is known is decided by the
/// checker.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeRef {
    /// The type name.
    pub name: Ident,
    /// Where the type stands in the source.
    pub range: TextRange,
}

impl TypeRef {
    /// Creates a type reference whose range is the range of its name.
    pub fn named(name: Ident) -> TypeRef {
        let range = name.range;
        TypeRef { name, range }
    }
}

/// One parameter `name: Type` of a function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Param {
    /// The parameter name.
    pub name: Ident,
    /// The declared type.
    pub ty: TypeRef,
    /// From the start of the name to the end of the type.
    pub range: TextRange,
}

impl Param {
    /// Creates a parameter whose range covers its name and its type.
    pub fn new(name: Ident, ty: TypeRef) -> Param {
        let range = name.range.cover(ty.range);
        Param { name, ty, range }
    }
}

/// A top level declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Item {
    /// `fn name(params) -> Type` with a body. Boxed so that an item stays
    /// within the node size budget of ARCHITECTURE 3.4.
    Fn(Box<FnDecl>),
}

/// A function declaration: `fn name(a: Int, b: Int) -> Int` and its body.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnDecl {
    /// The function name.
    pub name: Ident,
    /// Parameters in source order; may be empty.
    pub params: Box<[Param]>,
    /// The result type; `None` if the function returns nothing.
    pub result: Option<TypeRef>,
    /// The indented body.
    pub body: BlockId,
    /// From the `fn` keyword to the end of the body.
    pub range: TextRange,
}

/// An indented sequence of statements.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Block {
    /// Statements in source order; may be empty (SYNTAX 7 allows a block
    /// without statements, and trees built by hand may have one too).
    pub stmts: Box<[StmtId]>,
    /// From the first to the last statement; for an empty block an empty
    /// range at the position where the block would start.
    pub range: TextRange,
    height: u32,
}

/// A statement with its source range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stmt {
    /// What kind of statement this is.
    pub kind: StmtKind,
    /// From the first keyword or expression to the end of the statement.
    pub range: TextRange,
    height: u32,
}

/// The statements of the v0.1 slice.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StmtKind {
    /// `let name = value`
    Let {
        /// The bound name.
        name: Ident,
        /// The bound value.
        value: ExprId,
    },
    /// `if cond` with a block and an optional `else` block.
    If {
        /// The condition.
        cond: ExprId,
        /// The block run when the condition holds.
        then_block: BlockId,
        /// The block after `else`, if any.
        else_block: Option<BlockId>,
    },
    /// `return value`. The grammar requires a value (SYNTAX 7).
    Return {
        /// The returned value.
        value: ExprId,
    },
    /// An expression on its own line, for example a call.
    Expr(ExprId),
}

/// An expression with its source range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Expr {
    /// What kind of expression this is.
    pub kind: ExprKind,
    /// The source text of the whole expression.
    pub range: TextRange,
    height: u32,
}

/// The expressions of the v0.1 slice.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExprKind {
    /// An integer literal. Literals are written without sign, so the value is
    /// never negative; the lexer rejects values above 2^53 minus 1 (G9).
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
    /// A string literal, split into text and interpolations (G1).
    Str(Box<[StrPart]>),
    /// A use of a name, for example a local or a function.
    Name(Ident),
    /// A prefix operator applied to one operand.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: ExprId,
    },
    /// A binary operator. Chains are left associative: `a - b - c` is
    /// `(a - b) - c`.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// A call `callee(args)`.
    Call {
        /// The called expression; a name in v0.1.
        callee: ExprId,
        /// Arguments in source order; may be empty.
        args: Box<[ExprId]>,
    },
    /// An expression in parentheses. Kept so that the formatter and
    /// diagnostics see what the source says.
    Paren(ExprId),
}

/// One piece of a string literal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StrPart {
    /// Literal text with escapes already resolved; never empty when built by
    /// the parser.
    Text(Box<str>),
    /// An interpolation `{expr}`.
    Interp(ExprId),
}

/// Prefix operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `not x`
    Not,
}

/// Binary operators of the v0.1 slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`, truncates toward zero.
    Div,
    /// `%`, takes the sign of the dividend.
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `and`, short circuit.
    And,
    /// `or`, short circuit.
    Or,
}

impl BinaryOp {
    /// The operator as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "and",
            BinaryOp::Or => "or",
        }
    }
}

impl UnaryOp {
    /// The operator as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "not",
        }
    }
}

/// The syntax tree of one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    items: Vec<Item>,
    exprs: Vec<Expr>,
    stmts: Vec<Stmt>,
    blocks: Vec<Block>,
    expr_attached: Vec<bool>,
    stmt_attached: Vec<bool>,
    block_attached: Vec<bool>,
    nodes: u32,
    height: u32,
    limits: Limits,
}

/// Children marked as attached while a node is being added, so the marks
/// can be cleared if the node is rejected.
#[derive(Default)]
struct Attached {
    exprs: Vec<u32>,
    stmts: Vec<u32>,
    blocks: Vec<u32>,
}

impl Default for Module {
    fn default() -> Self {
        Module::new()
    }
}

impl Module {
    /// An empty module with the default [`Limits`].
    pub fn new() -> Module {
        Module::with_limits(Limits::DEFAULT)
    }

    /// An empty module with the given limits. The empty module already counts
    /// its root: one node, height 1. A `max_nodes` of 0 or a `max_height`
    /// below 2 is accepted but leaves no room for any further node, so every
    /// `add_*` call fails.
    pub fn with_limits(limits: Limits) -> Module {
        Module {
            items: Vec::new(),
            exprs: Vec::new(),
            stmts: Vec::new(),
            blocks: Vec::new(),
            expr_attached: Vec::new(),
            stmt_attached: Vec::new(),
            block_attached: Vec::new(),
            nodes: 1,
            height: 1,
            limits,
        }
    }

    /// The limits this module enforces.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Top level declarations in source order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Number of nodes, module root included.
    pub fn node_count(&self) -> u32 {
        self.nodes
    }

    /// Height of the tree, module root included.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The expression with id `id`, or `None` if the id is not from this
    /// module.
    pub fn expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.index())
    }

    /// The statement with id `id`, or `None` if the id is not from this
    /// module.
    pub fn stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.index())
    }

    /// The block with id `id`, or `None` if the id is not from this module.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }

    /// Height of the subtree rooted at expression `id`.
    pub fn expr_height(&self, id: ExprId) -> Option<u32> {
        self.expr(id).map(|e| e.height)
    }

    /// Height of the subtree rooted at statement `id`.
    pub fn stmt_height(&self, id: StmtId) -> Option<u32> {
        self.stmt(id).map(|s| s.height)
    }

    /// Height of the subtree rooted at block `id`.
    pub fn block_height(&self, id: BlockId) -> Option<u32> {
        self.block(id).map(|b| b.height)
    }

    // Limit and tree checks shared by all `add_*` methods.

    fn reserve_nodes(&self, extra: usize) -> Result<u32, AstError> {
        let extra = u32::try_from(extra).map_err(|_| AstError::NodeLimit)?;
        let total = self.nodes.checked_add(extra).ok_or(AstError::NodeLimit)?;
        if total > self.limits.max_nodes {
            return Err(AstError::NodeLimit);
        }
        Ok(total)
    }

    /// Checks a subtree height. The module root sits above every node, so a
    /// node of height `h` makes the whole tree at least `h + 1` high.
    fn check_height(&self, height: u32) -> Result<(), AstError> {
        match height.checked_add(1) {
            Some(total) if total <= self.limits.max_height => Ok(()),
            _ => Err(AstError::HeightLimit),
        }
    }

    fn next_id(len: usize) -> Result<u32, AstError> {
        u32::try_from(len).map_err(|_| AstError::NodeLimit)
    }

    // Attaching children. Each child is marked as attached as soon as it is
    // checked, so a repeated child is found in O(1) and adding a node is
    // linear in its number of children. If the node is rejected later, the
    // marks recorded in `Attached` are cleared again.

    fn attach_expr(&mut self, id: ExprId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.expr_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .expr_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.exprs.push(id.0);
        Ok(height)
    }

    fn attach_stmt(&mut self, id: StmtId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.stmt_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .stmt_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.stmts.push(id.0);
        Ok(height)
    }

    fn attach_block(&mut self, id: BlockId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.block_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .block_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.blocks.push(id.0);
        Ok(height)
    }

    /// Clears the marks of a rejected node, so its children stay free.
    fn detach(&mut self, att: &Attached) {
        let tables = [
            (&mut self.expr_attached, &att.exprs),
            (&mut self.stmt_attached, &att.stmts),
            (&mut self.block_attached, &att.blocks),
        ];
        for (flags, ids) in tables {
            for id in ids {
                if let Some(flag) = flags.get_mut(*id as usize) {
                    *flag = false;
                }
            }
        }
    }

    /// Runs `add` and clears the attach marks it set if it fails.
    fn transact<T>(
        &mut self,
        add: impl FnOnce(&mut Module, &mut Attached) -> Result<T, AstError>,
    ) -> Result<T, AstError> {
        let mut att = Attached::default();
        let result = add(self, &mut att);
        if result.is_err() {
            self.detach(&att);
        }
        result
    }

    /// Adds an expression and returns its id. All child ids must come from
    /// this module and must not have a parent yet. On error the module is
    /// unchanged.
    pub fn add_expr(&mut self, kind: ExprKind, range: TextRange) -> Result<ExprId, AstError> {
        self.transact(|m, att| {
            let child_height = match &kind {
                ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Name(_) => 0,
                ExprKind::Str(parts) => {
                    let mut max = 0;
                    for part in parts.iter() {
                        if let StrPart::Interp(id) = part {
                            max = max.max(m.attach_expr(*id, att)?);
                        }
                    }
                    max
                }
                ExprKind::Unary { operand, .. } => m.attach_expr(*operand, att)?,
                ExprKind::Binary { lhs, rhs, .. } => {
                    let l = m.attach_expr(*lhs, att)?;
                    let r = m.attach_expr(*rhs, att)?;
                    l.max(r)
                }
                ExprKind::Call { callee, args } => {
                    let mut max = m.attach_expr(*callee, att)?;
                    for arg in args.iter() {
                        max = max.max(m.attach_expr(*arg, att)?);
                    }
                    max
                }
                ExprKind::Paren(inner) => m.attach_expr(*inner, att)?,
            };
            let height = child_height.checked_add(1).ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.exprs.len())?;
            m.exprs.push(Expr {
                kind,
                range,
                height,
            });
            m.expr_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(ExprId(id))
        })
    }

    /// Adds a statement and returns its id. All child ids must come from
    /// this module and must not have a parent yet. On error the module is
    /// unchanged.
    pub fn add_stmt(&mut self, kind: StmtKind, range: TextRange) -> Result<StmtId, AstError> {
        self.transact(|m, att| {
            let child_height = match &kind {
                StmtKind::Let { value, .. } => m.attach_expr(*value, att)?,
                StmtKind::Return { value } => m.attach_expr(*value, att)?,
                StmtKind::Expr(value) => m.attach_expr(*value, att)?,
                StmtKind::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    let mut max = m.attach_expr(*cond, att)?;
                    max = max.max(m.attach_block(*then_block, att)?);
                    if let Some(else_block) = else_block {
                        max = max.max(m.attach_block(*else_block, att)?);
                    }
                    max
                }
            };
            let height = child_height.checked_add(1).ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.stmts.len())?;
            m.stmts.push(Stmt {
                kind,
                range,
                height,
            });
            m.stmt_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(StmtId(id))
        })
    }

    /// Adds a block of statements and returns its id. An empty block has
    /// height 1. On error the module is unchanged.
    pub fn add_block(&mut self, stmts: Vec<StmtId>, range: TextRange) -> Result<BlockId, AstError> {
        self.transact(|m, att| {
            let mut max = 0;
            for stmt in &stmts {
                max = max.max(m.attach_stmt(*stmt, att)?);
            }
            let height = max.checked_add(1).ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.blocks.len())?;
            m.blocks.push(Block {
                stmts: stmts.into_boxed_slice(),
                range,
                height,
            });
            m.block_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(BlockId(id))
        })
    }

    /// Appends a function declaration as the next top level item. Counts one
    /// node for the item, two per parameter (the parameter and its type) and
    /// one for the result type. On error the module is unchanged.
    pub fn add_fn(&mut self, decl: FnDecl) -> Result<(), AstError> {
        self.transact(|m, att| {
            let body = m.attach_block(decl.body, att)?;
            // A parameter has its type below it, so it is two levels high.
            let params_height = if decl.params.is_empty() { 0 } else { 2 };
            let result_height = u32::from(decl.result.is_some());
            let height = body
                .max(params_height)
                .max(result_height)
                .checked_add(1)
                .ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let extra = decl
                .params
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(1 + usize::from(decl.result.is_some())))
                .ok_or(AstError::NodeLimit)?;
            let nodes = m.reserve_nodes(extra)?;
            m.items.push(Item::Fn(Box::new(decl)));
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(())
        })
    }

    // Constructors for tests and tools. Every node gets an empty range at
    // offset 0.

    /// Adds an integer literal.
    pub fn int(&mut self, value: i64) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Int(value), TextRange::default())
    }

    /// Adds `true` or `false`.
    pub fn bool(&mut self, value: bool) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Bool(value), TextRange::default())
    }

    /// Adds a string literal without interpolation.
    pub fn text(&mut self, value: &str) -> Result<ExprId, AstError> {
        let parts: Box<[StrPart]> = if value.is_empty() {
            Box::new([])
        } else {
            Box::new([StrPart::Text(value.into())])
        };
        self.add_expr(ExprKind::Str(parts), TextRange::default())
    }

    /// Adds a string literal made of the given parts.
    pub fn string(&mut self, parts: Vec<StrPart>) -> Result<ExprId, AstError> {
        self.add_expr(
            ExprKind::Str(parts.into_boxed_slice()),
            TextRange::default(),
        )
    }

    /// Adds a use of the name `name`.
    pub fn name(&mut self, name: &str) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Name(Ident::bare(name)), TextRange::default())
    }

    /// Adds a prefix operator expression.
    pub fn unary(&mut self, op: UnaryOp, operand: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Unary { op, operand }, TextRange::default())
    }

    /// Adds a binary operator expression.
    pub fn binary(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Binary { op, lhs, rhs }, TextRange::default())
    }

    /// Adds a call of the function named `callee`.
    pub fn call(&mut self, callee: &str, args: Vec<ExprId>) -> Result<ExprId, AstError> {
        let callee = self.name(callee)?;
        self.add_expr(
            ExprKind::Call {
                callee,
                args: args.into_boxed_slice(),
            },
            TextRange::default(),
        )
    }

    /// Adds `(inner)`.
    pub fn paren(&mut self, inner: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Paren(inner), TextRange::default())
    }

    /// Adds `let name = value`.
    pub fn let_stmt(&mut self, name: &str, value: ExprId) -> Result<StmtId, AstError> {
        let kind = StmtKind::Let {
            name: Ident::bare(name),
            value,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds `return value`.
    pub fn return_stmt(&mut self, value: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Return { value }, TextRange::default())
    }

    /// Adds an expression statement.
    pub fn expr_stmt(&mut self, value: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Expr(value), TextRange::default())
    }

    /// Adds `if cond` with its blocks.
    pub fn if_stmt(
        &mut self,
        cond: ExprId,
        then_block: BlockId,
        else_block: Option<BlockId>,
    ) -> Result<StmtId, AstError> {
        let kind = StmtKind::If {
            cond,
            then_block,
            else_block,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds a block of statements.
    pub fn block_of(&mut self, stmts: Vec<StmtId>) -> Result<BlockId, AstError> {
        self.add_block(stmts, TextRange::default())
    }

    /// Appends `fn name(params) -> result` with the given body. Parameters
    /// are pairs of name and type name, for example `("n", "Int")`.
    pub fn fn_item(
        &mut self,
        name: &str,
        params: &[(&str, &str)],
        result: Option<&str>,
        body: BlockId,
    ) -> Result<(), AstError> {
        let params = params
            .iter()
            .map(|(name, ty)| Param::new(Ident::bare(name), TypeRef::named(Ident::bare(ty))))
            .collect();
        self.add_fn(FnDecl {
            name: Ident::bare(name),
            params,
            result: result.map(|ty| TypeRef::named(Ident::bare(ty))),
            body,
            range: TextRange::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = Result<(), AstError>;

    #[test]
    fn node_sizes_stay_within_d54_budget() {
        assert!(std::mem::size_of::<Expr>() <= 64);
        assert!(std::mem::size_of::<Stmt>() <= 64);
        assert!(std::mem::size_of::<Block>() <= 64);
        assert!(std::mem::size_of::<Item>() <= 64);
        assert!(std::mem::size_of::<Param>() <= 64);
        assert!(std::mem::size_of::<TypeRef>() <= 64);
    }

    #[test]
    fn empty_module_counts_its_root() {
        let m = Module::new();
        assert_eq!(m.node_count(), 1);
        assert_eq!(m.height(), 1);
        assert!(m.items().is_empty());
        assert_eq!(m.limits(), Limits::DEFAULT);
    }

    /// Builds the `fib` program of SYNTAX 4.11 with the test constructors.
    #[test]
    fn builds_fib_example() -> R {
        let mut m = Module::new();
        // fn fib(n: Int) -> Int
        let n = m.name("n")?;
        let two = m.int(2)?;
        let cond = m.binary(BinaryOp::Lt, n, two)?;
        let n2 = m.name("n")?;
        let ret_n = m.return_stmt(n2)?;
        let then_block = m.block_of(vec![ret_n])?;
        let if_stmt = m.if_stmt(cond, then_block, None)?;
        let n3 = m.name("n")?;
        let one = m.int(1)?;
        let sub1 = m.binary(BinaryOp::Sub, n3, one)?;
        let call1 = m.call("fib", vec![sub1])?;
        let n4 = m.name("n")?;
        let two2 = m.int(2)?;
        let sub2 = m.binary(BinaryOp::Sub, n4, two2)?;
        let call2 = m.call("fib", vec![sub2])?;
        let sum = m.binary(BinaryOp::Add, call1, call2)?;
        let ret = m.return_stmt(sum)?;
        let body = m.block_of(vec![if_stmt, ret])?;
        m.fn_item("fib", &[("n", "Int")], Some("Int"), body)?;

        // fn main()
        let twenty = m.int(20)?;
        let fib20 = m.call("fib", vec![twenty])?;
        let let_x = m.let_stmt("x", fib20)?;
        let x = m.name("x")?;
        let s = m.string(vec![StrPart::Text("fib(20) = ".into()), StrPart::Interp(x)])?;
        let print = m.call("print", vec![s])?;
        let print_stmt = m.expr_stmt(print)?;
        let main_body = m.block_of(vec![let_x, print_stmt])?;
        m.fn_item("main", &[], None, main_body)?;

        assert_eq!(m.items().len(), 2);
        let Some(Item::Fn(fib)) = m.items().first() else {
            return Err(AstError::UnknownId);
        };
        assert_eq!(&*fib.name.text, "fib");
        assert_eq!(fib.params.len(), 1);
        assert_eq!(fib.result.as_ref().map(|t| &*t.name.text), Some("Int"));
        let body = m.block(fib.body).ok_or(AstError::UnknownId)?;
        assert_eq!(body.stmts.len(), 2);
        // module > fn > block > return > add > call > sub > name
        assert_eq!(m.height(), 8);
        Ok(())
    }

    #[test]
    fn heights_follow_children() -> R {
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(m.expr_height(a), Some(1));
        let b = m.int(2)?;
        let sum = m.binary(BinaryOp::Add, a, b)?;
        assert_eq!(m.expr_height(sum), Some(2));
        let p = m.paren(sum)?;
        let neg = m.unary(UnaryOp::Neg, p)?;
        assert_eq!(m.expr_height(neg), Some(4));
        let stmt = m.expr_stmt(neg)?;
        assert_eq!(m.stmt_height(stmt), Some(5));
        let empty = m.block_of(vec![])?;
        assert_eq!(m.block_height(empty), Some(1));
        assert_eq!(m.height(), 6);
        Ok(())
    }

    #[test]
    fn empty_string_has_no_parts() -> R {
        let mut m = Module::new();
        let s = m.text("")?;
        let expr = m.expr(s).ok_or(AstError::UnknownId)?;
        assert_eq!(expr.kind, ExprKind::Str(Box::new([])));
        assert_eq!(m.expr_height(s), Some(1));
        Ok(())
    }

    #[test]
    fn node_limit_is_exact() -> R {
        // Root plus three nodes fit; the fourth node is rejected.
        let mut m = Module::with_limits(Limits {
            max_nodes: 4,
            max_height: MAX_HEIGHT,
        });
        m.int(1)?;
        m.int(2)?;
        m.int(3)?;
        assert_eq!(m.node_count(), 4);
        assert_eq!(m.int(4), Err(AstError::NodeLimit));
        assert_eq!(m.node_count(), 4);
        Ok(())
    }

    #[test]
    fn node_limit_counts_params_and_types() -> R {
        // Root, block, then fn (1) + param (2) + result (1) = 6 nodes.
        let mut m = Module::with_limits(Limits {
            max_nodes: 5,
            max_height: MAX_HEIGHT,
        });
        let body = m.block_of(vec![])?;
        assert_eq!(
            m.fn_item("f", &[("a", "Int")], Some("Int"), body),
            Err(AstError::NodeLimit)
        );
        assert!(m.items().is_empty());
        // The rejected item did not attach its body; a smaller fn may use it.
        m.fn_item("f", &[], None, body)?;
        assert_eq!(m.node_count(), 3);
        Ok(())
    }

    #[test]
    fn zero_node_limit_rejects_everything() {
        let mut m = Module::with_limits(Limits {
            max_nodes: 0,
            max_height: MAX_HEIGHT,
        });
        assert_eq!(m.int(1), Err(AstError::NodeLimit));
    }

    /// Builds `1 + 1 + ... ` with `operands` operands as a left leaning chain.
    fn chain(m: &mut Module, operands: u32) -> Result<ExprId, AstError> {
        let mut acc = m.int(1)?;
        for _ in 1..operands {
            let rhs = m.int(1)?;
            acc = m.binary(BinaryOp::Add, acc, rhs)?;
        }
        Ok(acc)
    }

    #[test]
    fn height_limit_is_exact() -> R {
        // With limit 10 the root leaves room for an expression of height 9.
        let limits = Limits {
            max_nodes: MAX_NODES,
            max_height: 10,
        };
        let mut m = Module::with_limits(limits);
        let e = chain(&mut m, 9)?;
        assert_eq!(m.expr_height(e), Some(9));
        assert_eq!(m.height(), 10);

        let mut m = Module::with_limits(limits);
        assert_eq!(chain(&mut m, 10), Err(AstError::HeightLimit));
        Ok(())
    }

    #[test]
    fn d54_chain_cases_with_default_limits() -> R {
        // chain_below: 2 000 operands in `let x = ...` inside fn main.
        let mut m = Module::new();
        let e = chain(&mut m, 2_000)?;
        let s = m.let_stmt("x", e)?;
        let b = m.block_of(vec![s])?;
        m.fn_item("main", &[], None, b)?;
        assert_eq!(m.height(), 2_004);

        // chain_above: 2 100 operands never fit.
        let mut m = Module::new();
        assert_eq!(chain(&mut m, 2_100), Err(AstError::HeightLimit));
        Ok(())
    }

    #[test]
    fn rejects_foreign_ids() -> R {
        let mut other = Module::new();
        other.int(1)?;
        let foreign = other.int(2)?;
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(
            m.binary(BinaryOp::Add, a, foreign),
            Err(AstError::UnknownId)
        );
        // The failed call must not attach `a`.
        let b = m.int(3)?;
        m.binary(BinaryOp::Add, a, b)?;
        Ok(())
    }

    #[test]
    fn rejects_shared_children() -> R {
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(
            m.binary(BinaryOp::Add, a, a),
            Err(AstError::AlreadyAttached)
        );
        let b = m.int(2)?;
        m.binary(BinaryOp::Add, a, b)?;
        assert_eq!(m.unary(UnaryOp::Neg, a), Err(AstError::AlreadyAttached));

        let s = m.expr_stmt(b).err();
        assert_eq!(s, Some(AstError::AlreadyAttached));

        let c = m.int(3)?;
        let stmt = m.expr_stmt(c)?;
        m.block_of(vec![stmt])?;
        assert_eq!(m.block_of(vec![stmt]), Err(AstError::AlreadyAttached));

        let blk = m.block_of(vec![])?;
        let cond = m.bool(true)?;
        assert_eq!(
            m.if_stmt(cond, blk, Some(blk)),
            Err(AstError::AlreadyAttached)
        );
        m.fn_item("f", &[], None, blk)?;
        assert_eq!(
            m.fn_item("g", &[], None, blk),
            Err(AstError::AlreadyAttached)
        );
        Ok(())
    }

    #[test]
    fn string_interpolations_must_be_distinct() -> R {
        let mut m = Module::new();
        let x = m.name("x")?;
        assert_eq!(
            m.string(vec![StrPart::Interp(x), StrPart::Interp(x)]),
            Err(AstError::AlreadyAttached)
        );
        let s = m.string(vec![StrPart::Text("a".into()), StrPart::Interp(x)])?;
        assert_eq!(m.expr_height(s), Some(2));
        Ok(())
    }

    /// Builds a `main` whose body has `wide` statements and ends in a call
    /// with `wide` arguments, and returns the module.
    fn wide_module(wide: usize) -> Result<Module, AstError> {
        let mut m = Module::new();
        let args = (0..wide).map(|_| m.int(1)).collect::<Result<Vec<_>, _>>()?;
        let call = m.call("f", args)?;
        let stmts = (0..wide)
            .map(|i| {
                let e = m.int(i as i64)?;
                m.expr_stmt(e)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let last = m.expr_stmt(call)?;
        let mut all = stmts;
        all.push(last);
        let body = m.block_of(all)?;
        m.fn_item("main", &[], None, body)?;
        Ok(m)
    }

    #[test]
    fn wide_call_and_block_count_their_nodes() -> R {
        const WIDE: usize = 200_000;
        let m = wide_module(WIDE)?;
        assert_eq!(
            m.node_count(),
            1 + 2 * WIDE as u32 + 2 + WIDE as u32 + 1 + 1 + 1
        );
        Ok(())
    }

    /// Wide nodes must be linear in their number of children (AC-04). With
    /// the old quadratic check 200 000 children took several seconds. The
    /// check compares two sizes instead of using a time limit, so it holds
    /// under any machine load (D74).
    #[test]
    fn wide_call_and_block_are_linear() {
        crate::test_support::assert_linear(8_000, |n| n, |&n| wide_module(n).ok());
    }

    #[test]
    fn rejected_wide_node_releases_all_children() -> R {
        let mut m = Module::new();
        let args = (0..1_000)
            .map(|_| m.int(1))
            .collect::<Result<Vec<_>, _>>()?;
        let mut dup = args.clone();
        dup.extend(args.first().copied());
        assert_eq!(m.call("f", dup), Err(AstError::AlreadyAttached));
        // Every argument is free again and can be used once.
        m.call("f", args)?;

        // A rejection after all children were marked also rolls back.
        let mut small = Module::with_limits(Limits {
            max_nodes: 3,
            max_height: MAX_HEIGHT,
        });
        let a = small.int(1)?;
        let b = small.int(2)?;
        assert_eq!(small.binary(BinaryOp::Add, a, b), Err(AstError::NodeLimit));
        let s = small.add_stmt(StmtKind::Expr(a), TextRange::default());
        assert_eq!(s, Err(AstError::NodeLimit));
        let mut wide = Module::with_limits(Limits {
            max_nodes: MAX_NODES,
            max_height: 3,
        });
        let x = wide.int(1)?;
        let neg = wide.unary(UnaryOp::Neg, x)?;
        assert_eq!(wide.expr_stmt(neg), Err(AstError::HeightLimit));
        // `neg` was released by the rejected statement.
        assert_eq!(wide.paren(neg), Err(AstError::HeightLimit));
        assert_eq!(
            wide.add_block(vec![], TextRange::default()).map(|_| ()),
            Ok(())
        );
        Ok(())
    }

    #[test]
    fn text_range_rules() {
        assert_eq!(TextRange::new(3, 2), None);
        let r = TextRange::new(2, 5);
        assert_eq!(r.map(TextRange::len), Some(3));
        assert!(TextRange::empty(7).is_empty());
        let a = TextRange::empty(10);
        let b = TextRange::new(2, 4).unwrap_or_default();
        let c = a.cover(b);
        assert_eq!((c.start(), c.end()), (2, 10));
    }

    #[test]
    fn operator_spelling() {
        assert_eq!(BinaryOp::Rem.as_str(), "%");
        assert_eq!(BinaryOp::Or.as_str(), "or");
        assert_eq!(UnaryOp::Not.as_str(), "not");
    }
}
