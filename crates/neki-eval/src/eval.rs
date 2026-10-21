//! Evaluation of expressions.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use neki_hash::StorePath;

use crate::ast::*;
use crate::value::*;
use crate::{Error, EvalStore, Result};

pub type Env = Rc<Scope>;

pub struct Scope {
    kind: ScopeKind,
    parent: Option<Env>,
}

enum ScopeKind {
    Vars(RefCell<HashMap<String, Thunk>>),
    /// `with e;`: names looked up in `e` after all lexical scopes.
    With(Thunk),
}

impl Scope {
    fn vars(vars: HashMap<String, Thunk>, parent: Option<Env>) -> Env {
        Rc::new(Scope {
            kind: ScopeKind::Vars(RefCell::new(vars)),
            parent,
        })
    }

    pub(crate) fn new_root(vars: HashMap<String, Thunk>) -> Env {
        Scope::vars(vars, None)
    }

    fn set(&self, name: String, value: Thunk) {
        if let ScopeKind::Vars(vars) = &self.kind {
            vars.borrow_mut().insert(name, value);
        }
    }
}

pub(crate) fn err(pos: &Pos, message: impl std::fmt::Display) -> Error {
    Error::Eval(format!("{pos}: {message}"))
}

pub struct Evaluator {
    /// Directories searched for `<name>` lookups.
    pub search_path: Vec<PathBuf>,
    pub(crate) store: Option<Rc<dyn EvalStore>>,
    /// `builtins.currentSystem`.
    pub system: String,
    /// Pure evaluation (the default): the result depends only on the files
    /// read, so `builtins.getEnv` gives `""` and fetches must be pinned by a
    /// hash or a commit.
    pub pure: bool,
    pub(crate) base: Env,
    imports: RefCell<HashMap<PathBuf, Value>>,
    copied: RefCell<HashMap<PathBuf, StorePath>>,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    /// An evaluator without a store: `derivation` and copying paths fail.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// An evaluator that writes derivations, copied paths and fetches to
    /// `store`.
    pub fn with_store(store: Rc<dyn EvalStore>) -> Self {
        Self::build(Some(store))
    }

    fn build(store: Option<Rc<dyn EvalStore>>) -> Self {
        let mut ev = Evaluator {
            search_path: Vec::new(),
            store,
            system: neki_derivation::host_system(),
            pure: true,
            base: Scope::vars(HashMap::new(), None),
            imports: RefCell::default(),
            copied: RefCell::default(),
        };
        ev.base = crate::builtins::base_env(&ev);
        ev
    }

    /// Evaluate the file at `path`, like `import path` (a directory means its
    /// `default.neki`). Each file is evaluated once and the value reused.
    pub fn eval_file(&mut self, path: &Path) -> Result<Value> {
        self.import(path)
    }

    /// Evaluate `src`; relative paths in it resolve against `base_dir`.
    pub fn eval_str(&mut self, src: &str, base_dir: &Path) -> Result<Value> {
        let expr = crate::parser::parse(src, "«string»", base_dir).map_err(|e| Error::Parse {
            file: PathBuf::from("«string»"),
            message: format!("{}: {}", e.pos, e.message),
        })?;
        self.eval(&expr, &self.base.clone())
    }

    /// Force `value` and everything inside it.
    pub fn force_deep(&mut self, value: &Value) -> Result<Value> {
        self.deep(value)?;
        Ok(value.clone())
    }

