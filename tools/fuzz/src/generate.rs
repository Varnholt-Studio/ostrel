//! Grammar aware generation of Ostrel source text (SYNTAX v1.1 section 7, G1 to G16).
//!
//! The generator writes mostly well formed programs, so inputs get past the lexer
//! into the parser and the checker, and then applies "twists": local, deliberate
//! violations and limit probes (deep nesting, long operator chains, tabs, bidi
//! controls, nested interpolation, bad escapes, invalid UTF-8). Every byte depends
//! only on the [`Rng`] passed in.

use crate::rng::Rng;

/// Keywords of SYNTAX section 6 (complete for v0.3).
pub const KEYWORDS: &[&str] = &[
    "app", "auth", "home", "data", "enum", "check", "merge", "fixed", "server", "serial", "per",
    "var", "let", "fn", "call", "as", "system", "return", "if", "else", "for", "in", "where",
    "sort", "desc", "limit", "last", "see", "make", "edit", "drop", "view", "style", "extern",
    "on", "and", "or", "not", "me", "now", "none", "signed", "true", "false", "catch",
];

const VALUE_NAMES: &[&str] = &[
    "x", "y", "n", "total", "it", "name", "members", "text", "r", "m",
];
const TYPE_NAMES: &[&str] = &["Room", "Message", "Issue", "Team", "User", "Status"];
const BASE_TYPES: &[&str] = &["Int", "Text", "Bool", "Time", "Url", "Rank"];
const ELEMENTS: &[&str] = &[
    "row", "col", "split", "text", "button", "input", "entry", "pick", "img", "link", "rawHtml",
];
const ATTRS: &[&str] = &["look", "selected", "scroll", "bind", "hint", "submit"];
const BIN_OPS: &[&str] = &[
    "+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "and", "or", "in", "??", "..",
];
const INTS: &[&str] = &[
    "0",
    "1",
    "-1",
    "42",
    "9007199254740991",
    "9007199254740992",
    "-9007199254740991",
    "9223372036854775807",
    "99999999999999999999999999",
    "007",
];
const CSS_SNIPPETS: &[&str] = &[
    "background: url(//host/x);",
    "/* comment */",
    "/* unterminated",
    "color: $accentSoft;",
    "grid-template-columns: 16rem 1fr;",
    "content: \"}\";",
    "// not a comment in css",
    "}",
    "{",
];
/// The nine bidirectional control characters the lexer rejects raw (D53).
const BIDI: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];
const ESCAPES: &[&str] = &[
    "\\{",
    "\\}",
    "\\\"",
    "\\\\",
    "\\n",
    "\\t",
    "\\u{e9}",
    "\\u{1F600}",
    "\\u{0}",
    "\\u{202E}",
];
const BAD_ESCAPES: &[&str] = &[
    "\\r",
    "\\0",
    "\\u{}",
    "\\u{1234567}",
    "\\u{D800}",
    "\\u{110000}",
    "\\u{41",
    "\\x41",
    "\\",
];

/// Limits of one generated program. The defaults keep inputs small and fast; twists
/// may exceed them on purpose.
#[derive(Debug, Clone)]
pub struct GenConfig {
    /// Maximum number of top level declarations.
    pub max_decls: usize,
    /// Maximum statements or child nodes per block.
    pub max_block: usize,
    /// Maximum nesting of blocks and expressions before the generator bottoms out.
    pub max_depth: usize,
    /// Probability in percent that a twist is applied to the program.
    pub twist_percent: u32,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            max_decls: 6,
            max_block: 5,
            max_depth: 5,
            twist_percent: 35,
        }
    }
}

/// Generates one program as raw bytes (usually UTF-8, not always: see the twists).
pub fn program(rng: &mut Rng, cfg: &GenConfig) -> Vec<u8> {
    let mut g = Gen {
        rng,
        cfg,
        out: String::new(),
    };
    g.program();
    let mut bytes = g.out.into_bytes();
    if g.rng.chance(cfg.twist_percent) {
        let twists = g.rng.range(1, 2);
        for _ in 0..twists {
            twist(g.rng, &mut bytes);
        }
    }
    bytes
}

struct Gen<'a> {
    rng: &'a mut Rng,
    cfg: &'a GenConfig,
    out: String,
}

