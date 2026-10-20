//! Lexer and recursive-descent parser.

use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::ast::*;

pub struct ParseError {
    pub pos: Pos,
    pub message: String,
}

type PResult<T> = Result<T, ParseError>;

/// Parse `src` (from `file`, used in positions); relative path literals
/// resolve against `base_dir`.
pub fn parse(src: &str, file: &str, base_dir: &Path) -> PResult<ExprRef> {
    let mut p = Parser {
        src,
        pos: 0,
        file: Rc::from(file),
        base_dir: base_dir.to_path_buf(),
        line_starts: std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| i + 1))
            .collect(),
    };
    let expr = p.expr()?;
    match p.next()? {
        (Tok::Eof, _) => Ok(expr),
        (tok, start) => Err(p.error_at(start, format!("unexpected {}", tok.describe()))),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Int(i64),
    Float(f64),
    Ident(String),
    Path(String),
    HomePath(String),
    SearchPath(String),
    Let,
    In,
    Rec,
    With,
    Inherit,
    If,
    Then,
    Else,
    Assert,
    Or,
    Ellipsis,
    Dot,
    EqEq,
    NotEq,
    Le,
    Ge,
    Lt,
    Gt,
    AndAnd,
    OrOr,
    Impl,
    Update,
    Concat,
    Plus,
    Minus,
    Star,
    Slash,
    Not,
    Question,
    At,
    Colon,
    Semi,
    Comma,
    Assign,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    DollarBrace,
    Quote,
    IndQuote,
    Eof,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Eof => "end of file".into(),
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::Int(i) => format!("number {i}"),
            Tok::Float(f) => format!("number {f}"),
            Tok::Path(p) | Tok::HomePath(p) => format!("path {p}"),
            Tok::SearchPath(p) => format!("<{p}>"),
            other => format!("`{}`", other.text()),
        }
    }

    fn text(&self) -> &'static str {
        match self {
            Tok::Let => "let",
            Tok::In => "in",
            Tok::Rec => "rec",
            Tok::With => "with",
            Tok::Inherit => "inherit",
            Tok::If => "if",
            Tok::Then => "then",
            Tok::Else => "else",
            Tok::Assert => "assert",
            Tok::Or => "or",
            Tok::Ellipsis => "...",
            Tok::Dot => ".",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Le => "<=",
            Tok::Ge => ">=",
            Tok::Lt => "<",
            Tok::Gt => ">",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Impl => "->",
            Tok::Update => "//",
            Tok::Concat => "++",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Not => "!",
            Tok::Question => "?",
            Tok::At => "@",
            Tok::Colon => ":",
            Tok::Semi => ";",
            Tok::Comma => ",",
            Tok::Assign => "=",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBrack => "[",
            Tok::RBrack => "]",
            Tok::DollarBrace => "${",
            Tok::Quote => "\"",
            Tok::IndQuote => "''",
            _ => "?",
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'\'' | b'-')
}

fn is_path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+')
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    file: Rc<str>,
    base_dir: PathBuf,
    line_starts: Vec<usize>,
}

/// One character of an indented string, before indentation is stripped.
enum IndAtom {
    /// `escaped` characters (from `''$` etc.) never count as indentation.
    Char {
        c: char,
        escaped: bool,
    },
    Interp(ExprRef),
}

impl<'a> Parser<'a> {
    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    fn pos_at(&self, offset: usize) -> Pos {
        let line = self.line_starts.partition_point(|&s| s <= offset);
        let start = self.line_starts[line - 1];
        Pos {
            file: self.file.clone(),
            line: line as u32,
            col: (self.src[start..offset].chars().count() + 1) as u32,
        }
    }