    pub(crate) fn deep(&self, value: &Value) -> Result<()> {
        match value {
            Value::List(items) => {
                for item in items.iter() {
                    let v = item.force(self)?;
                    self.deep(&v)?;
                }
            }
            Value::Attrs(attrs) => {
                for item in attrs.values() {
                    let v = item.force(self)?;
                    self.deep(&v)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `v` fully evaluated, printed in the expression syntax.
    pub fn print(&self, v: &Value) -> Result<String> {
        display(self, v, usize::MAX)
    }

    /// `v` fully evaluated as JSON, like `builtins.toJSON` (paths are
    /// copied to the store).
    pub fn to_json(&self, v: &Value) -> Result<serde_json::Value> {
        crate::builtins::json(self, v, &mut Context::new())
    }

    /// The attribute at `path` in `v`.
    pub fn select(&self, v: &Value, path: &[&str]) -> Result<Value> {
        let mut v = v.clone();
        for name in path {
            let Value::Attrs(attrs) = &v else {
                return Err(Error::Eval(format!(
                    "cannot select `{name}` from a {}",
                    v.type_name()
                )));
            };
            let t = attrs
                .get(*name)
                .ok_or_else(|| Error::Eval(format!("attribute `{name}` missing")))?
                .clone();
            v = t.force(self)?;
        }
        Ok(v)
    }

    /// If `v` is a derivation, its `.drv` path and output paths.
    pub fn derivation_info(&self, v: &Value) -> Result<Option<(StorePath, String)>> {
        let Value::Attrs(attrs) = v else {
            return Ok(None);
        };
        if !self.is_derivation(attrs)? {
            return Ok(None);
        }
        let drv = self.select(v, &["drvPath"])?;
        let output = match self.select(v, &["outputName"]) {
            Ok(Value::String { s, .. }) => s,
            _ => "out".to_string(),
        };
        let Value::String { context, .. } = drv else {
            return Err(Error::Eval("`drvPath` is not a string".into()));
        };
        for elem in context {
            if let ContextElem::Drv(path) = elem {
                return Ok(Some((path, output)));
            }
        }
        Err(Error::Eval("`drvPath` has no derivation context".into()))
    }

    /// Force a thunk (to weak head normal form).
    pub fn force(&self, thunk: &Thunk) -> Result<Value> {
        thunk.force(self)
    }

    /// Call `f` with `arg`.
    pub fn call(&self, f: &Value, arg: Value) -> Result<Value> {
        let pos = Pos {
            file: Rc::from("«call»"),
            line: 0,
            col: 0,
        };
        self.apply(f.clone(), Thunk::value(arg), &pos)
    }

    pub(crate) fn store(&self) -> Result<&Rc<dyn EvalStore>> {
        self.store
            .as_ref()
            .ok_or_else(|| Error::Eval("this needs a store, but evaluation has none".into()))
    }

    /// Evaluate the file at `path` (`<dir>/default.neki` for a directory),
    /// once per path.
    pub(crate) fn import(&self, path: &Path) -> Result<Value> {
        let mut path = crate::parser::normalize(&std::path::absolute(path)?);
        if path.is_dir() {
            path = path.join("default.neki");
        }
        if let Some(v) = self.imports.borrow().get(&path) {
            return Ok(v.clone());
        }
        let src = std::fs::read_to_string(&path)
            .map_err(|e| Error::Eval(format!("cannot read {}: {e}", path.display())))?;
        let base = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let expr = crate::parser::parse(&src, &path.display().to_string(), &base).map_err(|e| {
            Error::Parse {
                file: path.clone(),
                message: format!("{}: {}", e.pos, e.message),
            }
        })?;
        let value = self.eval(&expr, &self.base.clone())?;
        self.imports.borrow_mut().insert(path, value.clone());
        Ok(value)
    }

    /// A thunk for `e` in `env`, sharing variables and skipping literals.
    pub(crate) fn thunk(&self, e: &ExprRef, env: &Env) -> Thunk {
        match &e.expr {
            Expr::Int(i) => Thunk::value(Value::Int(*i)),
            Expr::Float(f) => Thunk::value(Value::Float(*f)),
            Expr::Str(parts) if parts.iter().all(|p| matches!(p, StrPart::Lit(_))) => {
                let s: String = parts
                    .iter()
                    .map(|p| match p {
                        StrPart::Lit(s) => s.as_str(),
                        StrPart::Interp(_) => "",
                    })
                    .collect();
                Thunk::value(Value::string(s))
            }
            Expr::Var(name) => match self.lookup(name, env) {
                Some(t) => t,
                None => Thunk::expr(e.clone(), env.clone()),
            },
            _ => Thunk::expr(e.clone(), env.clone()),
        }
    }

    fn lookup(&self, name: &str, env: &Env) -> Option<Thunk> {
        let mut scope = Some(env);
        while let Some(s) = scope {
            if let ScopeKind::Vars(vars) = &s.kind
                && let Some(t) = vars.borrow().get(name)
            {
                return Some(t.clone());
            }
            scope = s.parent.as_ref();
        }
        None
    }

    /// Look `name` up in the `with` scopes, innermost first.
    fn lookup_with(&self, name: &str, env: &Env) -> Result<Option<Thunk>> {
        let mut scope = Some(env);
        while let Some(s) = scope {
            if let ScopeKind::With(t) = &s.kind {
                let v = t.force(self)?;
                let Value::Attrs(attrs) = v else {
                    return Err(Error::Eval(format!(
                        "`with` needs a set, got a {}",
                        v.type_name()
                    )));
                };
                if let Some(t) = attrs.get(name) {
                    return Ok(Some(t.clone()));
                }
            }
            scope = s.parent.as_ref();
        }
        Ok(None)
    }

    pub(crate) fn eval(&self, e: &ExprRef, env: &Env) -> Result<Value> {
        let pos = &e.pos;
        match &e.expr {
            Expr::Int(i) => Ok(Value::Int(*i)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Path(p) => Ok(Value::Path(p.clone())),
            Expr::SearchPath(name) => self.search(name, pos),
            Expr::Str(parts) => {
                let mut out = String::new();
                let mut context = Context::new();
                for part in parts {
                    match part {
                        StrPart::Lit(s) => out.push_str(s),
                        StrPart::Interp(e) => {
                            let v = self.eval(e, env)?;
                            out.push_str(&self.coerce(&v, &mut context, true, false, &e.pos)?);
                        }
                    }
                }
                Ok(Value::String { s: out, context })
            }
            Expr::Var(name) => {
                if let Some(t) = self.lookup(name, env) {
                    return t.force(self);
                }
                match self.lookup_with(name, env)? {
                    Some(t) => t.force(self),
                    None => Err(err(pos, format!("undefined variable `{name}`"))),
                }
            }
            Expr::Select(base, path, default) => {
                let mut v = self.eval(base, env)?;
                for key in path {
                    let name = self.key(key, env)?;
                    let next = match &v {
                        Value::Attrs(attrs) => attrs.get(&name).cloned(),
                        _ if default.is_some() => None,
                        other => {
                            return Err(err(
                                pos,
                                format!("cannot select `{name}` from a {}", other.type_name()),
                            ));
                        }
                    };
                    match next {
                        Some(t) => v = t.force(self)?,
                        None => {
                            return match default {
                                Some(d) => self.eval(d, env),
                                None => Err(err(pos, format!("attribute `{name}` missing"))),
                            };
                        }
                    }
                }
                Ok(v)
            }
            Expr::HasAttr(base, path) => {
                let mut v = self.eval(base, env)?;
                for key in path {
                    let name = self.key(key, env)?;
                    let Value::Attrs(attrs) = &v else {
                        return Ok(Value::Bool(false));
                    };
                    match attrs.get(&name) {
                        Some(t) => v = t.force(self)?,
                        None => return Ok(Value::Bool(false)),
                    }
                }
                Ok(Value::Bool(true))
            }
            Expr::Attrs(def) => {
                if def.rec {
                    let scope = Scope::vars(HashMap::new(), Some(env.clone()));
                    Ok(Value::Attrs(Rc::new(self.bind(def, env, &scope, true)?)))
                } else {
                    Ok(Value::Attrs(Rc::new(self.bind(def, env, env, false)?)))
                }
            }
            Expr::Let(def, body) => {
                let scope = Scope::vars(HashMap::new(), Some(env.clone()));
                self.bind(def, env, &scope, true)?;
                self.eval(body, &scope)
            }
            Expr::List(items) => Ok(Value::list(
                items.iter().map(|item| self.thunk(item, env)).collect(),
            )),
            Expr::Lambda(def) => Ok(Value::Lambda(Rc::new(Lambda {
                def: def.clone(),
                env: env.clone(),
            }))),
            Expr::Apply(f, arg) => {
                let f = self.eval(f, env)?;
                self.apply(f, self.thunk(arg, env), pos)
            }
            Expr::If(cond, then, otherwise) => {
                if self.bool(&self.eval(cond, env)?, &cond.pos)? {
                    self.eval(then, env)
                } else {
                    self.eval(otherwise, env)
                }
            }
            Expr::With(scope, body) => {
                let scope = Rc::new(Scope {
                    kind: ScopeKind::With(self.thunk(scope, env)),
                    parent: Some(env.clone()),
                });
                self.eval(body, &scope)
            }
            Expr::Assert(cond, body) => {
                if self.bool(&self.eval(cond, env)?, &cond.pos)? {
                    self.eval(body, env)
                } else {
                    Err(Error::Throw(format!("{}: assertion failed", cond.pos)))
                }
            }
            Expr::Not(e) => Ok(Value::Bool(!self.bool(&self.eval(e, env)?, &e.pos)?)),
            Expr::Neg(e) => match self.eval(e, env)? {
                Value::Int(i) => i
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or_else(|| err(pos, "integer overflow")),
                Value::Float(f) => Ok(Value::Float(-f)),
                other => Err(err(pos, format!("cannot negate a {}", other.type_name()))),
            },
            Expr::BinOp(op, l, r) => self.binop(*op, l, r, env, pos),
        }
    }

    /// Fill `target`'s scope (for `rec`/`let`) and return the set's
    /// attributes. `outer` is where `inherit x;` looks.
    fn bind(&self, def: &AttrsDef, outer: &Env, target: &Env, rec: bool) -> Result<Attrs> {
        let value_env = if rec { target } else { outer };
        let sources: Vec<Thunk> = def
            .inherit_from
            .iter()
            .map(|e| self.thunk(e, value_env))
            .collect();
        let mut attrs = BTreeMap::new();
        for (name, value, pos) in &def.statics {
            let thunk = match value {
                // In `rec`, a variable may name a binding not added yet, so
                // look it up when forced rather than now.
                AttrValue::Expr(e) if rec && matches!(e.expr, Expr::Var(_)) => {
                    Thunk::expr(e.clone(), value_env.clone())
                }
                AttrValue::Expr(e) => self.thunk(e, value_env),
                AttrValue::Inherit(var) => match self.lookup(var, outer) {
                    Some(t) => t,
                    None => {
                        let (var, outer, pos) = (var.clone(), outer.clone(), pos.clone());
                        Thunk::lazy(move |ev| match ev.lookup_with(&var, &outer)? {
                            Some(t) => t.force(ev),
                            None => Err(err(&pos, format!("undefined variable `{var}`"))),
                        })
                    }
                },
                AttrValue::InheritFrom(i, attr) => {
                    let (source, attr, pos) = (sources[*i].clone(), attr.clone(), pos.clone());
                    Thunk::lazy(move |ev| match source.force(ev)? {
                        Value::Attrs(a) => match a.get(&attr) {
                            Some(t) => t.force(ev),
                            None => Err(err(&pos, format!("attribute `{attr}` missing"))),
                        },
                        other => Err(err(
                            &pos,
                            format!("cannot inherit from a {}", other.type_name()),
                        )),
                    })
                }
            };
            if rec {
                target.set(name.clone(), thunk.clone());
            }
            attrs.insert(name.clone(), thunk);
        }
        for (key, value, pos) in &def.dynamics {
            let name = match self.eval(key, outer)? {
                Value::Null => continue,
                v => self.coerce(&v, &mut Context::new(), false, false, pos)?,
            };
            if attrs.contains_key(&name) {
                return Err(err(pos, format!("attribute `{name}` is already defined")));
            }
            attrs.insert(name, self.thunk(value, outer));
        }
        Ok(attrs)
    }

    fn key(&self, key: &Key, env: &Env) -> Result<String> {
        match key {
            Key::Static(s) => Ok(s.clone()),
            Key::Dynamic(e) => {
                let v = self.eval(e, env)?;
                self.coerce(&v, &mut Context::new(), false, false, &e.pos)
            }
        }
    }

    fn search(&self, name: &str, pos: &Pos) -> Result<Value> {
        for dir in &self.search_path {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(Value::Path(candidate));
            }
        }
        Err(err(
            pos,
            format!("<{name}> was not found in the search path"),
        ))
    }

    pub(crate) fn apply(&self, f: Value, arg: Thunk, pos: &Pos) -> Result<Value> {
        match f {
            Value::Lambda(lambda) => self.call_lambda(&lambda, arg),
            Value::PrimOp(app) => {
                let mut args = app.args.clone();
                args.push(arg);
                if args.len() < app.op.arity {
                    return Ok(Value::PrimOp(Rc::new(PrimOpApp { op: app.op, args })));
                }
                (app.op.f)(self, &args).map_err(|e| match e {
                    Error::Eval(m) if !m.starts_with(&*pos.file) => {
                        Error::Eval(format!("{pos}: {}: {m}", app.op.name))
                    }
                    e => e,
                })
            }
            Value::Attrs(attrs) if attrs.contains_key("__functor") => {
                let functor = attrs["__functor"].force(self)?;
                let partial = self.apply(functor, Thunk::value(Value::Attrs(attrs)), pos)?;
                self.apply(partial, arg, pos)
            }
            other => Err(err(pos, format!("attempt to call a {}", other.type_name()))),
        }
    }

    fn call_lambda(&self, lambda: &Lambda, arg: Thunk) -> Result<Value> {
        let def = &lambda.def;
        let scope = match &def.param {
            Param::Ident(name) => Scope::vars(
                HashMap::from([(name.clone(), arg)]),
                Some(lambda.env.clone()),
            ),
            Param::Pattern {
                fields,
                ellipsis,
                bind,
            } => {
                let v = arg.force(self)?;
                let Value::Attrs(given) = v else {
                    return Err(err(
                        &def.pos,
                        format!("function expects a set, got a {}", v.type_name()),
                    ));
                };
                let scope = Scope::vars(HashMap::new(), Some(lambda.env.clone()));
                if let Some(bind) = bind {
                    scope.set(bind.clone(), arg);
                }
                for (name, default) in fields {
                    let value = match (given.get(name), default) {
                        (Some(t), _) => t.clone(),
                        // Defaults may refer to arguments not bound yet.
                        (None, Some(d)) => Thunk::expr(d.clone(), scope.clone()),
                        (None, None) => {
                            return Err(err(
                                &def.pos,
                                format!("function called without required argument `{name}`"),
                            ));
                        }
                    };
                    scope.set(name.clone(), value);
                }
                if !ellipsis
                    && let Some(extra) = given.keys().find(|k| !fields.iter().any(|(n, _)| n == *k))
                {
                    return Err(err(
                        &def.pos,
                        format!("function called with unexpected argument `{extra}`"),
                    ));
                }
                scope
            }
        };
        self.eval(&def.body, &scope)
    }

    pub(crate) fn bool(&self, v: &Value, pos: &Pos) -> Result<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            other => Err(err(
                pos,
                format!("expected a bool, got a {}", other.type_name()),
            )),
        }
    }

    fn binop(&self, op: BinOp, l: &ExprRef, r: &ExprRef, env: &Env, pos: &Pos) -> Result<Value> {
        match op {
            BinOp::And => {
                return Ok(Value::Bool(
                    self.bool(&self.eval(l, env)?, &l.pos)?
                        && self.bool(&self.eval(r, env)?, &r.pos)?,
                ));
            }
            BinOp::Or => {
                return Ok(Value::Bool(
                    self.bool(&self.eval(l, env)?, &l.pos)?
                        || self.bool(&self.eval(r, env)?, &r.pos)?,
                ));
            }
            BinOp::Impl => {
                return Ok(Value::Bool(
                    !self.bool(&self.eval(l, env)?, &l.pos)?
                        || self.bool(&self.eval(r, env)?, &r.pos)?,
                ));
            }
            _ => {}
        }
        let (a, b) = (self.eval(l, env)?, self.eval(r, env)?);
        self.binop_values(op, a, b, pos)
    }

    pub(crate) fn binop_values(&self, op: BinOp, a: Value, b: Value, pos: &Pos) -> Result<Value> {
        let type_error = |a: &Value, b: &Value| {
            err(
                pos,
                format!(
                    "cannot apply `{op:?}` to a {} and a {}",
                    a.type_name(),
                    b.type_name()
                ),
            )
        };
        Ok(match op {
            BinOp::Eq => Value::Bool(self.equal(&a, &b)?),
            BinOp::Ne => Value::Bool(!self.equal(&a, &b)?),
            BinOp::Lt => Value::Bool(self.compare(&a, &b, pos)? == Ordering::Less),
            BinOp::Le => Value::Bool(self.compare(&a, &b, pos)? != Ordering::Greater),
            BinOp::Gt => Value::Bool(self.compare(&a, &b, pos)? == Ordering::Greater),
            BinOp::Ge => Value::Bool(self.compare(&a, &b, pos)? != Ordering::Less),
            BinOp::Update => match (a, b) {
                (Value::Attrs(x), Value::Attrs(y)) => {
                    if y.is_empty() {
                        Value::Attrs(x)
                    } else if x.is_empty() {
                        Value::Attrs(y)
                    } else {
                        let mut merged = (*x).clone();
                        merged.extend(y.iter().map(|(k, v)| (k.clone(), v.clone())));
                        Value::attrs(merged)
                    }
                }
                (a, b) => return Err(type_error(&a, &b)),
            },
            BinOp::Concat => match (a, b) {
                (Value::List(x), Value::List(y)) => {
                    Value::list(x.iter().chain(y.iter()).cloned().collect())
                }
                (a, b) => return Err(type_error(&a, &b)),
            },
            BinOp::Add => match (a, b) {
                (
                    Value::String {
                        s: x,
                        context: mut cx,
                    },
                    b,
                ) => {
                    let mut out = x;
                    match &b {
                        Value::String { .. } | Value::Path(_) => {
                            out.push_str(&self.coerce(&b, &mut cx, true, false, pos)?);
                        }
                        _ => return Err(type_error(&Value::string(out), &b)),
                    }
                    Value::String {
                        s: out,
                        context: cx,
                    }
                }
                (Value::Path(p), Value::String { s, context }) => {
                    if !context.is_empty() {
                        return Err(err(
                            pos,
                            "a string that refers to store paths can't be added to a path",
                        ));
                    }
                    Value::Path(crate::parser::normalize(Path::new(&format!(
                        "{}{s}",
                        p.display()
                    ))))
                }
                (Value::Path(p), Value::Path(q)) => Value::Path(crate::parser::normalize(
                    Path::new(&format!("{}{}", p.display(), q.display())),
                )),
                (a, b) => self.arith(op, a, b, pos)?,
            },
            BinOp::Sub | BinOp::Mul | BinOp::Div => self.arith(op, a, b, pos)?,
            BinOp::And | BinOp::Or | BinOp::Impl => unreachable!("handled lazily"),
        })
    }

    fn arith(&self, op: BinOp, a: Value, b: Value, pos: &Pos) -> Result<Value> {
        let overflow = || err(pos, "integer overflow");
        Ok(match (a, b) {
            (Value::Int(x), Value::Int(y)) => Value::Int(match op {
                BinOp::Add => x.checked_add(y).ok_or_else(overflow)?,
                BinOp::Sub => x.checked_sub(y).ok_or_else(overflow)?,
                BinOp::Mul => x.checked_mul(y).ok_or_else(overflow)?,
                BinOp::Div => {
                    if y == 0 {
                        return Err(err(pos, "division by zero"));
                    }
                    x.checked_div(y).ok_or_else(overflow)?
                }
                _ => unreachable!(),
            }),
            (Value::Int(x), Value::Float(y)) => float_op(op, x as f64, y, pos)?,
            (Value::Float(x), Value::Int(y)) => float_op(op, x, y as f64, pos)?,
            (Value::Float(x), Value::Float(y)) => float_op(op, x, y, pos)?,
            (a, b) => {
                return Err(err(
                    pos,
                    format!(
                        "cannot apply `{op:?}` to a {} and a {}",
                        a.type_name(),
                        b.type_name()
                    ),
                ));
            }
        })
    }

    pub(crate) fn equal(&self, a: &Value, b: &Value) -> Result<bool> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
                (*x as f64) == *y
            }
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::String { s: x, .. }, Value::String { s: y, .. }) => x == y,
            (Value::Path(x), Value::Path(y)) => x == y,
            (Value::List(x), Value::List(y)) => {
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (a, b) in x.iter().zip(y.iter()) {
                    if !self.equal(&a.force(self)?, &b.force(self)?)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Attrs(x), Value::Attrs(y)) => {
                if Rc::ptr_eq(x, y) {
                    return Ok(true);
                }
                // Derivations are equal if their outputs are.
                if self.is_derivation(x)?
                    && self.is_derivation(y)?
                    && let (Some(p), Some(q)) = (x.get("outPath"), y.get("outPath"))
                {
                    return self.equal(&p.force(self)?, &q.force(self)?);
                }
                if x.len() != y.len() || !x.keys().eq(y.keys()) {
                    return Ok(false);
                }
                for (a, b) in x.values().zip(y.values()) {
                    if !self.equal(&a.force(self)?, &b.force(self)?)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => false,
        })
    }

