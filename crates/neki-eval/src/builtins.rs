//! Built-in functions and constants.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use neki_hash::{Hash, HashAlgo};

use crate::ast::Pos;
use crate::derivation::derivation;
use crate::eval::{Env, Evaluator, Scope};
use crate::value::*;
use crate::{Error, FetchRequest, Result};

pub struct PrimOp {
    pub name: &'static str,
    pub arity: usize,
    pub f: fn(&Evaluator, &[Thunk]) -> Result<Value>,
}

macro_rules! primops {
    ($($name:literal $arity:literal $f:ident),* $(,)?) => {
        &[$(PrimOp { name: $name, arity: $arity, f: $f }),*]
    };
}

/// Every builtin, available as `builtins.<name>`.
static PRIMOPS: &[PrimOp] = primops![
    "abort" 1 abort, "add" 2 add, "addErrorContext" 2 add_error_context, "all" 2 all,
    "any" 2 any, "attrNames" 1 attr_names, "attrValues" 1 attr_values,
    "baseNameOf" 1 base_name_of, "bitAnd" 2 bit_and, "bitOr" 2 bit_or, "bitXor" 2 bit_xor,
    "catAttrs" 2 cat_attrs, "ceil" 1 ceil, "compareVersions" 2 compare_versions,
    "concatLists" 1 concat_lists, "concatMap" 2 concat_map,
    "concatStringsSep" 2 concat_strings_sep, "deepSeq" 2 deep_seq,
    "derivation" 1 derivation, "dirOf" 1 dir_of, "div" 2 div,
    "elem" 2 elem, "elemAt" 2 elem_at, "fetchGit" 1 fetch_git, "fetchTarball" 1 fetch_tarball,
    "fetchurl" 1 fetchurl, "filter" 2 filter, "filterSource" 2 filter_source, "floor" 1 floor,
    "foldl'" 3 foldl, "fromJSON" 1 from_json, "fromTOML" 1 from_toml, "functionArgs" 1 function_args,
    "genList" 2 gen_list, "getAttr" 2 get_attr, "getEnv" 1 get_env, "groupBy" 2 group_by,
    "hasAttr" 2 has_attr, "hasContext" 1 has_context, "hashFile" 2 hash_file,
    "hashString" 2 hash_string, "head" 1 head, "import" 1 import,
    "intersectAttrs" 2 intersect_attrs, "isAttrs" 1 is_attrs, "isBool" 1 is_bool,
    "isFloat" 1 is_float, "isFunction" 1 is_function, "isInt" 1 is_int, "isList" 1 is_list,
    "isNull" 1 is_null, "isPath" 1 is_path, "isString" 1 is_string, "length" 1 length,
    "lessThan" 2 less_than, "listToAttrs" 1 list_to_attrs, "map" 2 map, "mapAttrs" 2 map_attrs,
    "match" 2 regex_match, "mul" 2 mul, "parseDrvName" 1 parse_drv_name,
    "partition" 2 partition, "path" 1 path, "pathExists" 1 path_exists, "readDir" 1 read_dir,
    "readFile" 1 read_file, "removeAttrs" 2 remove_attrs, "replaceStrings" 3 replace_strings,
    "seq" 2 seq, "sort" 2 sort, "split" 2 split, "splitVersion" 1 split_version,
    "stringLength" 1 string_length, "sub" 2 sub, "substring" 3 substring, "tail" 1 tail,
    "throw" 1 throw, "toFile" 2 to_file, "toJSON" 1 to_json, "toString" 1 to_string,
    "trace" 2 trace, "tryEval" 1 try_eval, "typeOf" 1 type_of,
    "unsafeDiscardStringContext" 1 discard_context, "zipAttrsWith" 2 zip_attrs_with,
];

/// Names also available without `builtins.`.
const GLOBAL: &[&str] = &[
    "abort",
    "baseNameOf",
    "derivation",
    "dirOf",
    "fetchTarball",
    "import",
    "isNull",
    "map",
    "removeAttrs",
    "throw",
    "toString",
];