impl Gen<'_> {
    fn line(&mut self, indent: usize, text: &str) {
        for _ in 0..indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn word(&mut self, items: &[&str]) -> String {
        self.rng.pick(items).copied().unwrap_or("x").to_string()
    }

    fn program(&mut self) {
        // About half the inputs are v0.1 script programs (G15), the rest use the
        // full v0.3 grammar.
        let script = self.rng.chance(50);
        if !script && self.rng.chance(60) {
            self.app();
        }
        let decls = self.rng.range(1, self.cfg.max_decls);
        for _ in 0..decls {
            if script {
                self.function(0);
            } else {
                match self.rng.below(8) {
                    0 => self.enum_decl(),
                    1 | 2 => self.data_decl(),
                    3 => self.var_decl(),
                    4 => self.view_decl(),
                    5 => self.style_decl(),
                    6 => self.extern_decl(),
                    _ => self.function(0),
                }
            }
            if self.rng.chance(40) {
                self.out.push('\n');
            }
        }
        if script || self.rng.chance(30) {
            self.main_fn();
        }
    }

    fn app(&mut self) {
        let name = self.word(TYPE_NAMES);
        self.line(0, &format!("app {name}"));
        let mode = if self.rng.chance(50) {
            "open"
        } else {
            "invite"
        };
        self.line(1, &format!("auth paseto {mode}"));
        if self.rng.chance(30) {
            let home = self.word(TYPE_NAMES);
            self.line(1, &format!("home {home}"));
        }
    }

    fn enum_decl(&mut self) {
        let name = self.word(TYPE_NAMES);
        let n = self.rng.range(1, 4);
        let variants: Vec<String> = (0..n).map(|_| self.word(VALUE_NAMES)).collect();
        self.line(0, &format!("enum {name} = {}", variants.join(" | ")));
    }

    fn ty(&mut self, depth: usize) -> String {
        let mut t = if self.rng.chance(60) {
            self.word(BASE_TYPES)
        } else {
            self.word(TYPE_NAMES)
        };
        if depth < 3 && self.rng.chance(20) {
            let coll = self.word(&["Set", "List", "Map", "Remote"]);
            let inner = self.ty(depth + 1);
            t = if coll == "Map" {
                let value = self.ty(depth + 1);
                format!("Map[{inner}, {value}]")
            } else {
                format!("{coll}[{inner}]")
            };
        }
        if self.rng.chance(15) {
            t.push('?');
        }
        t
    }

    fn data_decl(&mut self) {
        let name = self.word(TYPE_NAMES);
        self.line(0, &format!("data {name}"));
        let members = self.rng.range(1, self.cfg.max_block);
        for _ in 0..members {
            match self.rng.below(4) {
                0 | 1 => {
                    let field = self.word(VALUE_NAMES);
                    let ty = self.ty(0);
                    let mut l = format!("{field}: {ty}");
                    if self.rng.chance(20) {
                        let e = self.expr(1);
                        l.push_str(&format!(" = {e}"));
                    }
                    if self.rng.chance(15) {
                        let mode = self.word(&["fixed", "server", "serial", "serial per team"]);
                        l.push_str(&format!(" {mode}"));
                    }
                    if self.rng.chance(10) {
                        l.push_str(" merge text");
                    }
                    self.line(1, &l);
                }
                2 => {
                    let verb = self.word(&["see", "make", "edit", "drop", "edit members"]);
                    let e = self.expr(1);
                    self.line(1, &format!("{verb} if {e}"));
                }
                _ => {
                    let e = self.expr(1);
                    self.line(1, &format!("check {e}"));
                }
            }
        }
    }

    fn var_decl(&mut self) {
        let name = self.word(VALUE_NAMES);
        if self.rng.chance(50) {
            let ty = self.ty(0);
            self.line(0, &format!("var {name}: {ty}"));
        } else {
            let e = self.expr(1);
            self.line(0, &format!("var {name} = {e}"));
        }
    }

    fn params(&mut self) -> String {
        let n = self.rng.below(4);
        let ps: Vec<String> = (0..n)
            .map(|_| {
                let p = self.word(VALUE_NAMES);
                let t = self.ty(1);
                format!("{p}: {t}")
            })
            .collect();
        format!("({})", ps.join(", "))
    }

    fn main_fn(&mut self) {
        self.line(0, "fn main()");
        self.block(1, 1);
    }

    fn function(&mut self, indent: usize) {
        let mut head = String::new();
        if self.rng.chance(25) {
            head.push_str(&self.word(&["server ", "client "]));
        }
        let name = self.word(&["fib", "gcd", "step", "send", "closeDone", "main"]);
        let params = self.params();
        head.push_str(&format!("fn {name}{params}"));
        if self.rng.chance(50) {
            let t = self.ty(1);
            head.push_str(&format!(" -> {t}"));
        }
        if self.rng.chance(8) {
            head.push_str(" as system");
        }
        self.line(indent, &head);
        if self.rng.chance(15) {
            let e = self.expr(1);
            self.line(indent + 1, &format!("call if {e}"));
        }
        self.block(indent + 1, 1);
    }

    fn block(&mut self, indent: usize, depth: usize) {
        let n = self.rng.range(1, self.cfg.max_block);
        for _ in 0..n {
            self.stmt(indent, depth);
        }
    }

    fn stmt(&mut self, indent: usize, depth: usize) {
        let nest = depth < self.cfg.max_depth;
        match self.rng.below(9) {
            0 | 1 => {
                let name = self.word(VALUE_NAMES);
                let e = self.expr(depth);
                self.line(indent, &format!("let {name} = {e}"));
            }
            2 if nest => {
                let c = self.cond(depth);
                self.line(indent, &format!("if {c}"));
                self.block(indent + 1, depth + 1);
                if self.rng.chance(50) {
                    self.line(indent, "else");
                    self.block(indent + 1, depth + 1);
                }
            }
            3 if nest => {
                let name = self.word(VALUE_NAMES);
                let src = self.source(depth);
                self.line(indent, &format!("for {name} in {src}"));
                self.block(indent + 1, depth + 1);
            }
            4 => {
                let e = self.expr(depth);
                self.line(indent, &format!("return {e}"));
            }
            5 => {
                let target = self.postfix(depth);
                let e = self.expr(depth);
                self.line(indent, &format!("{target} = {e}"));
            }
            6 => {
                let src = self.source(depth);
                self.line(indent, &format!("drop {src}"));
            }
            _ => {
                let e = self.expr(depth);
                self.line(indent, &format!("print({e})"));
            }
        }
    }

    fn cond(&mut self, depth: usize) -> String {
        if self.rng.chance(15) {
            let name = self.word(VALUE_NAMES);
            let e = self.expr(depth);
            format!("let {name} = {e}")
        } else {
            self.expr(depth)
        }
    }

    fn source(&mut self, depth: usize) -> String {
        if !self.rng.chance(50) {
            return self.expr(depth);
        }
        let mut q = self.word(TYPE_NAMES);
        if self.rng.chance(60) {
            let e = self.expr(depth + 1);
            q.push_str(&format!(" where {e}"));
        }
        if self.rng.chance(50) {
            let f = self.word(&["made", "name", "changed", "rank"]);
            q.push_str(&format!(" sort {f}"));
            if self.rng.chance(30) {
                q.push_str(" desc");
            }
        }
        if self.rng.chance(40) {
            let kw = self.word(&["limit", "last"]);
            let n = self.word(INTS);
            q.push_str(&format!(" {kw} {n}"));
        }
        q
    }

    fn expr(&mut self, depth: usize) -> String {
        if depth >= self.cfg.max_depth || self.rng.chance(35) {
            return self.postfix(depth);
        }
        match self.rng.below(6) {
            0 | 1 => {
                let l = self.expr(depth + 1);
                let op = self.word(BIN_OPS);
                let r = self.expr(depth + 1);
                format!("{l} {op} {r}")
            }
            2 => {
                let e = self.expr(depth + 1);
                format!("not {e}")
            }
            3 => {
                let e = self.postfix(depth + 1);
                format!("-{e}")
            }
            4 => {
                let e = self.postfix(depth + 1);
                let kinds = self.word(&["Denied", "Denied, Invalid", "Offline"]);
                let f = self.postfix(depth + 1);
                format!("{e} catch {kinds} ?? {f}")
            }
            _ => {
                let e = self.expr(depth + 1);
                format!("({e})")
            }
        }
    }

    fn postfix(&mut self, depth: usize) -> String {
        let mut p = self.primary(depth);
        let n = if depth < self.cfg.max_depth {
            self.rng.below(3)
        } else {
            0
        };
        for _ in 0..n {
            match self.rng.below(3) {
                0 => {
                    let f = self.word(&["len", "trim", "members", "handle", "clock", "add"]);
                    p.push_str(&format!(".{f}"));
                }
                1 => {
                    let args = self.args(depth + 1);
                    p.push_str(&format!("({args})"));
                }
                _ => {
                    let e = self.expr(depth + 1);
                    p.push_str(&format!("[{e}]"));
                }
            }
        }
        p
    }

    fn args(&mut self, depth: usize) -> String {
        let n = self.rng.below(3);
        let xs: Vec<String> = (0..n).map(|_| self.expr(depth)).collect();
        xs.join(", ")
    }

    fn primary(&mut self, depth: usize) -> String {
        let deeper = depth < self.cfg.max_depth;
        match self.rng.below(12) {
            0 | 1 => self.word(INTS),
            2 | 3 => self.string(depth),
            4 => self.word(&["true", "false", "none", "me", "now", "signed"]),
            5 if deeper => {
                let f = self.word(&["print", "fib", "gcd", "step"]);
                let args = self.args(depth + 1);
                format!("{f}({args})")
            }
            6 if deeper => {
                let items = self.args(depth + 1);
                format!("{{{items}}}")
            }
            7 if deeper => {
                if self.rng.chance(30) {
                    "[:]".to_string()
                } else if self.rng.chance(50) {
                    let k = self.expr(depth + 1);
                    let v = self.expr(depth + 1);
                    format!("[{k}: {v}]")
                } else {
                    let items = self.args(depth + 1);
                    format!("[{items}]")
                }
            }
            8 if deeper => {
                let t = self.word(TYPE_NAMES);
                let f = self.word(VALUE_NAMES);
                let e = self.expr(depth + 1);
                format!("make {t} {{ {f}: {e} }}")
            }
            _ => self.word(VALUE_NAMES),
        }
    }

    /// A string literal with interpolations and escapes (G1). Interpolations hold
    /// expressions without string literals, as the lexer requires.
    fn string(&mut self, depth: usize) -> String {
        let mut s = String::from("\"");
        let parts = self.rng.below(4);
        for _ in 0..parts {
            match self.rng.below(4) {
                0 => s.push_str(&self.word(&["abc", "fib(20) = ", "Caf\u{e9} ", "\u{1F600}", " "])),
                1 => s.push_str(&self.word(ESCAPES)),
                _ if depth < self.cfg.max_depth => {
                    let name = self.word(VALUE_NAMES);
                    let op = self.word(&["+", "*", "-"]);
                    let n = self.word(INTS);
                    s.push_str(&format!("{{{name} {op} {n}}}"));
                }
                _ => s.push('x'),
            }
        }
        s.push('"');
        s
    }

    fn view_decl(&mut self) {
        let name = self.word(TYPE_NAMES);
        let params = if self.rng.chance(30) {
            self.params()
        } else {
            String::new()
        };
        self.line(0, &format!("view {name}{params}"));
        let n = self.rng.range(1, self.cfg.max_block);
        for _ in 0..n {
            self.node(1, 1);
        }
    }

    fn node(&mut self, indent: usize, depth: usize) {
        let nest = depth < self.cfg.max_depth;
        match self.rng.below(6) {
            0 if nest => {
                let c = self.cond(depth);
                self.line(indent, &format!("if {c}"));
                self.node(indent + 1, depth + 1);
                if self.rng.chance(40) {
                    self.line(indent, "else");
                    self.node(indent + 1, depth + 1);
                }
            }
            1 if nest => {
                let name = self.word(VALUE_NAMES);
                let src = self.source(depth);
                self.line(indent, &format!("for {name} in {src}"));
                self.node(indent + 1, depth + 1);
            }
            _ => {
                let mut l = self.word(ELEMENTS);
                if self.rng.chance(60) {
                    let v = self.expr(depth + 1);
                    l.push_str(&format!(" {v}"));
                }
                for _ in 0..self.rng.below(3) {
                    if self.rng.chance(50) {
                        let a = self.word(ATTRS);
                        let v = self.expr(depth + 1);
                        l.push_str(&format!(" {a}: {v}"));
                    } else {
                        let tag = self.word(&["mine", "pending", "active", "title"]);
                        if self.rng.chance(50) {
                            let c = self.expr(depth + 1);
                            l.push_str(&format!(" ~{tag}({c})"));
                        } else {
                            l.push_str(&format!(" ~{tag}"));
                        }
                    }
                }
                if self.rng.chance(30) {
                    let h = self.handler(depth);
                    l.push_str(&format!(" {h}"));
                }
                self.line(indent, &l);
                if nest && self.rng.chance(50) {
                    let kids = self.rng.range(1, 3);
                    for _ in 0..kids {
                        if self.rng.chance(20) {
                            let h = self.handler(depth);
                            self.line(indent + 1, &h);
                        } else {
                            self.node(indent + 1, depth + 1);
                        }
                    }
                }
            }
        }
    }

    fn handler(&mut self, depth: usize) -> String {
        let event = self.word(&["click", "submit", "enter", "change"]);
        let target = self.postfix(depth + 1);
        if self.rng.chance(50) {
            let e = self.expr(depth + 1);
            format!("on {event} {target} = {e}")
        } else {
            format!("on {event} {target}")
        }
    }

    fn style_decl(&mut self) {
        self.line(0, "style");
        let rules = self.rng.range(1, 3);
        for _ in 0..rules {
            let sel = self.word(&[
                ".mine",
                ".app,",
                ".log",
                "a:hover",
                "@media (max-width: 40rem)",
            ]);
            self.line(1, &format!("{sel} {{"));
            let decls = self.rng.range(1, 3);
            for _ in 0..decls {
                let d = self.word(CSS_SNIPPETS);
                self.line(2, &d);
            }
            self.line(1, "}");
        }
    }

    fn extern_decl(&mut self) {
        let side = self.word(&["client", "server"]);
        let module = self.word(&["\"marked\"", "\"./local.js\"", "\"\"", "\"{x}\""]);
        self.line(0, &format!("extern js {side} {module} as md"));
        let n = self.rng.range(1, 3);
        for _ in 0..n {
            let name = self.word(VALUE_NAMES);
            let params = self.params();
            let t = self.ty(1);
            self.line(1, &format!("fn {name}{params} -> {t}"));
        }
    }
}