    pub(crate) fn is_derivation(&self, attrs: &Attrs) -> Result<bool> {
        Ok(match attrs.get("type") {
            Some(t) => matches!(t.force(self)?, Value::String { s, .. } if s == "derivation"),
            None => false,
        })
    }

    pub(crate) fn compare(&self, a: &Value, b: &Value, pos: &Pos) -> Result<Ordering> {
        Ok(match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Int(x), Value::Float(y)) => (*x as f64).total_cmp(y),
            (Value::Float(x), Value::Int(y)) => x.total_cmp(&(*y as f64)),
            (Value::Float(x), Value::Float(y)) => x.total_cmp(y),
            (Value::String { s: x, .. }, Value::String { s: y, .. }) => x.cmp(y),
            (Value::Path(x), Value::Path(y)) => x.cmp(y),
            (Value::List(x), Value::List(y)) => {
                for (a, b) in x.iter().zip(y.iter()) {
                    match self.compare(&a.force(self)?, &b.force(self)?, pos)? {
                        Ordering::Equal => {}
                        other => return Ok(other),
                    }
                }
                x.len().cmp(&y.len())
            }
            (a, b) => {
                return Err(err(
                    pos,
                    format!(
                        "cannot compare a {} with a {}",
                        a.type_name(),
                        b.type_name()
                    ),
                ));
            }
        })
    }

    /// Turn `v` into a string, adding what it refers to to `context`.
    /// `copy`: paths are copied to the store (interpolation) instead of used
    /// as they are. `more`: also numbers, booleans, null and lists, as
    /// `toString` does.
    pub(crate) fn coerce(
        &self,
        v: &Value,
        context: &mut Context,
        copy: bool,
        more: bool,
        pos: &Pos,
    ) -> Result<String> {
        match v {
            Value::String { s, context: c } => {
                context.extend(c.iter().cloned());
                Ok(s.clone())
            }
            Value::Path(p) if copy => {
                let path = self.copy_path(p)?;
                let store = self.store()?;
                context.insert(ContextElem::Path(path.clone()));
                Ok(store.store_dir().to_path(&path).display().to_string())
            }
            Value::Path(p) => Ok(p.display().to_string()),
            Value::Attrs(attrs) => {
                if let Some(f) = attrs.get("__toString") {
                    let f = f.force(self)?;
                    let s = self.apply(f, Thunk::value(v.clone()), pos)?;
                    return self.coerce(&s, context, copy, more, pos);
                }
                match attrs.get("outPath") {
                    Some(out) => self.coerce(&out.force(self)?, context, copy, more, pos),
                    None => Err(err(pos, "cannot coerce a set to a string")),
                }
            }
            Value::Int(i) if more => Ok(i.to_string()),
            Value::Float(f) if more => Ok(format!("{f:.6}")),
            Value::Bool(b) if more => Ok(if *b { "1".into() } else { String::new() }),
            Value::Null if more => Ok(String::new()),
            Value::List(items) if more => {
                let mut parts = Vec::new();
                for item in items.iter() {
                    let v = item.force(self)?;
                    parts.push(self.coerce(&v, context, copy, more, pos)?);
                }
                Ok(parts.join(" "))
            }
            other => Err(err(
                pos,
                format!("cannot coerce a {} to a string", other.type_name()),
            )),
        }
    }

    /// Copy a source path to the store once per evaluation.
    pub(crate) fn copy_path(&self, p: &Path) -> Result<StorePath> {
        if let Some(path) = self.copied.borrow().get(p) {
            return Ok(path.clone());
        }
        let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            Error::Eval(format!("{}: can't copy a path without a name", p.display()))
        })?;
        let path = self
            .store()?
            .add_path(name, p, None)
            .map_err(|e| Error::Eval(format!("copying {} to the store: {e}", p.display())))?;
        self.copied
            .borrow_mut()
            .insert(p.to_path_buf(), path.clone());
        Ok(path)
    }
}

