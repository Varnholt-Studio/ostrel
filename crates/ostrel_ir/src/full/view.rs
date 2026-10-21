//! Views: the client UI tree (SYNTAX 4.8, G16), input of the browser backend.
//!
//! A view keeps its tree shape; every expression in it is a client or shared
//! function of the program (a [`Thunk`]) that the browser runtime calls and
//! calls again when its inputs change. Values a node needs from the view (its
//! parameters and the bindings of enclosing `for` and `if let` nodes) are
//! [`Slot`]s, passed to the thunk as arguments.

use super::query::QueryPlan;
use super::{FuncId, ViewId};

/// A value in scope inside a view: parameter `i` of the view for
/// `i < params`, after them the bindings of `for` and `if let` nodes in the
/// order of [`View::slots`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot(pub u32);

/// A function of the program applied to slots of the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thunk {
    /// The function; its parameters are the values of `args`, in order.
    pub func: FuncId,
    /// The slots passed to it.
    pub args: Vec<Slot>,
}

/// A view of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The view name.
    pub name: String,
    /// The number of parameters; they are the first slots.
    pub params: u32,
    /// The type of every slot, parameters first.
    pub slots: Vec<super::Ty>,
    /// The top level nodes in source order.
    pub nodes: Vec<ViewNode>,
    /// The declaration.
    pub span: ostrel_core::Span,
}

/// One node of a view tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewNode {
    /// What the node is.
    pub kind: ViewNodeKind,
    /// Its source.
    pub span: ostrel_core::Span,
}

/// The nodes of a view.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ViewNodeKind {
    /// A std element.
    Element(Box<Element>),
    /// The use of another view with its arguments.
    Use {
        /// The used view.
        view: ViewId,
        /// One thunk per parameter of the used view.
        args: Vec<Thunk>,
    },
    /// `if cond` (`bind` is `None`, `cond` is `Bool`) or `if let` (`cond` is
    /// an optional and its value is bound to `bind` in `then_nodes`).
    If {
        cond: Thunk,
        bind: Option<Slot>,
        then_nodes: Vec<ViewNode>,
        else_nodes: Vec<ViewNode>,
    },
    /// `for bind in source`; the nodes are shown once per item, and the root
    /// of each item carries `data-pending` and `data-rejected` of a row item
    /// (SYNTAX 4.7).
    For {
        bind: Slot,
        source: ForSource,
        nodes: Vec<ViewNode>,
    },
}

/// What a view `for` iterates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForSource {
    /// A live query: a subscription limited by `see` on the server
    /// (SYNTAX 4.5, ARCHITECTURE 5.4). Its plan has [`super::query::See::Replica`];
    /// a `Local(i)` in the plan (in the filter or as `n`) stands for the slot
    /// `args[i]`.
    Live {
        /// The query.
        plan: QueryPlan,
        /// The slots the plan reads.
        args: Vec<Slot>,
    },
    /// Any other iterable value (`Set`, `List`, `Range`).
    Value(Thunk),
}

/// An element line with attributes, style tags, handlers and children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// The std element name (`row`, `text`, `entry`, ...), validated by the
    /// checker.
    pub name: String,
    /// The value after the name: rendered as text, never as markup, except
    /// in `rawHtml` (D23, G7).
    pub value: Option<Thunk>,
    /// `name: value` attributes in source order.
    pub attrs: Vec<(String, Thunk)>,
    /// `~class` and `~class(cond)` in source order.
    pub styles: Vec<(String, Option<Thunk>)>,
    /// `on event action` in source order.
    pub handlers: Vec<Handler>,
    /// Child nodes.
    pub children: Vec<ViewNode>,
}

/// `on event action`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    /// The event name.
    pub event: String,
    /// The client function that runs the action; its parameters are `args`
    /// and then, for events that carry one, the value `it`.
    pub action: Thunk,
    /// The source of the handler.
    pub span: ostrel_core::Span,
}
