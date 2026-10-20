//! Syntax tree of the expression language.

use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

/// A position in a source file, for error messages.
#[derive(Debug, Clone)]
pub struct Pos {
    pub file: Rc<str>,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

pub type ExprRef = Rc<Spanned>;

#[derive(Debug)]
pub struct Spanned {
    pub expr: Expr,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(Vec<StrPart>),
    /// An absolute path (relative literals are resolved when parsing).
    Path(PathBuf),
    /// `<name/sub>`, looked up in the search path.
    SearchPath(String),
    Var(String),
    /// `e.a.b`, with an optional `or` default.
    Select(ExprRef, Vec<Key>, Option<ExprRef>),
    /// `e ? a.b`
    HasAttr(ExprRef, Vec<Key>),
    Attrs(Rc<AttrsDef>),
    Let(Rc<AttrsDef>, ExprRef),
    List(Vec<ExprRef>),
    Lambda(Rc<LambdaDef>),
    Apply(ExprRef, ExprRef),
    If(ExprRef, ExprRef, ExprRef),
    With(ExprRef, ExprRef),
    Assert(ExprRef, ExprRef),
    BinOp(BinOp, ExprRef, ExprRef),
    Not(ExprRef),
    Neg(ExprRef),
}

#[derive(Debug)]
pub enum StrPart {
    Lit(String),
    Interp(ExprRef),
}

#[derive(Debug)]
pub enum Key {
    Static(String),
    /// `${e}` or a string with interpolation.
    Dynamic(ExprRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Concat,
    Mul,
    Div,
    Add,
    Sub,
    Update,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Impl,
}

/// The bindings of an attribute set or `let`, with nested attribute paths
/// (`a.b = 1;`) already turned into nested sets.
#[derive(Debug, Default)]
pub struct AttrsDef {
    pub rec: bool,
    pub statics: Vec<(String, AttrValue, Pos)>,
    /// `${e} = v;` (not allowed in `let` or `rec`).
    pub dynamics: Vec<(ExprRef, ExprRef, Pos)>,
    /// The `e` of each `inherit (e) …;`, evaluated once per set.
    pub inherit_from: Vec<ExprRef>,
}

#[derive(Debug)]
pub enum AttrValue {
    Expr(ExprRef),
    /// `inherit x;`: `x` from the enclosing scope (not the set itself).
    Inherit(String),
    /// `inherit (e) x;`: `e.x`, by index into `inherit_from`.
    InheritFrom(usize, String),
}

#[derive(Debug)]
pub struct LambdaDef {
    pub param: Param,
    pub body: ExprRef,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum Param {
    Ident(String),
    Pattern {
        fields: Vec<(String, Option<ExprRef>)>,
        ellipsis: bool,
        bind: Option<String>,
    },
}