pub fn base_env(ev: &Evaluator) -> Env {
    let mut builtins: Attrs = PRIMOPS
        .iter()
        .map(|op| {
            let v = Value::PrimOp(Rc::new(PrimOpApp {
                op,
                args: Vec::new(),
            }));
            (op.name.to_string(), Thunk::value(v))
        })
        .collect();
    builtins.insert("true".into(), Thunk::value(Value::Bool(true)));
    builtins.insert("false".into(), Thunk::value(Value::Bool(false)));
    builtins.insert("null".into(), Thunk::value(Value::Null));
    builtins.insert(
        "currentSystem".into(),
        Thunk::value(Value::string(ev.system.clone())),
    );
    builtins.insert(
        "nekiVersion".into(),
        Thunk::value(Value::string(env!("CARGO_PKG_VERSION"))),
    );
    builtins.insert("langVersion".into(), Thunk::value(Value::Int(6)));
    if let Some(store) = &ev.store {
        let dir = store.store_dir().path().display().to_string();
        builtins.insert("storeDir".into(), Thunk::value(Value::string(dir)));
    }
    let mut vars: HashMap<String, Thunk> = GLOBAL
        .iter()
        .map(|name| (name.to_string(), builtins[*name].clone()))
        .collect();
    for name in ["true", "false", "null"] {
        vars.insert(name.into(), builtins[name].clone());
    }
    let builtins = Thunk::value(Value::attrs(builtins.clone()));
    // `builtins.builtins` exists in Nix too.
    if let Some(Value::Attrs(attrs)) = builtins.get() {
        let mut with_self = (*attrs).clone();
        with_self.insert("builtins".into(), builtins.clone());
        vars.insert("builtins".into(), Thunk::value(Value::attrs(with_self)));
    }
    Scope::new_root(vars)
}

// ----- argument helpers -----

fn fail(message: impl Into<String>) -> Error {
    Error::Eval(message.into())
}

fn pos() -> Pos {
    Pos {
        file: Rc::from("«builtin»"),
        line: 0,
        col: 0,
    }
}

fn force(ev: &Evaluator, t: &Thunk) -> Result<Value> {
    t.force(ev)
}

fn int(ev: &Evaluator, t: &Thunk) -> Result<i64> {
    match force(ev, t)? {
        Value::Int(i) => Ok(i),
        v => Err(fail(format!("expected an int, got a {}", v.type_name()))),
    }
}

fn number(ev: &Evaluator, t: &Thunk) -> Result<f64> {
    match force(ev, t)? {
        Value::Int(i) => Ok(i as f64),
        Value::Float(f) => Ok(f),
        v => Err(fail(format!("expected a number, got a {}", v.type_name()))),
    }
}

/// A string argument (not coerced), with its context.
fn string(ev: &Evaluator, t: &Thunk) -> Result<(String, Context)> {
    match force(ev, t)? {
        Value::String { s, context } => Ok((s, context)),
        v => Err(fail(format!("expected a string, got a {}", v.type_name()))),
    }
}

/// An argument coerced like string interpolation (paths copied).
fn coerced(ev: &Evaluator, t: &Thunk) -> Result<(String, Context)> {
    let v = force(ev, t)?;
    let mut context = Context::new();
    let s = ev.coerce(&v, &mut context, true, false, &pos())?;
    Ok((s, context))
}

/// A path argument: a path value, or a string naming an absolute path.
fn path_arg(ev: &Evaluator, t: &Thunk) -> Result<PathBuf> {
    match force(ev, t)? {
        Value::Path(p) => Ok(p),
        Value::String { s, .. } if Path::new(&s).is_absolute() => Ok(PathBuf::from(s)),
        v => Err(fail(format!("expected a path, got a {}", v.type_name()))),
    }
}

fn list(ev: &Evaluator, t: &Thunk) -> Result<Rc<Vec<Thunk>>> {
    match force(ev, t)? {
        Value::List(items) => Ok(items),
        v => Err(fail(format!("expected a list, got a {}", v.type_name()))),
    }
}

fn attrs(ev: &Evaluator, t: &Thunk) -> Result<Rc<Attrs>> {
    match force(ev, t)? {
        Value::Attrs(a) => Ok(a),
        v => Err(fail(format!("expected a set, got a {}", v.type_name()))),
    }
}

fn call(ev: &Evaluator, f: &Value, arg: Thunk) -> Result<Value> {
    ev.apply(f.clone(), arg, &pos())
}

fn call2(ev: &Evaluator, f: &Value, a: Thunk, b: Thunk) -> Result<Value> {
    let partial = call(ev, f, a)?;
    call(ev, &partial, b)
}

fn str_value(s: String, context: Context) -> Value {
    Value::String { s, context }
}

/// An optional attribute of a set argument.
fn opt_attr(ev: &Evaluator, a: &Attrs, name: &str) -> Result<Option<Value>> {
    a.get(name).map(|t| force(ev, t)).transpose()
}

// ----- builtins -----

fn abort(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Err(Error::Abort(coerced(ev, &args[0])?.0))
}

fn throw(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Err(Error::Throw(coerced(ev, &args[0])?.0))
}

fn add_error_context(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    force(ev, &args[1])
}

fn add(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    ev.binop_values(
        crate::ast::BinOp::Add,
        force(ev, &args[0])?,
        force(ev, &args[1])?,
        &pos(),
    )
}

fn sub(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    ev.binop_values(
        crate::ast::BinOp::Sub,
        force(ev, &args[0])?,
        force(ev, &args[1])?,
        &pos(),
    )
}

fn mul(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    ev.binop_values(
        crate::ast::BinOp::Mul,
        force(ev, &args[0])?,
        force(ev, &args[1])?,
        &pos(),
    )
}

