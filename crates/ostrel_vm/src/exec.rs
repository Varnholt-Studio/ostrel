//! The interpreter: runs compiled bytecode with frames on the heap.
//!
//! Invariants established by the compiler (see `bytecode`) and relied on here:
//! every register holds a value of the type declared for its local, every register
//! number and jump target is in range, and every function ends each block with a
//! terminator. The accessors below are total anyway, so malformed state can never
//! cause a panic; it would only produce a wrong value.

use std::cell::Cell;
use std::mem::size_of;
use std::rc::Rc;

use ostrel_ir::{INT_MAX, INT_MIN, Span, Ty};

use crate::bytecode::{Arith, Code, Op, Order};
use crate::{Host, Limits, RuntimeError, RuntimeErrorKind};

/// Heap bytes charged for a `Text` value in addition to its UTF 8 bytes: the shared
/// box with its reference counts.
const TEXT_OVERHEAD: u64 = (size_of::<HeapText>() + 2 * size_of::<usize>()) as u64;

/// Heap bytes charged for a frame in addition to its registers.
const FRAME_OVERHEAD: u64 = size_of::<Frame>() as u64;

/// Heap bytes charged per register.
const VALUE_BYTES: u64 = size_of::<Value>() as u64;

/// Live heap bytes of one run, shared by every value that holds a charge.
#[derive(Clone)]
struct Meter(Rc<Cell<u64>>);

impl Meter {
    /// Charges `bytes`, or fails without charging if the heap would exceed `max`.
    fn charge(&self, bytes: u64, max: u64) -> Result<(), RuntimeErrorKind> {
        match self.0.get().checked_add(bytes) {
            Some(total) if total <= max => {
                self.0.set(total);
                Ok(())
            }
            _ => Err(RuntimeErrorKind::HeapLimit),
        }
    }

    fn release(&self, bytes: u64) {
        self.0.set(self.0.get().saturating_sub(bytes));
    }
}

/// A `Text` value on the VM heap. Dropping the last reference releases its charge.
struct HeapText {
    text: Box<str>,
    charge: u64,
    meter: Meter,
}

impl Drop for HeapText {
    fn drop(&mut self) {
        self.meter.release(self.charge);
    }
}

/// A runtime value. `Text` is shared and immutable, so copies are cheap.
#[derive(Clone)]
enum Value {
    Int(i64),
    Bool(bool),
    Text(Rc<HeapText>),
}

impl Value {
    fn int(&self) -> i64 {
        match self {
            Value::Int(v) => *v,
            _ => 0,
        }
    }

    fn boolean(&self) -> bool {
        match self {
            Value::Bool(v) => *v,
            _ => false,
        }
    }

    fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Text(a), Value::Text(b)) => a.text == b.text,
            _ => false,
        }
    }
}

/// One activation of a function. Frames live in a `Vec`, never on the native stack.
struct Frame {
    func: u32,
    pc: u32,
    regs: Box<[Value]>,
    /// Register of the caller that receives the result.
    ret: Option<u32>,
    /// Heap bytes charged for this frame.
    charge: u64,
}

impl Frame {
    fn reg(&self, index: u32) -> Value {
        self.regs
            .get(index as usize)
            .cloned()
            .unwrap_or(Value::Int(0))
    }

    fn int(&self, index: u32) -> i64 {
        self.regs.get(index as usize).map_or(0, Value::int)
    }

    fn boolean(&self, index: u32) -> bool {
        self.regs.get(index as usize).is_some_and(Value::boolean)
    }

    fn set(&mut self, index: u32, value: Value) {
        if let Some(slot) = self.regs.get_mut(index as usize) {
            *slot = value;
        }
    }
}

/// Number of bytes of the decimal text form of `v`.
fn int_text_len(v: i64) -> u64 {
    let mut len = u64::from(v < 0);
    let mut rest = v.unsigned_abs();
    loop {
        len += 1;
        rest /= 10;
        if rest == 0 {
            return len;
        }
    }
}

struct Vm<'a, H: Host + ?Sized> {
    code: &'a Code,
    host: &'a mut H,
    limits: Limits,
    meter: Meter,
    steps: u64,
    /// Text constants of the image, allocated on the VM heap at their first load.
    texts: Vec<Option<Value>>,
    /// The empty text, initial value of `Text` registers.
    empty: Value,
    /// Callers of the current frame, innermost last.
    stack: Vec<Frame>,
}