    fn error_at(&self, offset: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            pos: self.pos_at(offset),
            message: message.into(),
        }
    }

    fn node(&self, start: usize, expr: Expr) -> ExprRef {
        Rc::new(Spanned {
            expr,
            pos: self.pos_at(start),
        })
    }

    fn skip_trivia(&mut self) -> PResult<()> {
        loop {
            let b = self.bytes();
            match b.get(self.pos) {
                Some(c) if c.is_ascii_whitespace() => self.pos += 1,
                Some(b'#') => {
                    while self.pos < b.len() && b[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                Some(b'/') if b.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    match self.src[self.pos + 2..].find("*/") {
                        Some(end) => self.pos += 2 + end + 2,
                        None => return Err(self.error_at(start, "unterminated comment")),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// The next token and where it starts, consuming it.
    fn next(&mut self) -> PResult<(Tok, usize)> {
        self.skip_trivia()?;
        let start = self.pos;
        let tok = self.lex()?;
        Ok((tok, start))
    }

    fn peek(&mut self) -> PResult<Tok> {
        let saved = self.pos;
        let (tok, _) = self.next()?;
        self.pos = saved;
        Ok(tok)
    }

    fn eat(&mut self, tok: &Tok) -> PResult<bool> {
        let saved = self.pos;
        if self.next()?.0 == *tok {
            Ok(true)
        } else {
            self.pos = saved;
            Ok(false)
        }
    }

    fn expect(&mut self, tok: Tok) -> PResult<usize> {
        let (got, start) = self.next()?;
        if got == tok {
            Ok(start)
        } else {
            Err(self.error_at(
                start,
                format!("expected {}, found {}", tok.describe(), got.describe()),
            ))
        }
    }

    fn lex(&mut self) -> PResult<Tok> {
        let b = self.bytes();
        let start = self.pos;
        let Some(&c) = b.get(start) else {
            return Ok(Tok::Eof);
        };
        let at = |i: usize| b.get(start + i).copied().unwrap_or(0);

        // Paths: a run of path characters containing a `/` followed by one.
        if is_path_char(c) || c == b'/' || (c == b'~' && at(1) == b'/') {
            let mut end = start + usize::from(c == b'~');
            let mut has_slash = false;
            loop {
                while end < b.len() && is_path_char(b[end]) {
                    end += 1;
                }
                if end + 1 < b.len() && b[end] == b'/' && is_path_char(b[end + 1]) {
                    has_slash = true;
                    end += 1;
                } else {
                    break;
                }
            }
            if has_slash {
                if b.get(end) == Some(&b'$') && b.get(end + 1) == Some(&b'{') {
                    return Err(self.error_at(start, "interpolation in paths is not supported"));
                }
                self.pos = end;
                let text = self.src[start..end].to_string();
                return Ok(if c == b'~' {
                    Tok::HomePath(text)
                } else {
                    Tok::Path(text)
                });
            }
        }
        if is_ident_start(c) {
            let mut end = start + 1;
            while end < b.len() && is_ident_char(b[end]) {
                end += 1;
            }
            self.pos = end;
            let word = &self.src[start..end];
            return Ok(match word {
                "let" => Tok::Let,
                "in" => Tok::In,
                "rec" => Tok::Rec,
                "with" => Tok::With,
                "inherit" => Tok::Inherit,
                "if" => Tok::If,
                "then" => Tok::Then,
                "else" => Tok::Else,
                "assert" => Tok::Assert,
                "or" => Tok::Or,
                _ => Tok::Ident(word.to_string()),
            });
        }
        if c.is_ascii_digit() || (c == b'.' && at(1).is_ascii_digit()) {
            let mut end = start;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
            let mut float = false;
            if end < b.len() && b[end] == b'.' && b.get(end + 1).is_some_and(u8::is_ascii_digit) {
                float = true;
                end += 1;
                while end < b.len() && b[end].is_ascii_digit() {
                    end += 1;
                }
            }
            if float && end < b.len() && (b[end] == b'e' || b[end] == b'E') {
                let mut e = end + 1;
                if e < b.len() && (b[e] == b'+' || b[e] == b'-') {
                    e += 1;
                }
                if e < b.len() && b[e].is_ascii_digit() {
                    end = e;
                    while end < b.len() && b[end].is_ascii_digit() {
                        end += 1;
                    }
                }
            }
            self.pos = end;
            let text = &self.src[start..end];
            return if float {
                text.parse()
                    .map(Tok::Float)
                    .map_err(|_| self.error_at(start, "invalid number"))
            } else {
                text.parse()
                    .map(Tok::Int)
                    .map_err(|_| self.error_at(start, "integer is too large"))
            };
        }
        if c == b'<' {
            let mut end = start + 1;
            while end < b.len() && (is_path_char(b[end]) || b[end] == b'/') {
                end += 1;
            }
            if end > start + 1 && b.get(end) == Some(&b'>') {
                self.pos = end + 1;
                return Ok(Tok::SearchPath(self.src[start + 1..end].to_string()));
            }
        }
        let (tok, len) = match (c, at(1), at(2)) {
            (b'.', b'.', b'.') => (Tok::Ellipsis, 3),
            (b'=', b'=', _) => (Tok::EqEq, 2),
            (b'!', b'=', _) => (Tok::NotEq, 2),
            (b'<', b'=', _) => (Tok::Le, 2),
            (b'>', b'=', _) => (Tok::Ge, 2),
            (b'&', b'&', _) => (Tok::AndAnd, 2),
            (b'|', b'|', _) => (Tok::OrOr, 2),
            (b'-', b'>', _) => (Tok::Impl, 2),
            (b'/', b'/', _) => (Tok::Update, 2),
            (b'+', b'+', _) => (Tok::Concat, 2),
            (b'$', b'{', _) => (Tok::DollarBrace, 2),
            (b'\'', b'\'', _) => (Tok::IndQuote, 2),
            (b'.', _, _) => (Tok::Dot, 1),
            (b'<', _, _) => (Tok::Lt, 1),
            (b'>', _, _) => (Tok::Gt, 1),
            (b'+', _, _) => (Tok::Plus, 1),
            (b'-', _, _) => (Tok::Minus, 1),
            (b'*', _, _) => (Tok::Star, 1),
            (b'/', _, _) => (Tok::Slash, 1),
            (b'!', _, _) => (Tok::Not, 1),
            (b'?', _, _) => (Tok::Question, 1),
            (b'@', _, _) => (Tok::At, 1),
            (b':', _, _) => (Tok::Colon, 1),
            (b';', _, _) => (Tok::Semi, 1),
            (b',', _, _) => (Tok::Comma, 1),
            (b'=', _, _) => (Tok::Assign, 1),
            (b'(', _, _) => (Tok::LParen, 1),
            (b')', _, _) => (Tok::RParen, 1),
            (b'{', _, _) => (Tok::LBrace, 1),
            (b'}', _, _) => (Tok::RBrace, 1),
            (b'[', _, _) => (Tok::LBrack, 1),
            (b']', _, _) => (Tok::RBrack, 1),
            (b'"', _, _) => (Tok::Quote, 1),
            _ => {
                let c = self.src[start..].chars().next().unwrap_or('?');
                return Err(self.error_at(start, format!("unexpected character {c:?}")));
            }
        };
        self.pos = start + len;
        Ok(tok)
    }

    // ----- expressions, lowest precedence first -----

    fn expr(&mut self) -> PResult<ExprRef> {
        self.skip_trivia()?;
        let start = self.pos;
        match self.peek()? {
            Tok::Let => {
                self.next()?;
                let def = self.bindings(true, Tok::In)?;
                let body = self.expr()?;
                Ok(self.node(start, Expr::Let(Rc::new(def), body)))
            }
            Tok::With => {
                self.next()?;
                let scope = self.expr()?;
                self.expect(Tok::Semi)?;
                let body = self.expr()?;
                Ok(self.node(start, Expr::With(scope, body)))
            }
            Tok::Assert => {
                self.next()?;
                let cond = self.expr()?;
                self.expect(Tok::Semi)?;
                let body = self.expr()?;
                Ok(self.node(start, Expr::Assert(cond, body)))
            }
            Tok::If => {
                self.next()?;
                let cond = self.expr()?;
                self.expect(Tok::Then)?;
                let then = self.expr()?;
                self.expect(Tok::Else)?;
                let otherwise = self.expr()?;
                Ok(self.node(start, Expr::If(cond, then, otherwise)))
            }
            Tok::Ident(name) => {
                self.next()?;
                if self.eat(&Tok::Colon)? {
                    let body = self.expr()?;
                    return Ok(self.lambda(start, Param::Ident(name), body));
                }
                if self.eat(&Tok::At)? {
                    self.expect(Tok::LBrace)?;
                    let param = self.pattern(Some(name))?;
                    self.expect(Tok::Colon)?;
                    let body = self.expr()?;
                    return Ok(self.lambda(start, param, body));
                }
                self.pos = start;
                self.op_expr(0)
            }
            Tok::LBrace => {
                // A pattern if it parses as one and is followed by `:` or `@`.
                self.next()?;
                if let Ok(mut param) = self.pattern(None) {
                    if self.eat(&Tok::Colon)? {
                        let body = self.expr()?;
                        return Ok(self.lambda(start, param, body));
                    }
                    if self.eat(&Tok::At)? {
                        let (tok, at) = self.next()?;
                        let Tok::Ident(name) = tok else {
                            return Err(self.error_at(at, "expected a name after `@`"));
                        };
                        if let Param::Pattern { bind, .. } = &mut param {
                            *bind = Some(name);
                        }
                        self.expect(Tok::Colon)?;
                        let body = self.expr()?;
                        return Ok(self.lambda(start, param, body));
                    }
                }
                self.pos = start;
                self.op_expr(0)
            }
            _ => self.op_expr(0),
        }
    }

    fn lambda(&self, start: usize, param: Param, body: ExprRef) -> ExprRef {
        self.node(
            start,
            Expr::Lambda(Rc::new(LambdaDef {
                param,
                body,
                pos: self.pos_at(start),
            })),
        )
    }

    /// `{ a, b ? default, ... }` after the `{`.
    fn pattern(&mut self, bind: Option<String>) -> PResult<Param> {
        let mut fields = Vec::new();
        let mut ellipsis = false;
        loop {
            let (tok, at) = self.next()?;
            match tok {
                Tok::RBrace => break,
                Tok::Ellipsis => {
                    ellipsis = true;
                    self.expect(Tok::RBrace)?;
                    break;
                }
                Tok::Ident(name) => {
                    let default = if self.eat(&Tok::Question)? {
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    if fields.iter().any(|(n, _)| *n == name) {
                        return Err(self.error_at(at, format!("duplicate argument `{name}`")));
                    }
                    fields.push((name, default));
                    if !self.eat(&Tok::Comma)? {
                        self.expect(Tok::RBrace)?;
                        break;
                    }
                }
                other => {
                    return Err(self.error_at(at, format!("unexpected {}", other.describe())));
                }
            }
        }
        Ok(Param::Pattern {
            fields,
            ellipsis,
            bind,
        })
    }

    /// Binary operators by precedence climbing; `level` is the lowest
    /// level allowed.
    fn op_expr(&mut self, level: u8) -> PResult<ExprRef> {
        // (level, operator, right-associative)
        fn info(tok: &Tok) -> Option<(u8, BinOp, bool)> {
            Some(match tok {
                Tok::Impl => (1, BinOp::Impl, true),
                Tok::OrOr => (2, BinOp::Or, false),
                Tok::AndAnd => (3, BinOp::And, false),
                Tok::EqEq => (4, BinOp::Eq, false),
                Tok::NotEq => (4, BinOp::Ne, false),
                Tok::Lt => (5, BinOp::Lt, false),
                Tok::Le => (5, BinOp::Le, false),
                Tok::Gt => (5, BinOp::Gt, false),
                Tok::Ge => (5, BinOp::Ge, false),
                Tok::Update => (6, BinOp::Update, true),
                Tok::Plus => (8, BinOp::Add, false),
                Tok::Minus => (8, BinOp::Sub, false),
                Tok::Star => (9, BinOp::Mul, false),
                Tok::Slash => (9, BinOp::Div, false),
                Tok::Concat => (10, BinOp::Concat, true),
                _ => return None,
            })
        }
        self.skip_trivia()?;
        let start = self.pos;
        let mut left = if level <= 7 && self.peek()? == Tok::Not {
            // `!` binds looser than arithmetic: `!a + b` is `!(a + b)`.
            self.next()?;
            let operand = self.op_expr(7)?;
            self.node(start, Expr::Not(operand))
        } else {
            self.unary()?
        };
        loop {
            let saved = self.pos;
            let (tok, _) = self.next()?;
            let Some((op_level, op, right)) = info(&tok) else {
                self.pos = saved;
                return Ok(left);
            };
            if op_level < level {
                self.pos = saved;
                return Ok(left);
            }
            let rhs = self.op_expr(if right { op_level } else { op_level + 1 })?;
            left = self.node(start, Expr::BinOp(op, left, rhs));
            // Comparison operators don't chain.
            if matches!(op_level, 4 | 5)
                && let Some((l, _, _)) = info(&self.peek()?)
                && l == op_level
            {
                return Err(self.error_at(self.pos, "comparison operators can't be chained"));
            }
        }
    }

    /// `?` and unary minus, above `++`.
    fn unary(&mut self) -> PResult<ExprRef> {
        self.skip_trivia()?;
        let start = self.pos;
        if self.peek()? == Tok::Minus {
            self.next()?;
            let operand = self.unary()?;
            return Ok(self.node(start, Expr::Neg(operand)));
        }
        let mut expr = self.application()?;
        while self.eat(&Tok::Question)? {
            let path = self.attr_path()?;
            expr = self.node(start, Expr::HasAttr(expr, path));
        }
        Ok(expr)
    }

    fn application(&mut self) -> PResult<ExprRef> {
        self.skip_trivia()?;
        let start = self.pos;
        let mut func = self.select()?;
        while self.starts_atom()? {
            let arg = self.select()?;
            func = self.node(start, Expr::Apply(func, arg));
        }
        Ok(func)
    }

    fn starts_atom(&mut self) -> PResult<bool> {
        Ok(matches!(
            self.peek()?,
            Tok::Ident(_)
                | Tok::Int(_)
                | Tok::Float(_)
                | Tok::Path(_)
                | Tok::HomePath(_)
                | Tok::SearchPath(_)
                | Tok::Quote
                | Tok::IndQuote
                | Tok::LParen
                | Tok::LBrace
                | Tok::LBrack
                | Tok::Rec
        ))
    }

    fn select(&mut self) -> PResult<ExprRef> {
        self.skip_trivia()?;
        let start = self.pos;
        let atom = self.atom()?;
        if !self.eat(&Tok::Dot)? {
            return Ok(atom);
        }
        let path = self.attr_path()?;
        let default = if self.eat(&Tok::Or)? {
            Some(self.select()?)
        } else {
            None
        };
        Ok(self.node(start, Expr::Select(atom, path, default)))
    }

    /// `a.b."c".${d}`
    fn attr_path(&mut self) -> PResult<Vec<Key>> {
        let mut path = vec![self.attr_key()?];
        while self.eat(&Tok::Dot)? {
            path.push(self.attr_key()?);
        }
        Ok(path)
    }

    fn attr_key(&mut self) -> PResult<Key> {
        let (tok, at) = self.next()?;
        match tok {
            Tok::Ident(name) => Ok(Key::Static(name)),
            Tok::Or => Ok(Key::Static("or".into())),
            Tok::Quote => {
                let parts = self.string_parts()?;
                match parts.as_slice() {
                    [] => Ok(Key::Static(String::new())),
                    [StrPart::Lit(s)] => Ok(Key::Static(s.clone())),
                    _ => Ok(Key::Dynamic(self.node(at, Expr::Str(parts)))),
                }
            }
            Tok::DollarBrace => {
                let expr = self.expr()?;
                self.expect(Tok::RBrace)?;
                Ok(Key::Dynamic(expr))
            }
            other => Err(self.error_at(
                at,
                format!("expected an attribute name, found {}", other.describe()),
            )),
        }
    }

    fn atom(&mut self) -> PResult<ExprRef> {
        let (tok, start) = self.next()?;
        let expr = match tok {
            Tok::Int(i) => Expr::Int(i),
            Tok::Float(f) => Expr::Float(f),
            Tok::Ident(name) => Expr::Var(name),
            Tok::Path(p) => Expr::Path(normalize(&if p.starts_with('/') {
                PathBuf::from(&p)
            } else {
                self.base_dir.join(&p)
            })),
            Tok::HomePath(p) => {
                let home = std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .ok_or_else(|| self.error_at(start, "no home directory for ~/ paths"))?;
                Expr::Path(normalize(&PathBuf::from(home).join(&p[2..])))
            }
            Tok::SearchPath(p) => Expr::SearchPath(p),
            Tok::Quote => Expr::Str(self.string_parts()?),
            Tok::IndQuote => Expr::Str(self.indented_string()?),
            Tok::LParen => {
                let inner = self.expr()?;
                self.expect(Tok::RParen)?;
                return Ok(inner);
            }
            Tok::LBrace => Expr::Attrs(Rc::new(self.bindings(false, Tok::RBrace)?)),
            Tok::Rec => {
                self.expect(Tok::LBrace)?;
                Expr::Attrs(Rc::new(self.bindings(true, Tok::RBrace)?))
            }
            Tok::LBrack => {
                let mut items = Vec::new();
                while !self.eat(&Tok::RBrack)? {
                    items.push(self.select()?);
                }
                Expr::List(items)
            }
            other => {
                return Err(self.error_at(start, format!("unexpected {}", other.describe())));
            }
        };
        Ok(self.node(start, expr))
    }

    /// Bindings up to `end` (`}` or `in`), which is consumed.
    fn bindings(&mut self, rec: bool, end: Tok) -> PResult<AttrsDef> {
        let mut def = Bindings::new(rec);
        loop {
            self.skip_trivia()?;
            let start = self.pos;
            if self.eat(&end)? {
                break;
            }
            if self.eat(&Tok::Inherit)? {
                let from = if self.eat(&Tok::LParen)? {
                    let e = self.expr()?;
                    self.expect(Tok::RParen)?;
                    def.inherit_from.push(e);
                    Some(def.inherit_from.len() - 1)
                } else {
                    None
                };
                loop {
                    let (tok, at) = self.next()?;
                    let name = match tok {
                        Tok::Semi => break,
                        Tok::Ident(name) => name,
                        Tok::Quote => match self.string_parts()?.as_slice() {
                            [StrPart::Lit(s)] => s.clone(),
                            _ => return Err(self.error_at(at, "dynamic attribute in inherit")),
                        },
                        other => {
                            return Err(
                                self.error_at(at, format!("unexpected {}", other.describe()))
                            );
                        }
                    };
                    let value = match from {
                        Some(i) => AttrValue::InheritFrom(i, name.clone()),
                        None => AttrValue::Inherit(name.clone()),
                    };
                    def.insert_value(&name, value, self.pos_at(at))
                        .map_err(|m| self.error_at(at, m))?;
                }
                continue;
            }
            let path = self.attr_path()?;
            self.expect(Tok::Assign)?;
            let value = self.expr()?;
            self.expect(Tok::Semi)?;
            def.insert_path(path, value, self.pos_at(start))
                .map_err(|m| self.error_at(start, m))?;
        }
        if (rec || end == Tok::In) && !def.dynamics.is_empty() {
            return Err(self.error_at(
                self.pos,
                "dynamic attributes are not allowed in `let` or `rec`",
            ));
        }
        Ok(def.finish())
    }

    /// The parts of a `"…"` string, after the opening quote.
    fn string_parts(&mut self) -> PResult<Vec<StrPart>> {
        let start = self.pos;
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let rest = &self.src[self.pos..];
            let Some(c) = rest.chars().next() else {
                return Err(self.error_at(start, "unterminated string"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    let Some(e) = rest[1..].chars().next() else {
                        return Err(self.error_at(start, "unterminated string"));
                    };
                    lit.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        e => e,
                    });
                    self.pos += 1 + e.len_utf8();
                }
                '$' if rest.starts_with("$$") => {
                    lit.push_str("$$");
                    self.pos += 2;
                }
                '$' if rest.starts_with("${") => {
                    self.pos += 2;
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    let e = self.expr()?;
                    self.expect(Tok::RBrace)?;
                    parts.push(StrPart::Interp(e));
                }
                c => {
                    lit.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        if !lit.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(parts)
    }

    /// The parts of a `''…''` string, after the opening quotes, with common
    /// indentation removed as Nix does.
    fn indented_string(&mut self) -> PResult<Vec<StrPart>> {
        let start = self.pos;
        let mut atoms = Vec::new();
        loop {
            let rest = &self.src[self.pos..];
            let Some(c) = rest.chars().next() else {
                return Err(self.error_at(start, "unterminated indented string"));
            };
            if rest.starts_with("'''") {
                atoms.push(IndAtom::Char {
                    c: '\'',
                    escaped: true,
                });
                atoms.push(IndAtom::Char {
                    c: '\'',
                    escaped: true,
                });
                self.pos += 3;
            } else if rest.starts_with("''$") {
                atoms.push(IndAtom::Char {
                    c: '$',
                    escaped: true,
                });
                self.pos += 3;
            } else if let Some(escaped) = rest.strip_prefix("''\\") {
                let Some(e) = escaped.chars().next() else {
                    return Err(self.error_at(start, "unterminated indented string"));
                };
                let c = match e {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    e => e,
                };
                atoms.push(IndAtom::Char { c, escaped: true });
                self.pos += 3 + e.len_utf8();
            } else if rest.starts_with("''") {
                self.pos += 2;
                break;
            } else if rest.starts_with("$$") {
                atoms.push(IndAtom::Char {
                    c: '$',
                    escaped: false,
                });
                atoms.push(IndAtom::Char {
                    c: '$',
                    escaped: false,
                });
                self.pos += 2;
            } else if rest.starts_with("${") {
                self.pos += 2;
                let e = self.expr()?;
                self.expect(Tok::RBrace)?;
                atoms.push(IndAtom::Interp(e));
            } else {
                atoms.push(IndAtom::Char { c, escaped: false });
                self.pos += c.len_utf8();
            }
        }
        Ok(strip_indentation(atoms))
    }
}

/// Nix's rules: drop the first line if it holds only spaces, remove the
/// smallest indentation of the non-blank lines from every line, and drop
/// the spaces of a last line that holds only spaces.
fn strip_indentation(atoms: Vec<IndAtom>) -> Vec<StrPart> {
    // Split into lines (each keeps its trailing newline).
    let mut lines: Vec<Vec<IndAtom>> = vec![Vec::new()];
    for atom in atoms {
        let newline = matches!(
            atom,
            IndAtom::Char {
                c: '\n',
                escaped: false
            }
        );
        lines.last_mut().expect("never empty").push(atom);
        if newline {
            lines.push(Vec::new());
        }
    }
    let leading_spaces = |line: &[IndAtom]| {
        line.iter()
            .take_while(|a| {
                matches!(
                    a,
                    IndAtom::Char {
                        c: ' ',
                        escaped: false
                    }
                )
            })
            .count()
    };
    let is_blank = |line: &[IndAtom]| {
        line.iter().all(|a| {
            matches!(
                a,
                IndAtom::Char {
                    c: ' ' | '\n',
                    escaped: false
                }
            )
        })
    };
    let min_indent = lines
        .iter()
        .filter(|line| !is_blank(line))
        .map(|line| leading_spaces(line))
        .min()
        .unwrap_or(0);
    if lines.first().is_some_and(|l| {
        is_blank(l) && l.iter().any(|a| matches!(a, IndAtom::Char { c: '\n', .. }))
    }) {
        lines.remove(0);
    }
    let last = lines.len().saturating_sub(1);
    let mut parts = Vec::new();
    let mut lit = String::new();
    for (i, line) in lines.into_iter().enumerate() {
        let blank_last = i == last && is_blank(&line);
        let skip = if blank_last {
            line.len()
        } else {
            leading_spaces(&line).min(min_indent)
        };
        for atom in line.into_iter().skip(skip) {
            match atom {
                IndAtom::Char { c, .. } => lit.push(c),
                IndAtom::Interp(e) => {
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(StrPart::Interp(e));
                }
            }
        }
    }
    if !lit.is_empty() {
        parts.push(StrPart::Lit(lit));
    }
    parts
}

/// Resolve `.` and `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Collects bindings, turning attribute paths into nested sets.
struct Bindings {
    rec: bool,
    statics: Vec<(String, Item, Pos)>,
    dynamics: Vec<(ExprRef, ExprRef, Pos)>,
    inherit_from: Vec<ExprRef>,
}

enum Item {
    Value(AttrValue),
    Nested(Bindings),
}

impl Bindings {
    fn new(rec: bool) -> Self {
        Self {
            rec,
            statics: Vec::new(),
            dynamics: Vec::new(),
            inherit_from: Vec::new(),
        }
    }

    fn insert_value(&mut self, name: &str, value: AttrValue, pos: Pos) -> Result<(), String> {
        if self.statics.iter().any(|(n, _, _)| n == name) {
            return Err(format!("attribute `{name}` is already defined"));
        }
        self.statics
            .push((name.to_string(), Item::Value(value), pos));
        Ok(())
    }

    fn insert_path(&mut self, mut path: Vec<Key>, value: ExprRef, pos: Pos) -> Result<(), String> {
        let rest = path.split_off(1);
        let key = path.pop().expect("attribute paths are never empty");
        let name = match key {
            Key::Static(name) => name,
            Key::Dynamic(key) => {
                let value = if rest.is_empty() {
                    value
                } else {
                    let mut nested = Bindings::new(false);
                    nested.insert_path(rest, value, pos.clone())?;
                    Rc::new(Spanned {
                        expr: Expr::Attrs(Rc::new(nested.finish())),
                        pos: pos.clone(),
                    })
                };
                self.dynamics.push((key, value, pos));
                return Ok(());
            }
        };
        if rest.is_empty() {
            return self.insert_value(&name, AttrValue::Expr(value), pos);
        }
        match self.statics.iter_mut().find(|(n, _, _)| *n == name) {
            Some((_, Item::Nested(nested), _)) => nested.insert_path(rest, value, pos),
            Some(_) => Err(format!("attribute `{name}` is already defined")),
            None => {
                let mut nested = Bindings::new(false);
                nested.insert_path(rest, value, pos.clone())?;
                self.statics.push((name, Item::Nested(nested), pos));
                Ok(())
            }
        }
    }

    fn finish(self) -> AttrsDef {
        AttrsDef {
            rec: self.rec,
            statics: self
                .statics
                .into_iter()
                .map(|(name, item, pos)| {
                    let value = match item {
                        Item::Value(value) => value,
                        Item::Nested(nested) => AttrValue::Expr(Rc::new(Spanned {
                            expr: Expr::Attrs(Rc::new(nested.finish())),
                            pos: pos.clone(),
                        })),
                    };
                    (name, value, pos)
                })
                .collect(),
            dynamics: self.dynamics,
            inherit_from: self.inherit_from,
        }
    }
}