fn div(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    ev.binop_values(
        crate::ast::BinOp::Div,
        force(ev, &args[0])?,
        force(ev, &args[1])?,
        &pos(),
    )
}

fn less_than(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let ord = ev.compare(&force(ev, &args[0])?, &force(ev, &args[1])?, &pos())?;
    Ok(Value::Bool(ord == std::cmp::Ordering::Less))
}

fn bit_and(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(int(ev, &args[0])? & int(ev, &args[1])?))
}

fn bit_or(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(int(ev, &args[0])? | int(ev, &args[1])?))
}

fn bit_xor(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(int(ev, &args[0])? ^ int(ev, &args[1])?))
}

fn ceil(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(number(ev, &args[0])?.ceil() as i64))
}

fn floor(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(number(ev, &args[0])?.floor() as i64))
}

fn all(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    for item in list(ev, &args[1])?.iter() {
        if !matches!(call(ev, &f, item.clone())?, Value::Bool(true)) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn any(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    for item in list(ev, &args[1])?.iter() {
        if matches!(call(ev, &f, item.clone())?, Value::Bool(true)) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn attr_names(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::list(
        attrs(ev, &args[0])?
            .keys()
            .map(|k| Thunk::value(Value::string(k.clone())))
            .collect(),
    ))
}

fn attr_values(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::list(
        attrs(ev, &args[0])?.values().cloned().collect(),
    ))
}

fn base_name_of(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (s, context) = match force(ev, &args[0])? {
        Value::Path(p) => (p.display().to_string(), Context::new()),
        _ => coerced(ev, &args[0])?,
    };
    let trimmed = s.trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    Ok(str_value(base.to_string(), context))
}

fn dir_of(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    match force(ev, &args[0])? {
        Value::Path(p) => Ok(Value::Path(p.parent().map_or(p.clone(), Path::to_path_buf))),
        _ => {
            let (s, context) = coerced(ev, &args[0])?;
            let dir = match s.rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => s[..i].to_string(),
                None => ".".to_string(),
            };
            Ok(str_value(dir, context))
        }
    }
}

fn cat_attrs(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (name, _) = string(ev, &args[0])?;
    let mut out = Vec::new();
    for item in list(ev, &args[1])?.iter() {
        if let Some(t) = attrs(ev, item)?.get(&name) {
            out.push(t.clone());
        }
    }
    Ok(Value::list(out))
}

fn split_version_str(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '.' || c == '-' {
            i += 1;
            continue;
        }
        let digit = c.is_ascii_digit();
        let start = i;
        while i < chars.len()
            && chars[i] != '.'
            && chars[i] != '-'
            && chars[i].is_ascii_digit() == digit
        {
            i += 1;
        }
        parts.push(chars[start..i].iter().collect());
    }
    parts
}

fn component_lt(a: &str, b: &str) -> bool {
    let (na, nb) = (a.parse::<u64>().ok(), b.parse::<u64>().ok());
    match (na, nb) {
        (Some(x), Some(y)) => x < y,
        _ if a.is_empty() && nb.is_some() => true,
        _ if a == "pre" && b != "pre" => true,
        _ if b == "pre" => false,
        (_, Some(_)) => true,
        (Some(_), _) => false,
        _ => a < b,
    }
}

fn compare_versions(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (a, _) = string(ev, &args[0])?;
    let (b, _) = string(ev, &args[1])?;
    let (pa, pb) = (split_version_str(&a), split_version_str(&b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).map_or("", String::as_str);
        let y = pb.get(i).map_or("", String::as_str);
        if component_lt(x, y) {
            return Ok(Value::Int(-1));
        }
        if component_lt(y, x) {
            return Ok(Value::Int(1));
        }
    }
    Ok(Value::Int(0))
}

fn split_version(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (s, _) = string(ev, &args[0])?;
    Ok(Value::list(
        split_version_str(&s)
            .into_iter()
            .map(|p| Thunk::value(Value::string(p)))
            .collect(),
    ))
}

fn parse_drv_name(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (s, _) = string(ev, &args[0])?;
    // The version starts after the first `-` that isn't followed by a letter.
    let split = s
        .char_indices()
        .find(|&(i, c)| c == '-' && !s[i + 1..].starts_with(|n: char| n.is_alphabetic()));
    let (name, version) = match split {
        Some((i, _)) => (s[..i].to_string(), s[i + 1..].to_string()),
        None => (s.clone(), String::new()),
    };
    Ok(Value::attrs(BTreeMap::from([
        ("name".into(), Thunk::value(Value::string(name))),
        ("version".into(), Thunk::value(Value::string(version))),
    ])))
}

fn concat_lists(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let mut out = Vec::new();
    for item in list(ev, &args[0])?.iter() {
        out.extend(list(ev, item)?.iter().cloned());
    }
    Ok(Value::list(out))
}

