//! The `derivation` builtin.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use neki_derivation::{Derivation, DerivationOutput};
use neki_hash::Hash;

use crate::eval::Evaluator;
use crate::value::*;
use crate::{Error, Result};

fn fail(message: impl Into<String>) -> Error {
    Error::Eval(message.into())
}

fn pos() -> crate::ast::Pos {
    crate::ast::Pos {
        file: Rc::from("«derivation»"),
        line: 0,
        col: 0,
    }
}

/// `derivation { name; system; builder; args ? []; outputs ? ["out"]; … }`.
/// Every other attribute becomes an environment variable of the builder.
/// `outputHash` (with `outputHashAlgo` unless it's SRI, and
/// `outputHashMode = "recursive"` for a tree) makes a fixed-output
/// derivation.
pub fn derivation(ev: &Evaluator, args: &[Thunk]) -> Result<Value> {
    let attrs = match args[0].force(ev)? {
        Value::Attrs(a) => a,
        v => return Err(fail(format!("expected a set, got a {}", v.type_name()))),
    };
    let store = ev.store()?.clone();
    let store_dir = store.store_dir().clone();

    let mut context = Context::new();
    let mut env = BTreeMap::new();
    let mut drv_args = Vec::new();
    let mut outputs = vec!["out".to_string()];
    for (key, thunk) in attrs.iter() {
        let v = thunk.force(ev)?;
        match key.as_str() {
            "args" => {
                let Value::List(items) = &v else {
                    return Err(fail("`args` must be a list"));
                };
                for item in items.iter() {
                    let v = item.force(ev)?;
                    drv_args.push(ev.coerce(&v, &mut context, true, false, &pos())?);
                }
                continue;
            }
            "outputs" => {
                let Value::List(items) = &v else {
                    return Err(fail("`outputs` must be a list"));
                };
                outputs = items
                    .iter()
                    .map(|t| match t.force(ev)? {
                        Value::String { s, .. } => Ok(s),
                        v => Err(fail(format!(
                            "output names must be strings, got a {}",
                            v.type_name()
                        ))),
                    })
                    .collect::<Result<_>>()?;
                if outputs.is_empty() {
                    return Err(fail("`outputs` must not be empty"));
                }
            }
            _ => {}
        }
        env.insert(
            key.clone(),
            ev.coerce(&v, &mut context, true, true, &pos())?,
        );
    }
    let name = env
        .get("name")
        .cloned()
        .ok_or_else(|| fail("derivation has no `name`"))?;
    let need = |k: &str| {
        env.get(k)
            .cloned()
            .ok_or_else(|| fail(format!("derivation `{name}` has no `{k}`")))
    };
    let system = need("system")?;
    let builder = need("builder")?;

    let mut drv_outputs = BTreeMap::new();
    if let Some(hash) = env.get("outputHash") {
        if outputs != ["out"] {
            return Err(fail(
                "a fixed-output derivation must have only the `out` output",
            ));
        }
        let hash = match env.get("outputHashAlgo").filter(|a| !a.is_empty()) {
            Some(algo) if !hash.contains('-') => Hash::parse(&format!("{algo}:{hash}")),
            _ => Hash::parse(hash),
        }
        .map_err(|e| fail(format!("outputHash: {e}")))?;
        let recursive = match env.get("outputHashMode").map(String::as_str) {
            None | Some("flat") => false,
            Some("recursive" | "nar") => true,
            Some(other) => return Err(fail(format!("unknown outputHashMode {other:?}"))),
        };
        drv_outputs.insert("out".into(), DerivationOutput::Fixed { hash, recursive });
    } else {
        for output in &outputs {
            drv_outputs.insert(output.clone(), DerivationOutput::Deferred);
        }
    }

    let mut input_drvs: BTreeMap<_, BTreeSet<String>> = BTreeMap::new();
    let mut input_srcs = BTreeSet::new();
    for elem in context {
        match elem {
            ContextElem::Path(p) => {
                input_srcs.insert(p);
            }
            ContextElem::Output(drv, output) => {
                input_drvs.entry(drv).or_default().insert(output);
            }
            ContextElem::Drv(drv) => {
                // The whole derivation: its file and every output.
                let d = store.read_derivation(&drv).map_err(fail)?;
                input_drvs
                    .entry(drv.clone())
                    .or_default()
                    .extend(d.outputs.keys().cloned());
                input_srcs.insert(drv);
            }
        }
    }

    let mut drv = Derivation {
        outputs: drv_outputs,
        input_drvs,
        input_srcs,
        system,
        builder,
        args: drv_args,
        env,
    };
    let resolve_store = store.clone();
    drv.finalize(&store_dir, &|p| {
        resolve_store
            .read_derivation(p)
            .map_err(neki_derivation::Error::Invalid)
    })
    .map_err(|e| fail(e.to_string()))?;
    let drv_path = store.add_derivation(&drv).map_err(fail)?;
    let out_paths = drv
        .output_paths(&store_dir)
        .map_err(|e| fail(e.to_string()))?;

    // Like Nix's `derivation` wrapper: the result is the first output's set;
    // every output's set has all attributes, its own `outPath`, and the
    // other outputs by name.
    let drv_path_value = Value::String {
        s: store_dir.to_path(&drv_path).display().to_string(),
        context: Context::from([ContextElem::Drv(drv_path.clone())]),
    };
    let common: Rc<RefCell<Attrs>> = Rc::default();
    let mut output_sets = BTreeMap::new();
    for output in &outputs {
        let out_path = Value::String {
            s: store_dir.to_path(&out_paths[output]).display().to_string(),
            context: Context::from([ContextElem::Output(drv_path.clone(), output.clone())]),
        };
        let (common, output) = (common.clone(), output.clone());
        output_sets.insert(
            output.clone(),
            Thunk::lazy(move |_| {
                let mut set = common.borrow().clone();
                set.insert("outPath".into(), Thunk::value(out_path));
                set.insert("outputName".into(), Thunk::value(Value::string(output)));
                Ok(Value::attrs(set))
            }),
        );
    }
    {
        let mut c = common.borrow_mut();
        *c = (*attrs).clone();
        c.extend(output_sets.iter().map(|(k, v)| (k.clone(), v.clone())));
        c.insert(
            "all".into(),
            Thunk::value(Value::list(output_sets.values().cloned().collect())),
        );
        c.insert("drvAttrs".into(), Thunk::value(Value::Attrs(attrs.clone())));
        c.insert("drvPath".into(), Thunk::value(drv_path_value));
        c.insert("type".into(), Thunk::value(Value::string("derivation")));
    }
    output_sets[&outputs[0]].force(ev)
}