/// Applies one twist to a generated program. Twists target the limits and the must
/// diagnose cases of SPEC AC-04 and AC-36 and of D53 and D54.
pub fn twist(rng: &mut Rng, bytes: &mut Vec<u8>) {
    let at = line_start(bytes, rng);
    match rng.below(13) {
        // Deep nesting of parentheses, blocks or view elements (depth limit 256).
        0 => {
            let depth = rng.range(200, 3000);
            let mut s = String::from("fn main()\n  let x = ");
            s.push_str(&"(".repeat(depth));
            s.push('1');
            s.push_str(&")".repeat(depth));
            s.push('\n');
            insert(bytes, at, s.as_bytes());
        }
        1 => {
            let depth = rng.range(100, 600);
            let mut s = String::from("view Deep\n");
            for i in 1..=depth {
                s.push_str(&"  ".repeat(i));
                s.push_str("col\n");
            }
            insert(bytes, at, s.as_bytes());
        }
        // Flat operator chains around the AST height limit of 2 048 (D54).
        2 => {
            let n = rng.range(1900, 2200);
            let op = rng
                .pick(&[" + ", " * ", " and ", " ?? "])
                .copied()
                .unwrap_or(" + ");
            let mut s = String::from("fn main()\n  let x = 1");
            for _ in 1..n {
                s.push_str(op);
                s.push('1');
            }
            s.push('\n');
            insert(bytes, at, s.as_bytes());
        }
        // Tabs in indentation (a lex error, SYNTAX 3).
        3 => {
            if let Some(pos) = bytes.windows(2).position(|w| w == b"  ") {
                bytes.splice(pos..pos + 2, [b'\t']);
            } else {
                insert(bytes, at, b"\tlet x = 1\n");
            }
        }
        // Raw bidi control characters (D53), in a comment or a string.
        4 => {
            let c = rng.pick(BIDI).copied().unwrap_or('\u{202E}');
            let s = if rng.chance(50) {
                format!("// admin{c} check\n")
            } else {
                format!("fn main()\n  print(\"a{c}b\")\n")
            };
            insert(bytes, at, s.as_bytes());
        }
        // Nested interpolation up to and past the brace depth 32 (G1).
        5 => {
            let depth = rng.range(1, 40);
            let mut s = String::from("fn main()\n  print(\"");
            s.push_str(&"{".repeat(depth));
            s.push('x');
            s.push_str(&"}".repeat(depth));
            if rng.chance(50) {
                s.push_str("{\"inner\"}");
            }
            s.push_str("\")\n");
            insert(bytes, at, s.as_bytes());
        }
        // Invalid escapes (E0004, E0005).
        6 => {
            let e = rng.pick(BAD_ESCAPES).copied().unwrap_or("\\r");
            let s = format!("fn main()\n  print(\"a{e}b\")\n");
            insert(bytes, at, s.as_bytes());
        }
        // Unterminated string or interpolation at end of file.
        7 => {
            let tail = rng
                .pick(&["\"abc", "\"{x", "\"\\", "\"\\u{"])
                .copied()
                .unwrap_or("\"");
            bytes.extend_from_slice(tail.as_bytes());
        }
        // Invalid UTF-8 and NUL bytes.
        8 => {
            let junk: &[&[u8]] = &[
                b"\xff",
                b"\xc3\x28",
                b"\xed\xa0\x80",
                b"\x00",
                b"\xf4\x90\x80\x80",
            ];
            let j = rng.pick(junk).copied().unwrap_or(b"\xff");
            insert(bytes, at, j);
        }
        // CRLF line endings.
        9 => {
            let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 8);
            for &b in bytes.iter() {
                if b == b'\n' {
                    out.push(b'\r');
                }
                out.push(b);
            }
            *bytes = out;
        }
        // Indentation that jumps by two levels.
        10 => insert(bytes, at, b"fn main()\n      let x = 1\n"),
        // Keyword soup on one line.
        11 => {
            let n = rng.range(5, 60);
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(rng.pick(KEYWORDS).copied().unwrap_or("fn"));
                s.push(' ');
            }
            s.push('\n');
            insert(bytes, at, s.as_bytes());
        }
        // One long line (width and lexer throughput).
        _ => {
            let n = rng.range(10_000, 200_000);
            let mut s = String::from("fn main()\n  let x = \"");
            s.push_str(&"a".repeat(n));
            s.push_str("\"\n");
            insert(bytes, at, s.as_bytes());
        }
    }
}