fn concat_map(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let mut out = Vec::new();
    for item in list(ev, &args[1])?.iter() {
        match call(ev, &f, item.clone())? {
            Value::List(items) => out.extend(items.iter().cloned()),
            v => {
                return Err(fail(format!(
                    "concatMap: function returned a {}",
                    v.type_name()
                )));
            }
        }
    }
    Ok(Value::list(out))
}

fn concat_strings_sep(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (sep, mut context) = string(ev, &args[0])?;
    let mut parts = Vec::new();
    for item in list(ev, &args[1])?.iter() {
        let (s, c) = coerced(ev, item)?;
        context.extend(c);
        parts.push(s);
    }
    Ok(str_value(parts.join(&sep), context))
}

fn deep_seq(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    ev.deep(&force(ev, &args[0])?)?;
    force(ev, &args[1])
}

fn seq(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    force(ev, &args[0])?;
    force(ev, &args[1])
}

fn elem(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let x = force(ev, &args[0])?;
    for item in list(ev, &args[1])?.iter() {
        if ev.equal(&x, &force(ev, item)?)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn elem_at(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let items = list(ev, &args[0])?;
    let i = int(ev, &args[1])?;
    let item = usize::try_from(i)
        .ok()
        .and_then(|i| items.get(i))
        .ok_or_else(|| {
            fail(format!(
                "index {i} out of bounds (list has {} items)",
                items.len()
            ))
        })?;
    force(ev, item)
}

fn head(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let items = list(ev, &args[0])?;
    force(
        ev,
        items.first().ok_or_else(|| fail("head of an empty list"))?,
    )
}

fn tail(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let items = list(ev, &args[0])?;
    if items.is_empty() {
        return Err(fail("tail of an empty list"));
    }
    Ok(Value::list(items[1..].to_vec()))
}

fn length(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(list(ev, &args[0])?.len() as i64))
}

fn filter(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let mut out = Vec::new();
    for item in list(ev, &args[1])?.iter() {
        if ev.bool(&call(ev, &f, item.clone())?, &pos())? {
            out.push(item.clone());
        }
    }
    Ok(Value::list(out))
}

fn foldl(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let mut acc = args[1].clone();
    for item in list(ev, &args[2])?.iter() {
        acc = Thunk::value(call2(ev, &f, acc, item.clone())?);
    }
    force(ev, &acc)
}

fn map(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    Ok(Value::list(
        list(ev, &args[1])?
            .iter()
            .map(|item| {
                let (f, item) = (f.clone(), item.clone());
                Thunk::lazy(move |ev| call(ev, &f, item))
            })
            .collect(),
    ))
}

fn gen_list(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let n = int(ev, &args[1])?;
    if n < 0 {
        return Err(fail("genList: negative length"));
    }
    Ok(Value::list(
        (0..n)
            .map(|i| {
                let f = f.clone();
                Thunk::lazy(move |ev| call(ev, &f, Thunk::value(Value::Int(i))))
            })
            .collect(),
    ))
}

fn function_args(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    match force(ev, &args[0])? {
        Value::Lambda(l) => Ok(Value::attrs(match &l.def.param {
            crate::ast::Param::Pattern { fields, .. } => fields
                .iter()
                .map(|(name, default)| (name.clone(), Thunk::value(Value::Bool(default.is_some()))))
                .collect(),
            crate::ast::Param::Ident(_) => BTreeMap::new(),
        })),
        Value::PrimOp(_) => Ok(Value::attrs(BTreeMap::new())),
        v => Err(fail(format!(
            "expected a function, got a {}",
            v.type_name()
        ))),
    }
}

fn get_attr(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (name, _) = string(ev, &args[0])?;
    let a = attrs(ev, &args[1])?;
    force(
        ev,
        a.get(&name)
            .ok_or_else(|| fail(format!("attribute `{name}` missing")))?,
    )
}

fn has_attr(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (name, _) = string(ev, &args[0])?;
    Ok(Value::Bool(attrs(ev, &args[1])?.contains_key(&name)))
}

fn get_env(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (name, _) = string(ev, &args[0])?;
    if ev.pure {
        return Ok(Value::string(String::new()));
    }
    Ok(Value::string(std::env::var(name).unwrap_or_default()))
}

fn group_by(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let mut groups: BTreeMap<String, Vec<Thunk>> = BTreeMap::new();
    for item in list(ev, &args[1])?.iter() {
        let key = call(ev, &f, item.clone())?;
        let Value::String { s, .. } = key else {
            return Err(fail("groupBy: function must return a string"));
        };
        groups.entry(s).or_default().push(item.clone());
    }
    Ok(Value::attrs(
        groups
            .into_iter()
            .map(|(k, v)| (k, Thunk::value(Value::list(v))))
            .collect(),
    ))
}

fn has_context(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Bool(!string(ev, &args[0])?.1.is_empty()))
}

fn discard_context(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::string(string(ev, &args[0])?.0))
}