pub(crate) fn run<H: Host + ?Sized>(
    code: &Code,
    host: &mut H,
    limits: Limits,
) -> Result<(), RuntimeError> {
    let Some(main) = code.main else {
        return Ok(());
    };
    let meter = Meter(Rc::new(Cell::new(0)));
    let empty = Value::Text(Rc::new(HeapText {
        text: Box::from(""),
        charge: 0,
        meter: meter.clone(),
    }));
    let mut vm = Vm {
        code,
        host,
        limits,
        meter,
        steps: 0,
        texts: vec![None; code.texts.len()],
        empty,
        stack: Vec::new(),
    };
    let main_span = code
        .funcs
        .get(main as usize)
        .map_or(Span::default(), |f| f.span);
    let frame = vm.new_frame(main, &[], None, 0, main_span)?;
    vm.execute(frame)
}

impl<H: Host + ?Sized> Vm<'_, H> {
    /// Allocates a `Text` of exactly `len` bytes filled by `fill`, after checking
    /// `TextLimit` and `HeapLimit`.
    fn alloc_text(
        &self,
        len: u64,
        span: Span,
        fill: impl FnOnce(&mut String),
    ) -> Result<Value, RuntimeError> {
        if len > self.limits.max_text_bytes {
            return Err(RuntimeError {
                kind: RuntimeErrorKind::TextLimit,
                span,
            });
        }
        let charge = len.saturating_add(TEXT_OVERHEAD);
        self.meter
            .charge(charge, self.limits.max_heap_bytes)
            .map_err(|kind| RuntimeError { kind, span })?;
        let Ok(capacity) = usize::try_from(len) else {
            self.meter.release(charge);
            return Err(RuntimeError {
                kind: RuntimeErrorKind::HeapLimit,
                span,
            });
        };
        let mut buf = String::with_capacity(capacity);
        fill(&mut buf);
        Ok(Value::Text(Rc::new(HeapText {
            text: buf.into_boxed_str(),
            charge,
            meter: self.meter.clone(),
        })))
    }

    /// Value of text constant `index`, allocated on its first load.
    fn load_text(&mut self, index: u32, span: Span) -> Result<Value, RuntimeError> {
        let index = index as usize;
        if let Some(Some(value)) = self.texts.get(index) {
            return Ok(value.clone());
        }
        let Some(text) = self.code.texts.get(index) else {
            return Ok(self.empty.clone());
        };
        let value = self.alloc_text(text.len() as u64, span, |buf| buf.push_str(text))?;
        if let Some(slot) = self.texts.get_mut(index) {
            *slot = Some(value.clone());
        }
        Ok(value)
    }

    /// Creates the frame for a call of `func`, after checking `CallDepth` and
    /// `HeapLimit`. `depth` is the number of frames held before this one.
    fn new_frame(
        &self,
        func: u32,
        args: &[Value],
        ret: Option<u32>,
        depth: usize,
        span: Span,
    ) -> Result<Frame, RuntimeError> {
        let error = |kind| RuntimeError { kind, span };
        if depth >= self.limits.max_frames as usize {
            return Err(error(RuntimeErrorKind::CallDepth));
        }
        let locals: &[Ty] = self
            .code
            .funcs
            .get(func as usize)
            .map_or(&[], |f| &f.locals);
        let charge = (locals.len() as u64)
            .saturating_mul(VALUE_BYTES)
            .saturating_add(FRAME_OVERHEAD);
        self.meter
            .charge(charge, self.limits.max_heap_bytes)
            .map_err(error)?;
        let regs = locals
            .iter()
            .enumerate()
            .map(|(index, ty)| match args.get(index) {
                Some(arg) => arg.clone(),
                None => match ty {
                    Ty::Text => self.empty.clone(),
                    Ty::Bool => Value::Bool(false),
                    Ty::Int | Ty::Unit => Value::Int(0),
                },
            })
            .collect();
        Ok(Frame {
            func,
            pc: 0,
            regs,
            ret,
            charge,
        })
    }

    /// Span of the call expression in `main` that is running, or `None` while `main`
    /// itself is the running frame (ARCHITECTURE 3.4, D83). `main` is the bottom
    /// caller, and its `pc` already points past the `Call` that created frame 2.
    fn main_call_span(&self) -> Option<Span> {
        let main = self.stack.first()?;
        let pc = main.pc.checked_sub(1)?;
        self.code
            .funcs
            .get(main.func as usize)?
            .spans
            .get(pc as usize)
            .copied()
    }

    /// Runs until `main` returns.
    fn execute(&mut self, mut cur: Frame) -> Result<(), RuntimeError> {
        let code = self.code;
        loop {
            let Some(func) = code.funcs.get(cur.func as usize) else {
                return Ok(());
            };
            let pc = cur.pc as usize;
            let (Some(op), Some(&span)) = (func.ops.get(pc), func.spans.get(pc)) else {
                return Ok(());
            };
            if self.steps >= self.limits.max_steps {
                return Err(RuntimeError {
                    kind: RuntimeErrorKind::StepLimit,
                    span: self.main_call_span().unwrap_or(span),
                });
            }
            self.steps += 1;
            cur.pc = cur.pc.wrapping_add(1);
            let error = |kind| RuntimeError { kind, span };
            match op {
                Op::LoadInt { dst, value } => cur.set(*dst, Value::Int(*value)),
                Op::LoadBool { dst, value } => cur.set(*dst, Value::Bool(*value)),
                Op::LoadText { dst, index } => {
                    let value = self.load_text(*index, span)?;
                    cur.set(*dst, value);
                }
                Op::Copy { dst, src } => {
                    let value = cur.reg(*src);
                    cur.set(*dst, value);
                }
                Op::Neg { dst, arg } => {
                    let value = checked(cur.int(*arg).checked_neg()).map_err(error)?;
                    cur.set(*dst, Value::Int(value));
                }
                Op::Not { dst, arg } => {
                    let value = !cur.boolean(*arg);
                    cur.set(*dst, Value::Bool(value));
                }
                Op::Arith { dst, op, lhs, rhs } => {
                    let value = arith(*op, cur.int(*lhs), cur.int(*rhs)).map_err(error)?;
                    cur.set(*dst, Value::Int(value));
                }
                Op::Order { dst, op, lhs, rhs } => {
                    let (a, b) = (cur.int(*lhs), cur.int(*rhs));
                    let value = match op {
                        Order::Lt => a < b,
                        Order::Le => a <= b,
                        Order::Gt => a > b,
                        Order::Ge => a >= b,
                    };
                    cur.set(*dst, Value::Bool(value));
                }
                Op::Equal {
                    dst,
                    lhs,
                    rhs,
                    negate,
                } => {
                    let value = cur.reg(*lhs).same(&cur.reg(*rhs)) != *negate;
                    cur.set(*dst, Value::Bool(value));
                }
                Op::ToText { dst, src } => {
                    let value = match cur.reg(*src) {
                        text @ Value::Text(_) => text,
                        Value::Int(v) => self.alloc_text(int_text_len(v), span, |buf| {
                            buf.push_str(&v.to_string());
                        })?,
                        Value::Bool(v) => {
                            let word = if v { "true" } else { "false" };
                            self.alloc_text(word.len() as u64, span, |buf| buf.push_str(word))?
                        }
                    };
                    cur.set(*dst, value);
                }
                Op::Concat { dst, parts } => {
                    let values: Vec<Value> = parts.iter().map(|p| cur.reg(*p)).collect();
                    let len = values.iter().fold(0u64, |sum, value| match value {
                        Value::Text(t) => sum.saturating_add(t.text.len() as u64),
                        _ => sum,
                    });
                    let value = self.alloc_text(len, span, |buf| {
                        for value in &values {
                            if let Value::Text(t) = value {
                                buf.push_str(&t.text);
                            }
                        }
                    })?;
                    cur.set(*dst, value);
                }
                Op::Call { dst, func, args } => {
                    let values: Vec<Value> = args.iter().map(|a| cur.reg(*a)).collect();
                    let depth = self.stack.len().saturating_add(1);
                    let callee = self.new_frame(*func, &values, *dst, depth, span)?;
                    self.stack.push(std::mem::replace(&mut cur, callee));
                }
                Op::Print { arg } => match cur.reg(*arg) {
                    Value::Text(t) => self.host.print(&t.text),
                    Value::Int(v) => self.host.print(&v.to_string()),
                    Value::Bool(v) => self.host.print(if v { "true" } else { "false" }),
                },
                Op::Jump { target } => cur.pc = *target,
                Op::Branch {
                    cond,
                    then,
                    otherwise,
                } => {
                    cur.pc = if cur.boolean(*cond) {
                        *then
                    } else {
                        *otherwise
                    }
                }
                Op::Return { src } => {
                    let result = src.map(|s| cur.reg(s));
                    let ret = cur.ret;
                    self.meter.release(cur.charge);
                    let Some(caller) = self.stack.pop() else {
                        return Ok(());
                    };
                    cur = caller;
                    if let (Some(dst), Some(value)) = (ret, result) {
                        cur.set(dst, value);
                    }
                }
            }
        }
    }
}