fn line_start(bytes: &[u8], rng: &mut Rng) -> usize {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect();
    rng.pick(&starts).copied().unwrap_or(0)
}

fn insert(bytes: &mut Vec<u8>, at: usize, what: &[u8]) {
    let at = at.min(bytes.len());
    bytes.splice(at..at, what.iter().copied());
}

#[cfg(test)]
mod tests {
    use super::{GenConfig, program, twist};
    use crate::rng::Rng;

    #[test]
    fn generation_is_deterministic() {
        let cfg = GenConfig::default();
        for i in 0..200 {
            let a = program(&mut Rng::for_input(5, i), &cfg);
            let b = program(&mut Rng::for_input(5, i), &cfg);
            assert_eq!(a, b, "iteration {i}");
        }
    }

    #[test]
    fn untwisted_programs_are_utf8_and_indented_with_spaces() {
        let cfg = GenConfig {
            twist_percent: 0,
            ..GenConfig::default()
        };
        for i in 0..500 {
            let p = program(&mut Rng::for_input(11, i), &cfg);
            let text = String::from_utf8(p).expect("generated text is UTF-8");
            assert!(!text.is_empty());
            for line in text.lines() {
                let indent = line.len() - line.trim_start_matches(' ').len();
                assert_eq!(indent % 2, 0, "odd indentation in {line:?}");
                assert!(!line.starts_with('\t'));
            }
        }
    }

    #[test]
    fn programs_cover_the_main_constructs() {
        let cfg = GenConfig::default();
        let mut all = String::new();
        for i in 0..400 {
            let p = program(&mut Rng::for_input(3, i), &cfg);
            all.push_str(&String::from_utf8_lossy(&p));
        }
        for needle in [
            "fn main()",
            "let ",
            "if ",
            "else",
            "return ",
            "data ",
            "view ",
            "style",
            "extern js",
            "app ",
            "enum ",
            "{",
            "\\u{",
            "~",
            "on ",
            "where ",
        ] {
            assert!(
                all.contains(needle),
                "no generated input contains {needle:?}"
            );
        }
    }

    #[test]
    fn every_twist_kind_runs_and_changes_the_input() {
        for i in 0..300 {
            let mut rng = Rng::for_input(9, i);
            let mut bytes = b"fn main()\n  print(1)\n".to_vec();
            let before = bytes.clone();
            twist(&mut rng, &mut bytes);
            assert_ne!(bytes, before, "iteration {i}");
        }
    }
}