fn algo(name: &str) -> Result<HashAlgo> {
    match name {
        "sha256" => Ok(HashAlgo::Sha256),
        "sha512" => Ok(HashAlgo::Sha512),
        other => Err(fail(format!(
            "unsupported hash algorithm {other:?} (use sha256 or sha512)"
        ))),
    }
}

fn hash_string(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let algo = algo(&string(ev, &args[0])?.0)?;
    let (s, _) = string(ev, &args[1])?;
    Ok(Value::string(
        Hash::of_bytes(algo, s.as_bytes()).to_base16(),
    ))
}

fn hash_file(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let algo = algo(&string(ev, &args[0])?.0)?;
    let path = path_arg(ev, &args[1])?;
    let data = std::fs::read(&path).map_err(|e| fail(format!("{}: {e}", path.display())))?;
    Ok(Value::string(Hash::of_bytes(algo, &data).to_base16()))
}

fn import(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let path = path_arg(ev, &args[0])?;
    ev.import(&path)
}

fn intersect_attrs(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (a, b) = (attrs(ev, &args[0])?, attrs(ev, &args[1])?);
    Ok(Value::attrs(
        b.iter()
            .filter(|(k, _)| a.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    ))
}

macro_rules! is_type {
    ($($f:ident $pat:pat),*) => {$(
        fn $f(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
            Ok(Value::Bool(matches!(force(ev, &args[0])?, $pat)))
        }
    )*};
}

is_type!(
    is_attrs Value::Attrs(_), is_bool Value::Bool(_), is_float Value::Float(_),
    is_function Value::Lambda(_) | Value::PrimOp(_), is_int Value::Int(_),
    is_list Value::List(_), is_null Value::Null, is_path Value::Path(_),
    is_string Value::String { .. }
);

fn list_to_attrs(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let mut out = BTreeMap::new();
    for item in list(ev, &args[0])?.iter() {
        let a = attrs(ev, item)?;
        let name = a
            .get("name")
            .ok_or_else(|| fail("listToAttrs: element without `name`"))?;
        let (name, _) = string(ev, name)?;
        let value = a
            .get("value")
            .ok_or_else(|| fail("listToAttrs: element without `value`"))?;
        // The first occurrence wins.
        out.entry(name).or_insert_with(|| value.clone());
    }
    Ok(Value::attrs(out))
}

fn map_attrs(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    Ok(Value::attrs(
        attrs(ev, &args[1])?
            .iter()
            .map(|(k, v)| {
                let (f, name, v) = (f.clone(), k.clone(), v.clone());
                let thunk =
                    Thunk::lazy(move |ev| call2(ev, &f, Thunk::value(Value::string(name)), v));
                (k.clone(), thunk)
            })
            .collect(),
    ))
}

fn regex(pattern: &str) -> Result<regex::Regex> {
    regex::Regex::new(&format!("^(?:{pattern})$"))
        .map_err(|e| fail(format!("invalid regular expression {pattern:?}: {e}")))
}

fn regex_match(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (pattern, _) = string(ev, &args[0])?;
    let (s, _) = string(ev, &args[1])?;
    let re = regex(&pattern)?;
    Ok(match re.captures(&s) {
        None => Value::Null,
        Some(caps) => Value::list(
            caps.iter()
                .skip(1)
                .map(|m| Thunk::value(m.map_or(Value::Null, |m| Value::string(m.as_str()))))
                .collect(),
        ),
    })
}

fn split(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (pattern, _) = string(ev, &args[0])?;
    let (s, _) = string(ev, &args[1])?;
    let re = regex::Regex::new(&pattern)
        .map_err(|e| fail(format!("invalid regular expression {pattern:?}: {e}")))?;
    let mut out = Vec::new();
    let mut last = 0;
    for caps in re.captures_iter(&s) {
        let m = caps.get(0).expect("group 0 always matches");
        out.push(Thunk::value(Value::string(&s[last..m.start()])));
        out.push(Thunk::value(Value::list(
            caps.iter()
                .skip(1)
                .map(|g| Thunk::value(g.map_or(Value::Null, |g| Value::string(g.as_str()))))
                .collect(),
        )));
        last = m.end();
    }
    out.push(Thunk::value(Value::string(&s[last..])));
    Ok(Value::list(out))
}

fn partition(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let (mut right, mut wrong) = (Vec::new(), Vec::new());
    for item in list(ev, &args[1])?.iter() {
        if ev.bool(&call(ev, &f, item.clone())?, &pos())? {
            right.push(item.clone());
        } else {
            wrong.push(item.clone());
        }
    }
    Ok(Value::attrs(BTreeMap::from([
        ("right".into(), Thunk::value(Value::list(right))),
        ("wrong".into(), Thunk::value(Value::list(wrong))),
    ])))
}

/// Copy `path` to the store, keeping what `filter` (a function of the path
/// string and its type) accepts.
fn add_filtered(ev: &Evaluator, name: &str, path: &Path, filter: Option<Value>) -> Result<Value> {
    let store = ev.store()?.clone();
    let mut callback = |p: &Path, kind: &str| -> Result<bool> {
        let f = filter.as_ref().expect("only called with a filter");
        let r = call2(
            ev,
            f,
            Thunk::value(Value::string(p.display().to_string())),
            Thunk::value(Value::string(kind)),
        )?;
        ev.bool(&r, &pos())
    };
    let stored = store
        .add_path(
            name,
            path,
            filter
                .is_some()
                .then_some(&mut callback as &mut dyn FnMut(&Path, &str) -> Result<bool>),
        )
        .map_err(|e| fail(format!("copying {} to the store: {e}", path.display())))?;
    let location = store.store_dir().to_path(&stored).display().to_string();
    Ok(str_value(
        location,
        Context::from([ContextElem::Path(stored)]),
    ))
}

fn path(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let a = attrs(ev, &args[0])?;
    let p = path_arg(
        ev,
        a.get("path").ok_or_else(|| fail("path: missing `path`"))?,
    )?;
    let name = match opt_attr(ev, &a, "name")? {
        Some(Value::String { s, .. }) => s,
        _ => p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("source")
            .to_string(),
    };
    add_filtered(ev, &name, &p, opt_attr(ev, &a, "filter")?)
}

fn filter_source(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let p = path_arg(ev, &args[1])?;
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source")
        .to_string();
    add_filtered(ev, &name, &p, Some(f))
}

fn path_exists(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Bool(path_arg(ev, &args[0])?.exists()))
}

