//! Values and thunks.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

use neki_hash::StorePath;

use crate::ast::{ExprRef, LambdaDef};
use crate::eval::{Env, Evaluator};
use crate::{Error, Result};

pub type Attrs = BTreeMap<String, Thunk>;

/// What a string refers to in the store, so that a derivation using the
/// string depends on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextElem {
    /// A plain store path (a copied source, a fetched file).
    Path(StorePath),
    /// A `.drv` file and all its outputs (`drvPath`).
    Drv(StorePath),
    /// One output of a derivation (`outPath`): the `.drv` and the output name.
    Output(StorePath, String),
}

pub type Context = BTreeSet<ContextElem>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A string plus the store paths it refers to ("string context").
    String {
        s: String,
        context: Context,
    },
    Path(PathBuf),
    List(Rc<Vec<Thunk>>),
    Attrs(Rc<Attrs>),
    Lambda(Rc<Lambda>),
    PrimOp(Rc<PrimOpApp>),
}

impl Value {
    /// A string with no context: it refers to no store paths.
    pub fn string(s: impl Into<String>) -> Self {
        Value::String {
            s: s.into(),
            context: Context::new(),
        }
    }

    /// The type's name as error messages and `builtins.typeOf` show it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String { .. } => "string",
            Value::Path(_) => "path",
            Value::List(_) => "list",
            Value::Attrs(_) => "set",
            Value::Lambda(_) | Value::PrimOp(_) => "lambda",
        }
    }

    /// An attribute set holding `map`.
    pub fn attrs(map: Attrs) -> Self {
        Value::Attrs(Rc::new(map))
    }

    /// A list of `items`, which stay unevaluated until used.
    pub fn list(items: Vec<Thunk>) -> Self {
        Value::List(Rc::new(items))
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::String { s, .. } => write!(f, "{s:?}"),
            Value::Path(p) => write!(f, "{}", p.display()),
            Value::List(items) => write!(f, "[ {} items ]", items.len()),
            Value::Attrs(attrs) => write!(f, "{{ {} attributes }}", attrs.len()),
            Value::Lambda(_) | Value::PrimOp(_) => write!(f, "<lambda>"),
        }
    }
}

/// A value that may not be evaluated yet. Clones share the evaluation.
#[derive(Clone)]
pub struct Thunk(Rc<RefCell<State>>);

pub(crate) type LazyFn = Box<dyn FnOnce(&Evaluator) -> Result<Value>>;

pub(crate) enum State {
    Expr(ExprRef, Env),
    Lazy(LazyFn),
    Busy,
    Done(Value),
    /// A lazy computation that failed (it can't be retried): the message and
    /// whether it was a catchable `throw`.
    Failed(String, bool),
}

impl Thunk {
    /// A thunk that is already evaluated to `v`.
    pub fn value(v: Value) -> Self {
        Thunk(Rc::new(RefCell::new(State::Done(v))))
    }

    pub(crate) fn expr(e: ExprRef, env: Env) -> Self {
        Thunk(Rc::new(RefCell::new(State::Expr(e, env))))
    }

    pub(crate) fn lazy(f: impl FnOnce(&Evaluator) -> Result<Value> + 'static) -> Self {
        Thunk(Rc::new(RefCell::new(State::Lazy(Box::new(f)))))
    }

    /// The value, if it has been computed.
    pub fn get(&self) -> Option<Value> {
        match &*self.0.borrow() {
            State::Done(v) => Some(v.clone()),
            _ => None,
        }
    }

    pub(crate) fn force(&self, ev: &Evaluator) -> Result<Value> {
        let state = std::mem::replace(&mut *self.0.borrow_mut(), State::Busy);
        let result = match state {
            State::Done(v) => {
                *self.0.borrow_mut() = State::Done(v.clone());
                return Ok(v);
            }
            State::Busy => {
                return Err(Error::Eval("infinite recursion encountered".into()));
            }
            State::Failed(message, throw) => {
                *self.0.borrow_mut() = State::Failed(message.clone(), throw);
                return Err(if throw {
                    Error::Throw(message)
                } else {
                    Error::Eval(message)
                });
            }
            State::Expr(e, env) => {
                let saved = (e.clone(), env.clone());
                let r = ev.eval(&e, &env);
                if r.is_err() {
                    // Leave it re-evaluable, so a caught error (tryEval) can repeat.
                    *self.0.borrow_mut() = State::Expr(saved.0, saved.1);
                }
                r
            }
            State::Lazy(f) => {
                let r = f(ev);
                if let Err(err) = &r {
                    let (message, throw) = match err {
                        Error::Throw(m) => (m.clone(), true),
                        Error::Eval(m) => (m.clone(), false),
                        other => (other.to_string(), false),
                    };
                    *self.0.borrow_mut() = State::Failed(message, throw);
                }
                r
            }
        };
        if let Ok(v) = &result {
            *self.0.borrow_mut() = State::Done(v.clone());
        }
        result
    }
}

impl fmt::Debug for Thunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Some(v) => v.fmt(f),
            None => write!(f, "<thunk>"),
        }
    }
}

/// A function value with its captured environment.
pub struct Lambda {
    pub(crate) def: Rc<LambdaDef>,
    pub(crate) env: Env,
}

impl fmt::Debug for Lambda {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<lambda at {}>", self.def.pos)
    }
}

/// A builtin applied to fewer arguments than it takes.
pub struct PrimOpApp {
    pub(crate) op: &'static crate::builtins::PrimOp,
    pub(crate) args: Vec<Thunk>,
}

impl fmt::Debug for PrimOpApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<primop {}>", self.op.name)
    }
}