fn float_op(op: BinOp, x: f64, y: f64, pos: &Pos) -> Result<Value> {
    Ok(Value::Float(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y == 0.0 {
                return Err(err(pos, "division by zero"));
            }
            x / y
        }
        _ => unreachable!(),
    }))
}

/// Print `v` in the expression syntax, forcing everything up to `depth`
/// levels deep (deeper values print as `…`).
pub(crate) fn display(ev: &Evaluator, v: &Value, depth: usize) -> Result<String> {
    Ok(match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String { s, .. } => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '$' => out.push_str("\\$"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Path(p) => p.display().to_string(),
        Value::Lambda(_) | Value::PrimOp(_) => "«lambda»".into(),
        _ if depth == 0 => "…".into(),
        Value::List(items) => {
            let mut out = String::from("[");
            for item in items.iter() {
                out.push(' ');
                out.push_str(&display(ev, &item.force(ev)?, depth - 1)?);
            }
            out.push_str(" ]");
            out
        }
        Value::Attrs(attrs) => {
            if ev.is_derivation(attrs)?
                && let Some(drv) = attrs.get("drvPath")
                && let Value::String { s, .. } = drv.force(ev)?
            {
                return Ok(format!("«derivation {s}»"));
            }
            let mut out = String::from("{");
            for (k, t) in attrs.iter() {
                let simple = k
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && k.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_'-".contains(c));
                out.push(' ');
                if simple {
                    out.push_str(k);
                } else {
                    out.push_str(&display(ev, &Value::string(k.clone()), 1)?);
                }
                out.push_str(" = ");
                out.push_str(&display(ev, &t.force(ev)?, depth - 1)?);
                out.push(';');
            }
            out.push_str(" }");
            out
        }
    })
}