fn read_dir(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let p = path_arg(ev, &args[0])?;
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(&p).map_err(|e| fail(format!("{}: {e}", p.display())))? {
        let entry = entry.map_err(|e| fail(format!("{}: {e}", p.display())))?;
        let kind = match entry.file_type() {
            Ok(t) if t.is_symlink() => "symlink",
            Ok(t) if t.is_dir() => "directory",
            Ok(t) if t.is_file() => "regular",
            _ => "unknown",
        };
        out.insert(
            entry.file_name().to_string_lossy().into_owned(),
            Thunk::value(Value::string(kind)),
        );
    }
    Ok(Value::attrs(out))
}

fn read_file(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let p = path_arg(ev, &args[0])?;
    std::fs::read_to_string(&p)
        .map(Value::string)
        .map_err(|e| fail(format!("{}: {e}", p.display())))
}

fn remove_attrs(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let mut a = (*attrs(ev, &args[0])?).clone();
    for name in list(ev, &args[1])?.iter() {
        a.remove(&string(ev, name)?.0);
    }
    Ok(Value::attrs(a))
}

fn replace_strings(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let from: Vec<String> = list(ev, &args[0])?
        .iter()
        .map(|t| string(ev, t).map(|s| s.0))
        .collect::<Result<_>>()?;
    let to: Vec<(String, Context)> = list(ev, &args[1])?
        .iter()
        .map(|t| string(ev, t))
        .collect::<Result<_>>()?;
    if from.len() != to.len() {
        return Err(fail("replaceStrings: lists must have the same length"));
    }
    let (s, mut context) = string(ev, &args[2])?;
    let mut out = String::new();
    let mut i = 0;
    while i <= s.len() {
        let found = from.iter().position(|f| s[i..].starts_with(f.as_str()));
        match found {
            Some(k) => {
                out.push_str(&to[k].0);
                context.extend(to[k].1.iter().cloned());
                if from[k].is_empty() {
                    // An empty pattern matches between every character.
                    if let Some(c) = s[i..].chars().next() {
                        out.push(c);
                        i += c.len_utf8();
                    } else {
                        break;
                    }
                } else {
                    i += from[k].len();
                }
            }
            None => match s[i..].chars().next() {
                Some(c) => {
                    out.push(c);
                    i += c.len_utf8();
                }
                None => break,
            },
        }
    }
    Ok(str_value(out, context))
}

/// A stable merge sort that can fail, unlike `slice::sort_by`.
fn sort(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let items = list(ev, &args[1])?;
    fn merge_sort(ev: &Evaluator, f: &Value, items: &[Thunk]) -> Result<Vec<Thunk>> {
        if items.len() <= 1 {
            return Ok(items.to_vec());
        }
        let (left, right) = items.split_at(items.len() / 2);
        let (left, right) = (merge_sort(ev, f, left)?, merge_sort(ev, f, right)?);
        let mut out = Vec::with_capacity(items.len());
        let (mut i, mut j) = (0, 0);
        while i < left.len() && j < right.len() {
            // Take from the right only if it's strictly less, for stability.
            if ev.bool(&call2(ev, f, right[j].clone(), left[i].clone())?, &pos())? {
                out.push(right[j].clone());
                j += 1;
            } else {
                out.push(left[i].clone());
                i += 1;
            }
        }
        out.extend_from_slice(&left[i..]);
        out.extend_from_slice(&right[j..]);
        Ok(out)
    }
    Ok(Value::list(merge_sort(ev, &f, &items)?))
}