/// Maps an `Int` result to `IntOverflow` if it is missing or outside the D24 range.
fn checked(value: Option<i64>) -> Result<i64, RuntimeErrorKind> {
    match value {
        Some(v) if (INT_MIN..=INT_MAX).contains(&v) => Ok(v),
        _ => Err(RuntimeErrorKind::IntOverflow),
    }
}

/// Checked `Int` arithmetic. `Div` truncates toward zero and `Rem` has the sign of
/// the dividend (ARCHITECTURE 3.4); a zero divisor is `DivisionByZero`.
fn arith(op: Arith, a: i64, b: i64) -> Result<i64, RuntimeErrorKind> {
    match op {
        Arith::Add => checked(a.checked_add(b)),
        Arith::Sub => checked(a.checked_sub(b)),
        Arith::Mul => checked(a.checked_mul(b)),
        Arith::Div | Arith::Rem if b == 0 => Err(RuntimeErrorKind::DivisionByZero),
        Arith::Div => checked(a.checked_div(b)),
        Arith::Rem => checked(a.checked_rem(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Image, compile};
    use ostrel_ir::{
        BinOp, Block, BlockId, Const, FuncId, Function, Inst, InstKind, Local, Program, Terminator,
    };

    struct Capture(Vec<String>);

    impl Host for Capture {
        fn print(&mut self, text: &str) {
            self.0.push(text.to_string());
        }
    }

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

    fn call(func: u32, args: &[u32]) -> InstKind {
        InstKind::Call {
            dst: None,
            func: FuncId(func),
            args: args.iter().map(|a| Local(*a)).collect(),
        }
    }

    fn int(dst: u32, value: i64) -> InstKind {
        InstKind::Const {
            dst: Local(dst),
            value: Const::Int(value),
        }
    }

    fn func(name: &str, params: u32, locals: &[Ty], start: u32, blocks: Vec<Block>) -> Function {
        Function {
            name: name.to_string(),
            params,
            locals: locals.to_vec(),
            result: Ty::Unit,
            blocks,
            span: sp(start),
        }
    }

    /// A function whose only block jumps to itself.
    fn spinner(name: &str, start: u32) -> Function {
        func(
            name,
            0,
            &[Ty::Int],
            start,
            vec![Block {
                insts: vec![at(start + 1, int(0, 1))],
                term: Terminator::Jump(BlockId(0)),
            }],
        )
    }

    /// `fn down(n: Int) { if n == 0 { return } down(n - 1) }` as function `id`.
    fn down(id: u32) -> Function {
        func(
            "down",
            1,
            &[Ty::Int, Ty::Int, Ty::Bool],
            900,
            vec![
                Block {
                    insts: vec![
                        at(901, int(1, 0)),
                        at(
                            902,
                            InstKind::Binary {
                                dst: Local(2),
                                op: BinOp::Eq,
                                lhs: Local(0),
                                rhs: Local(1),
                            },
                        ),
                    ],
                    term: Terminator::Branch {
                        cond: Local(2),
                        then: BlockId(1),
                        otherwise: BlockId(2),
                    },
                },
                Block {
                    insts: vec![],
                    term: Terminator::Return(None),
                },
                Block {
                    insts: vec![
                        at(903, int(1, 1)),
                        at(
                            904,
                            InstKind::Binary {
                                dst: Local(1),
                                op: BinOp::Sub,
                                lhs: Local(0),
                                rhs: Local(1),
                            },
                        ),
                        at(905, call(id, &[1])),
                    ],
                    term: Terminator::Return(None),
                },
            ],
        )
    }

    fn image(functions: Vec<Function>) -> Option<Image> {
        compile(&Program {
            functions,
            main: FuncId(0),
        })
        .ok()
    }

    fn step_limit_at(image: &Image, max_steps: u64) -> Option<u32> {
        let limits = Limits {
            max_steps,
            ..Limits::default()
        };
        let mut host = Capture(Vec::new());
        match crate::run(image, &mut host, limits) {
            Err(RuntimeError {
                kind: RuntimeErrorKind::StepLimit,
                span,
            }) => Some(span.start),
            _ => None,
        }
    }

    #[test]
    fn step_limit_in_main_reports_the_instruction() {
        // `fn main() { loop { } }` with one constant per round.
        let image = image(vec![spinner("main", 10)]);
        assert!(image.is_some(), "program compiles");
        let Some(image) = image else { return };
        for max_steps in [1, 2, 3, 1000, 1001] {
            assert_eq!(step_limit_at(&image, max_steps), Some(11), "{max_steps}");
        }
    }

    #[test]
    fn step_limit_in_recursion_reports_the_call_in_main() {
        // `fn main() { let n = 100000; down(n) }`: the failing instruction inside
        // `down` depends on the limit, the reported call does not.
        let main = func(
            "main",
            0,
            &[Ty::Int],
            0,
            vec![Block {
                insts: vec![at(3, int(0, 100_000)), at(7, call(1, &[0]))],
                term: Terminator::Return(None),
            }],
        );
        let image = image(vec![main, down(1)]);
        assert!(image.is_some(), "program compiles");
        let Some(image) = image else { return };
        // With one step only the constant in `main` would run next.
        assert_eq!(step_limit_at(&image, 0), Some(3));
        assert_eq!(step_limit_at(&image, 1), Some(7));
        for max_steps in [2, 3, 4, 5, 6, 7, 99, 500, 501, 502, 503] {
            assert_eq!(step_limit_at(&image, max_steps), Some(7), "{max_steps}");
        }
    }

    #[test]
    fn step_limit_reports_the_running_call_of_main_through_nested_calls() {
        // `fn main() { a(); b() }`, `fn a() { }`, `fn b() { c() }`, `fn c() { loop { } }`.
        let main = func(
            "main",
            0,
            &[],
            0,
            vec![Block {
                insts: vec![at(20, call(1, &[])), at(30, call(2, &[]))],
                term: Terminator::Return(None),
            }],
        );
        let a = func(
            "a",
            0,
            &[],
            100,
            vec![Block {
                insts: vec![],
                term: Terminator::Return(None),
            }],
        );
        let b = func(
            "b",
            0,
            &[],
            200,
            vec![Block {
                insts: vec![at(201, call(3, &[]))],
                term: Terminator::Return(None),
            }],
        );
        let image = image(vec![main, a, b, spinner("c", 300)]);
        assert!(image.is_some(), "program compiles");
        let Some(image) = image else { return };
        // Steps: call a, return in a, call b, then b and c.
        assert_eq!(step_limit_at(&image, 1), Some(20));
        assert_eq!(step_limit_at(&image, 2), Some(30));
        for max_steps in [3, 4, 5, 6, 50, 51] {
            assert_eq!(step_limit_at(&image, max_steps), Some(30), "{max_steps}");
        }
    }

    #[test]
    fn int_text_len_matches_formatting() {
        for v in [
            0,
            7,
            -7,
            10,
            -10,
            99,
            100,
            INT_MAX,
            INT_MIN,
            i64::MIN,
            i64::MAX,
        ] {
            assert_eq!(int_text_len(v), v.to_string().len() as u64, "{v}");
        }
    }

    #[test]
    fn arithmetic_follows_the_architecture() {
        assert_eq!(arith(Arith::Div, 7, 2), Ok(3));
        assert_eq!(arith(Arith::Div, -7, 2), Ok(-3));
        assert_eq!(arith(Arith::Rem, -7, 2), Ok(-1));
        assert_eq!(arith(Arith::Rem, 7, -2), Ok(1));
        assert_eq!(arith(Arith::Div, INT_MIN, -1), Ok(INT_MAX));
        assert_eq!(
            arith(Arith::Rem, 1, 0),
            Err(RuntimeErrorKind::DivisionByZero)
        );
        assert_eq!(
            arith(Arith::Add, INT_MAX, 1),
            Err(RuntimeErrorKind::IntOverflow)
        );
        assert_eq!(
            arith(Arith::Sub, INT_MIN, 1),
            Err(RuntimeErrorKind::IntOverflow)
        );
        assert_eq!(
            arith(Arith::Mul, INT_MAX, INT_MAX),
            Err(RuntimeErrorKind::IntOverflow)
        );
    }

    #[test]
    fn meter_checks_before_charging() {
        let meter = Meter(Rc::new(Cell::new(0)));
        assert_eq!(meter.charge(10, 10), Ok(()));
        assert_eq!(meter.charge(1, 10), Err(RuntimeErrorKind::HeapLimit));
        assert_eq!(meter.0.get(), 10);
        meter.release(4);
        assert_eq!(
            meter.charge(u64::MAX, u64::MAX),
            Err(RuntimeErrorKind::HeapLimit)
        );
        assert_eq!(meter.0.get(), 6);
    }
}