fn string_length(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::Int(coerced(ev, &args[0])?.0.len() as i64))
}

fn substring(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let start = int(ev, &args[0])?;
    let len = int(ev, &args[1])?;
    let (s, context) = coerced(ev, &args[2])?;
    if start < 0 {
        return Err(fail("substring: negative start"));
    }
    let start = (start as usize).min(s.len());
    let end = if len < 0 {
        s.len()
    } else {
        start.saturating_add(len as usize).min(s.len())
    };
    let slice = s
        .get(start..end)
        .ok_or_else(|| fail("substring: not at a character boundary"))?;
    Ok(str_value(slice.to_string(), context))
}

fn to_file(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (name, _) = string(ev, &args[0])?;
    let (text, context) = string(ev, &args[1])?;
    let mut refs = BTreeSet::new();
    for elem in context {
        match elem {
            ContextElem::Path(p) => {
                refs.insert(p);
            }
            _ => return Err(fail("toFile: the text can't refer to derivation outputs")),
        }
    }
    let store = ev.store()?;
    let path = store.add_text(&name, &text, &refs).map_err(fail)?;
    let location = store.store_dir().to_path(&path).display().to_string();
    Ok(str_value(
        location,
        Context::from([ContextElem::Path(path)]),
    ))
}

pub(crate) fn json(ev: &Evaluator, v: &Value, context: &mut Context) -> Result<serde_json::Value> {
    Ok(match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Int(i) => (*i).into(),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .ok_or_else(|| fail("toJSON: number is not finite"))?,
        Value::String { s, context: c } => {
            context.extend(c.iter().cloned());
            s.clone().into()
        }
        Value::Path(_) => ev.coerce(v, context, true, false, &pos())?.into(),
        Value::List(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|t| json(ev, &force(ev, t)?, context))
                .collect::<Result<_>>()?,
        ),
        Value::Attrs(a) => {
            if a.contains_key("__toString") || a.contains_key("outPath") {
                return Ok(ev.coerce(v, context, true, false, &pos())?.into());
            }
            let mut map = serde_json::Map::new();
            for (k, t) in a.iter() {
                map.insert(k.clone(), json(ev, &force(ev, t)?, context)?);
            }
            serde_json::Value::Object(map)
        }
        Value::Lambda(_) | Value::PrimOp(_) => {
            return Err(fail("toJSON: cannot convert a function"));
        }
    })
}

fn to_json(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let mut context = Context::new();
    let j = json(ev, &force(ev, &args[0])?, &mut context)?;
    Ok(str_value(j.to_string(), context))
}

pub(crate) fn from_json_value(j: serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::string(s),
        serde_json::Value::Array(items) => Value::list(
            items
                .into_iter()
                .map(|j| Thunk::value(from_json_value(j)))
                .collect(),
        ),
        serde_json::Value::Object(map) => Value::attrs(
            map.into_iter()
                .map(|(k, j)| (k, Thunk::value(from_json_value(j))))
                .collect(),
        ),
    }
}

fn from_json(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (s, _) = string(ev, &args[0])?;
    let j: serde_json::Value =
        serde_json::from_str(&s).map_err(|e| fail(format!("fromJSON: {e}")))?;
    Ok(from_json_value(j))
}

fn from_toml_value(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::string(s),
        toml::Value::Integer(i) => Value::Int(i),
        toml::Value::Float(f) => Value::Float(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::string(d.to_string()),
        toml::Value::Array(items) => Value::list(
            items
                .into_iter()
                .map(|v| Thunk::value(from_toml_value(v)))
                .collect(),
        ),
        toml::Value::Table(table) => Value::attrs(
            table
                .into_iter()
                .map(|(k, v)| (k, Thunk::value(from_toml_value(v))))
                .collect(),
        ),
    }
}

fn from_toml(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (s, _) = string(ev, &args[0])?;
    let table: toml::Table = s.parse().map_err(|e| fail(format!("fromTOML: {e}")))?;
    Ok(from_toml_value(toml::Value::Table(table)))
}

fn to_string(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let v = force(ev, &args[0])?;
    let mut context = Context::new();
    let s = ev.coerce(&v, &mut context, false, true, &pos())?;
    Ok(str_value(s, context))
}

fn trace(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let v = force(ev, &args[0])?;
    match &v {
        Value::String { s, .. } => eprintln!("trace: {s}"),
        other => eprintln!("trace: {}", crate::eval::display(ev, other, 2)?),
    }
    force(ev, &args[1])
}

fn try_eval(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (success, value) = match force(ev, &args[0]) {
        Ok(v) => (true, v),
        Err(Error::Throw(_)) => (false, Value::Bool(false)),
        Err(e) => return Err(e),
    };
    Ok(Value::attrs(BTreeMap::from([
        ("success".into(), Thunk::value(Value::Bool(success))),
        ("value".into(), Thunk::value(value)),
    ])))
}

fn type_of(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    Ok(Value::string(force(ev, &args[0])?.type_name()))
}

fn zip_attrs_with(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let f = force(ev, &args[0])?;
    let mut grouped: BTreeMap<String, Vec<Thunk>> = BTreeMap::new();
    for set in list(ev, &args[1])?.iter() {
        for (k, v) in attrs(ev, set)?.iter() {
            grouped.entry(k.clone()).or_default().push(v.clone());
        }
    }
    Ok(Value::attrs(
        grouped
            .into_iter()
            .map(|(k, values)| {
                let (f, name) = (f.clone(), k.clone());
                let thunk = Thunk::lazy(move |ev| {
                    call2(
                        ev,
                        &f,
                        Thunk::value(Value::string(name)),
                        Thunk::value(Value::list(values)),
                    )
                });
                (k, thunk)
            })
            .collect(),
    ))
}

// ----- fetchers -----

/// The URL and optional hash of a fetcher argument: a URL string, or a set
/// with `url` and `sha256` or `hash`.
fn fetch_args(ev: &Evaluator, t: &Thunk) -> Result<(String, Option<Hash>, Option<String>)> {
    match force(ev, t)? {
        Value::String { s, .. } => Ok((s, None, None)),
        Value::Attrs(a) => {
            let url = match opt_attr(ev, &a, "url")? {
                Some(Value::String { s, .. }) => s,
                _ => return Err(fail("missing `url`")),
            };
            let hash = match (opt_attr(ev, &a, "hash")?, opt_attr(ev, &a, "sha256")?) {
                (Some(Value::String { s, .. }), _) => Some(s),
                (_, Some(Value::String { s, .. })) => Some(format!("sha256:{s}")),
                _ => None,
            };
            let hash = hash
                .map(|h| Hash::parse(&h).map_err(|e| fail(e.to_string())))
                .transpose()?;
            let name = match opt_attr(ev, &a, "name")? {
                Some(Value::String { s, .. }) => Some(s),
                _ => None,
            };
            Ok((url, hash, name))
        }
        v => Err(fail(format!(
            "expected a URL or a set, got a {}",
            v.type_name()
        ))),
    }
}

fn url_name(url: &str) -> String {
    let base = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("source");
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "+-._=".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('.').to_string();
    if clean.is_empty() {
        "source".into()
    } else {
        clean
    }
}

fn fetched(ev: &Evaluator, name: &str, request: &FetchRequest) -> Result<Value> {
    let store = ev.store()?;
    let path = store.fetch(name, request).map_err(fail)?;
    let location = store.store_dir().to_path(&path).display().to_string();
    Ok(str_value(
        location,
        Context::from([ContextElem::Path(path)]),
    ))
}

/// In pure evaluation, a fetch must say what it gets.
fn pinned(ev: &Evaluator, url: &str, pinned: bool, by: &str) -> Result<()> {
    if ev.pure && !pinned {
        return Err(fail(format!(
            "{url}: pure evaluation needs {by} (or use --impure)"
        )));
    }
    Ok(())
}

fn fetchurl(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (url, hash, name) = fetch_args(ev, &args[0])?;
    pinned(ev, &url, hash.is_some(), "a `hash` or `sha256`")?;
    let name = name.unwrap_or_else(|| url_name(&url));
    fetched(ev, &name, &FetchRequest::Url { url, hash })
}

fn fetch_tarball(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (url, hash, name) = fetch_args(ev, &args[0])?;
    pinned(ev, &url, hash.is_some(), "a `hash` or `sha256`")?;
    fetched(
        ev,
        &name.unwrap_or_else(|| "source".into()),
        &FetchRequest::Tarball { url, hash },
    )
}

fn fetch_git(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let (url, rev, reference) = match force(ev, &args[0])? {
        Value::String { s, .. } => (s, None, None),
        Value::Attrs(a) => {
            let get = |name: &str| -> Result<Option<String>> {
                Ok(match opt_attr(ev, &a, name)? {
                    Some(Value::String { s, .. }) => Some(s),
                    _ => None,
                })
            };
            (
                get("url")?.ok_or_else(|| fail("fetchGit: missing `url`"))?,
                get("rev")?,
                get("ref")?,
            )
        }
        v => Err(fail(format!(
            "expected a URL or a set, got a {}",
            v.type_name()
        )))?,
    };
    pinned(ev, &url, rev.is_some(), "a `rev`")?;
    let out = fetched(
        ev,
        "source",
        &FetchRequest::Git {
            url,
            rev: rev.clone(),
            reference,
        },
    )?;
    let mut set = BTreeMap::from([("outPath".to_string(), Thunk::value(out))]);
    if let Some(rev) = rev {
        set.insert("rev".into(), Thunk::value(Value::string(rev)));
    }
    Ok(Value::attrs(set))
}
