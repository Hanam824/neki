use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;

use neki_derivation::Derivation;
use neki_hash::{StoreDir, StorePath};
use neki_store::{LocalStore, Store};

use super::*;
use crate::PathFilter;

/// Evaluate `src` without a store and print the result.
fn eval(src: &str) -> String {
    let mut ev = Evaluator::new();
    let v = ev
        .eval_str(src, Path::new("/project"))
        .unwrap_or_else(|e| panic!("{src}: {e}"));
    ev.print(&v).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn eval_err(src: &str) -> String {
    let mut ev = Evaluator::new();
    match ev
        .eval_str(src, Path::new("/project"))
        .and_then(|v| ev.print(&v))
    {
        Ok(v) => panic!("{src} should fail, got {v}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn literals_and_operators() {
    for (src, expected) in [
        ("1 + 2 * 3", "7"),
        ("(1 + 2) * 3", "9"),
        ("10 / 3", "3"),
        ("10 / 4.0", "2.5"),
        ("-3 - -4", "1"),
        ("2 - 1 - 1", "0"),
        ("1 < 2 && 2 <= 2 && !(3 > 4) && 4 >= 4", "true"),
        ("true -> false", "false"),
        ("false -> x", "true"),
        ("false || true", "true"),
        ("[1 2] ++ [3]", "[ 1 2 3 ]"),
        ("{ a = 1; b = 2; } // { b = 3; }", "{ a = 1; b = 3; }"),
        ("\"a\" + \"b\"", "\"ab\""),
        ("[ 1 \"two\" null true 1.5 ]", "[ 1 \"two\" null true 1.5 ]"),
        ("1 == 1.0", "true"),
        ("[1 [2]] == [1 [2]]", "true"),
        ("{ a = 1; } == { a = 1; }", "true"),
        ("{ a = 1; } != { a = 2; }", "true"),
        ("\"abc\" < \"abd\"", "true"),
        ("!true == false", "true"),
    ] {
        assert_eq!(eval(src), expected, "{src}");
    }
    // Path values print with the OS's separator.
    let path = |s: &str| s.replace('/', std::path::MAIN_SEPARATOR_STR);
    assert_eq!(eval("/x/y + \"/z\""), path("/x/y/z"));
    assert_eq!(eval("./a/../b"), path("/project/b"));
}

#[test]
fn strings() {
    for (src, expected) in [
        (r#""a\n\t\"\\\$${"b"}""#, r#""a\n\t\"\\\$b""#),
        (r#"let x = "y"; in "x${x}x""#, r#""xyx""#),
        ("\"$${x}\"", "\"\\$\\${x}\""),
        ("''\n  one\n    two\n  ''", "\"one\\n  two\\n\""),
        ("''  a ''${b} '''x ''\\n''", "\"a \\${b} ''x \\n\""),
        ("let n = \"z\"; in ''\n  ${n}\n   y''", "\"z\\n y\""),
        ("''${\"x\"}''", "\"x\""),
        ("toString [ 1 null true \"a\" [ 2 ] ]", "\"1  1 a 2\""),
        ("toString 1.5", "\"1.500000\""),
    ] {
        assert_eq!(eval(src), expected, "{src}");
    }
}

#[test]
fn bindings_and_scopes() {
    for (src, expected) in [
        ("let a = 1; b = a + 1; in b", "2"),
        ("let x = 1; in let y = x; x = 2; in y", "2"),
        ("rec { a = 1; b = a + 1; }.b", "2"),
        (
            "{ a.b.c = 1; a.d = 2; }",
            "{ a = { b = { c = 1; }; d = 2; }; }",
        ),
        ("let x = 5; in { inherit x; }", "{ x = 5; }"),
        (
            "let s = { a = 1; b = 2; }; in { inherit (s) a b; }",
            "{ a = 1; b = 2; }",
        ),
        // `inherit` directly in `rec` reads the enclosing scope (no recursion)...
        ("let x = 1; in (rec { inherit x; }).x", "1"),
        // ...but a nested set sees the `rec` bindings.
        ("let x = 1; in rec { x = 2; y = { inherit x; }; }.y.x", "2"),
        (
            "{ ${\"dyn\" + \"amic\"} = 1; ${null} = 2; }",
            "{ dynamic = 1; }",
        ),
        ("{ \"a b\" = 1; }", "{ \"a b\" = 1; }"),
        ("with { a = 1; }; a", "1"),
        ("let a = 1; in with { a = 2; }; a", "1"),
        ("with { a = 1; }; with { a = 2; }; a", "2"),
        ("{ a = { b = 1; }; }.a.b", "1"),
        ("{ a = 1; }.b or 7", "7"),
        ("({ a = 1; }) ? a", "true"),
        ("{ a.b = 1; } ? a.b", "true"),
        ("{ a = 1; } ? a.b", "false"),
        ("if 1 > 2 then \"y\" else \"n\"", "\"n\""),
        ("assert true; 1", "1"),
    ] {
        assert_eq!(eval(src), expected, "{src}");
    }
}

#[test]
fn functions() {
    for (src, expected) in [
        ("(x: x + 1) 2", "3"),
        ("(x: y: x * y) 3 4", "12"),
        ("({ a, b ? a * 10 }: a + b) { a = 1; }", "11"),
        ("({ a, ... }: a) { a = 1; b = 2; }", "1"),
        ("(args@{ a, ... }: args.b) { a = 1; b = 2; }", "2"),
        ("({ a }@args: args.a) { a = 3; }", "3"),
        (
            "let f = n: if n == 0 then 1 else n * f (n - 1); in f 10",
            "3628800",
        ),
        (
            "let fs = { __functor = self: x: x + self.n; n = 5; }; in fs 1",
            "6",
        ),
        (
            "builtins.functionArgs ({ a, b ? 1 }: a)",
            "{ a = false; b = true; }",
        ),
        // Laziness: unused arguments and elements are never evaluated.
        ("(x: 1) (throw \"no\")", "1"),
        ("builtins.length [ (throw \"no\") ]", "1"),
        ("let x = throw \"no\"; in 2", "2"),
    ] {
        assert_eq!(eval(src), expected, "{src}");
    }
}

#[test]
fn builtins_work() {
    for (src, expected) in [
        ("map (x: x * 2) [ 1 2 3 ]", "[ 2 4 6 ]"),
        ("builtins.filter (x: x > 1) [ 1 2 3 ]", "[ 2 3 ]"),
        ("builtins.foldl' (a: b: a + b) 0 [ 1 2 3 ]", "6"),
        ("builtins.genList (i: i * i) 4", "[ 0 1 4 9 ]"),
        ("builtins.attrNames { b = 1; a = 2; }", "[ \"a\" \"b\" ]"),
        ("builtins.attrValues { b = 1; a = 2; }", "[ 2 1 ]"),
        (
            "builtins.listToAttrs [ { name = \"a\"; value = 1; } ]",
            "{ a = 1; }",
        ),
        (
            "builtins.mapAttrs (n: v: n + toString v) { a = 1; }",
            "{ a = \"a1\"; }",
        ),
        (
            "builtins.removeAttrs { a = 1; b = 2; } [ \"a\" ]",
            "{ b = 2; }",
        ),
        (
            "builtins.intersectAttrs { a = 0; } { a = 1; b = 2; }",
            "{ a = 1; }",
        ),
        (
            "builtins.catAttrs \"a\" [ { a = 1; } { b = 2; } { a = 3; } ]",
            "[ 1 3 ]",
        ),
        ("builtins.elem 2 [ 1 2 ]", "true"),
        ("builtins.elemAt [ 1 2 ] 1", "2"),
        ("builtins.head [ 1 2 ]", "1"),
        ("builtins.tail [ 1 2 ]", "[ 2 ]"),
        ("builtins.concatLists [ [ 1 ] [ 2 3 ] ]", "[ 1 2 3 ]"),
        ("builtins.concatMap (x: [ x x ]) [ 1 2 ]", "[ 1 1 2 2 ]"),
        (
            "builtins.concatStringsSep \", \" [ \"a\" \"b\" ]",
            "\"a, b\"",
        ),
        (
            "builtins.replaceStrings [ \"a\" \"bb\" ] [ \"x\" \"y\" ] \"abba\"",
            "\"xyx\"",
        ),
        ("builtins.substring 1 3 \"hello\"", "\"ell\""),
        ("builtins.substring 2 (-1) \"hello\"", "\"llo\""),
        ("builtins.stringLength \"héllo\"", "6"),
        ("builtins.match \"a(b+)c\" \"abbc\"", "[ \"bb\" ]"),
        ("builtins.match \"a\" \"ab\"", "null"),
        ("builtins.split \",\" \"a,b\"", "[ \"a\" [ ] \"b\" ]"),
        ("builtins.compareVersions \"1.2.10\" \"1.2.9\"", "1"),
        ("builtins.compareVersions \"1.0pre1\" \"1.0\"", "-1"),
        ("builtins.compareVersions \"2.3\" \"2.3\"", "0"),
        (
            "builtins.splitVersion \"1.2a-3\"",
            "[ \"1\" \"2\" \"a\" \"3\" ]",
        ),
        (
            "builtins.parseDrvName \"node-js-20.1.0\"",
            "{ name = \"node-js\"; version = \"20.1.0\"; }",
        ),
        ("builtins.sort (a: b: a < b) [ 3 1 2 ]", "[ 1 2 3 ]"),
        (
            "builtins.partition (x: x > 1) [ 1 2 3 ]",
            "{ right = [ 2 3 ]; wrong = [ 1 ]; }",
        ),
        (
            "builtins.groupBy (x: if x > 1 then \"big\" else \"small\") [ 1 2 ]",
            "{ big = [ 2 ]; small = [ 1 ]; }",
        ),
        ("builtins.any (x: x > 2) [ 1 3 ]", "true"),
        ("builtins.all (x: x > 2) [ 1 3 ]", "false"),
        ("builtins.typeOf { }", "\"set\""),
        ("builtins.isFunction (x: x)", "true"),
        (
            "builtins.toJSON { a = [ 1 \"x\" null true ]; }",
            "\"{\\\"a\\\":[1,\\\"x\\\",null,true]}\"",
        ),
        (
            "builtins.fromJSON \"{\\\"a\\\": [1, 2.5]}\"",
            "{ a = [ 1 2.5 ]; }",
        ),
        (
            r#"builtins.fromTOML "a = 1\n[[p]]\nname = \"x\"""#,
            "{ a = 1; p = [ { name = \"x\"; } ]; }",
        ),
        (
            "builtins.hashString \"sha256\" \"abc\"",
            "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"",
        ),
        (
            "builtins.tryEval (throw \"x\")",
            "{ success = false; value = false; }",
        ),
        (
            "builtins.tryEval (assert false; 1)",
            "{ success = false; value = false; }",
        ),
        ("builtins.tryEval 1", "{ success = true; value = 1; }"),
        (
            "builtins.zipAttrsWith (n: vs: vs) [ { a = 1; } { a = 2; } ]",
            "{ a = [ 1 2 ]; }",
        ),
        ("baseNameOf \"/a/b.txt\"", "\"b.txt\""),
        ("dirOf \"/a/b.txt\"", "\"/a\""),
        ("builtins.seq 1 2", "2"),
        ("builtins.bitAnd 6 3", "2"),
    ] {
        assert_eq!(eval(src), expected, "{src}");
    }
}

#[test]
fn errors_are_reported() {
    for (src, needle) in [
        ("x", "undefined variable `x`"),
        ("{ a = 1; }.b", "attribute `b` missing"),
        ("1 + \"a\"", "cannot apply"),
        ("throw \"boom\"", "boom"),
        ("abort \"stop\"", "stop"),
        ("assert 1 == 2; 3", "assertion failed"),
        ("({ a }: a) { }", "without required argument `a`"),
        ("({ a }: a) { a = 1; b = 2; }", "unexpected argument `b`"),
        ("let x = x; in x", "infinite recursion"),
        ("1 / 0", "division by zero"),
        ("builtins.head [ ]", "empty list"),
        ("9223372036854775807 + 1", "overflow"),
        ("{ a = 1; a = 2; }", "already defined"),
        ("1 +", "parse error"),
        ("\"unterminated", "unterminated string"),
        ("1 < 2 < 3", "can't be chained"),
    ] {
        let message = eval_err(src);
        assert!(message.contains(needle), "{src}: {message}");
    }
    // Positions point at the expression.
    assert!(eval_err("\n  { a = 1; }.b").contains("«string»:2:3"));
}

#[test]
fn imports_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("lib")).unwrap();
    std::fs::write(
        dir.path().join("lib/default.neki"),
        "{ double = x: x * 2; }",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.neki"),
        "let lib = import ./lib; in lib.double (import ./value.neki)",
    )
    .unwrap();
    std::fs::write(dir.path().join("value.neki"), "21").unwrap();
    let mut ev = Evaluator::new();
    let v = ev.eval_file(&dir.path().join("main.neki")).unwrap();
    assert_eq!(ev.print(&v).unwrap(), "42");
}

/// A store in a temporary directory, for `derivation`.
struct TestStore(LocalStore);

impl EvalStore for TestStore {
    fn store_dir(&self) -> &StoreDir {
        self.0.store_dir()
    }
    fn add_path(
        &self,
        name: &str,
        path: &Path,
        _filter: Option<PathFilter>,
    ) -> Result<StorePath, String> {
        self.0.add_path(name, path).map_err(|e| e.to_string())
    }
    fn add_text(
        &self,
        name: &str,
        text: &str,
        refs: &BTreeSet<StorePath>,
    ) -> Result<StorePath, String> {
        self.0.add_text(name, text, refs).map_err(|e| e.to_string())
    }
    fn add_derivation(&self, drv: &Derivation) -> Result<StorePath, String> {
        self.0.add_derivation(drv).map_err(|e| e.to_string())
    }
    fn read_derivation(&self, path: &StorePath) -> Result<Derivation, String> {
        self.0.read_derivation(path).map_err(|e| e.to_string())
    }
    fn fetch(&self, _name: &str, _request: &FetchRequest) -> Result<StorePath, String> {
        Err("no fetching in tests".into())
    }
}

#[test]
fn derivations_and_context() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let local = LocalStore::open(StoreDir::new(&store_dir), dir.path().join("var")).unwrap();
    let store = Rc::new(TestStore(local));
    let mut ev = Evaluator::with_store(store.clone());
    ev.system = "x86_64-linux".into();
    std::fs::write(dir.path().join("src.txt"), "source").unwrap();
    std::fs::write(
        dir.path().join("pkgs.neki"),
        r#"rec {
          dep = derivation { name = "dep"; system = builtins.currentSystem; builder = "builtin:run"; };
          app = derivation {
            name = "app";
            system = builtins.currentSystem;
            builder = "builtin:run";
            src = ./src.txt;
            buildInputs = [ dep ];
            outputs = [ "out" "doc" ];
          };
          fixed = derivation {
            name = "f";
            system = "x";
            builder = "builtin:fetchurl";
            url = "https://example.org/f";
            outputHash = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
          };
        }"#,
    )
    .unwrap();
    let pkgs = ev.eval_file(&dir.path().join("pkgs.neki")).unwrap();
    let app = ev.select(&pkgs, &["app"]).unwrap();
    let (drv_path, output) = ev.derivation_info(&app).unwrap().unwrap();
    assert_eq!(output, "out");

    let drv = store.0.read_derivation(&drv_path).unwrap();
    assert_eq!(drv.input_drvs.len(), 1, "{drv:?}");
    assert_eq!(drv.input_srcs.len(), 1, "the copied src.txt");
    assert_eq!(drv.env["outputs"], "out doc");
    assert!(drv.env["buildInputs"].ends_with("-dep"));
    assert!(drv.env["src"].ends_with("-src.txt"));

    // Outputs as attributes, `outPath` and string context.
    let out = ev.select(&app, &["outPath"]).unwrap();
    let doc = ev.select(&app, &["doc", "outPath"]).unwrap();
    let (Value::String { s: out, context }, Value::String { s: doc, .. }) = (out, doc) else {
        panic!("outPath is not a string");
    };
    assert!(out.ends_with("-app") && doc.ends_with("-app-doc"));
    assert!(
        context
            .iter()
            .any(|c| matches!(c, ContextElem::Output(d, o) if *d == drv_path && o == "out"))
    );
    let interpolated = ev.eval_str("x: \"${x}/bin\"", dir.path()).unwrap();
    let Value::String { context, .. } = ev.call(&interpolated, app.clone()).unwrap() else {
        panic!();
    };
    assert_eq!(context.len(), 1);

    let fixed = ev.select(&pkgs, &["fixed"]).unwrap();
    let (fixed_drv, _) = ev.derivation_info(&fixed).unwrap().unwrap();
    assert!(
        store
            .0
            .read_derivation(&fixed_drv)
            .unwrap()
            .is_fixed_output()
    );

    // Evaluating again gives the same paths.
    let mut ev2 = Evaluator::with_store(store.clone());
    ev2.system = "x86_64-linux".into();
    let pkgs2 = ev2.eval_file(&dir.path().join("pkgs.neki")).unwrap();
    let app2 = ev2.select(&pkgs2, &["app"]).unwrap();
    assert_eq!(ev2.derivation_info(&app2).unwrap().unwrap().0, drv_path);
    drop((ev, ev2, store));
    neki_platform_cleanup(dir.path());
}

/// Store entries are read-only; clear that so the temp dir can be removed.
fn neki_platform_cleanup(path: &Path) {
    fn walk(p: &Path) {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() {
                return;
            }
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(p, perms);
            if meta.is_dir() {
                for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
                    walk(&e.path());
                }
            }
        }
    }
    walk(path);
}

/// Computes store paths without writing anything, for golden tests with the
/// default store dir.
struct DryStore {
    dir: StoreDir,
    drvs: std::cell::RefCell<std::collections::HashMap<StorePath, Derivation>>,
}

impl EvalStore for DryStore {
    fn store_dir(&self) -> &StoreDir {
        &self.dir
    }
    fn add_path(
        &self,
        name: &str,
        path: &Path,
        _filter: Option<PathFilter>,
    ) -> Result<StorePath, String> {
        let (hash, _) = neki_store::nar_hash(path).map_err(|e| e.to_string())?;
        self.dir
            .make_path("source", &hash, name)
            .map_err(|e| e.to_string())
    }
    fn add_text(&self, _: &str, _: &str, _: &BTreeSet<StorePath>) -> Result<StorePath, String> {
        Err("not needed".into())
    }
    fn add_derivation(&self, drv: &Derivation) -> Result<StorePath, String> {
        let path = drv.drv_path(&self.dir).map_err(|e| e.to_string())?;
        self.drvs.borrow_mut().insert(path.clone(), drv.clone());
        Ok(path)
    }
    fn read_derivation(&self, path: &StorePath) -> Result<Derivation, String> {
        self.drvs
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| format!("unknown {path}"))
    }
    fn fetch(&self, _: &str, _: &FetchRequest) -> Result<StorePath, String> {
        Err("no fetching".into())
    }
}

/// Golden vector (M4): this expression's `.drv` path, with the default
/// store dir, never changes; if it does, every package's path changes (a
/// breaking change). Cross-checked against an independent Python
/// implementation of the ATerm, output-path and text-path algorithms.
#[test]
fn golden_drv_path() {
    let store = Rc::new(DryStore {
        dir: StoreDir::new("/opt/neki/store"),
        drvs: Default::default(),
    });
    let mut ev = Evaluator::with_store(store.clone());
    let v = ev
        .eval_str(
            r#"let
              dep = derivation { name = "dep"; system = "x86_64-linux"; builder = "builtin:run"; };
            in derivation {
              name = "golden";
              system = "x86_64-linux";
              builder = "builtin:run";
              buildInputs = [ dep ];
              commands = builtins.toJSON [ [ "echo" "hi" ] ];
            }"#,
            Path::new("/"),
        )
        .unwrap();
    let (drv, _) = ev.derivation_info(&v).unwrap().unwrap();
    assert_eq!(drv.to_string(), GOLDEN_DRV);
    let out = ev.select(&v, &["outPath"]).unwrap();
    let Value::String { s, .. } = out else {
        panic!()
    };
    assert_eq!(s, GOLDEN_OUT);
}

const GOLDEN_DRV: &str = "b8mxwkbgcwh1la9xwz1fr0d40v971xqh-golden.drv";
const GOLDEN_OUT: &str = "/opt/neki/store/qq5ihbrcqnp2llyam289mrrysvdq04w7-golden";

/// Pure evaluation (the default) doesn't read the environment and refuses
/// fetches that a hash or commit doesn't pin; `pure = false` allows both.
#[test]
fn pure_evaluation() {
    let get_path = r#"builtins.getEnv "PATH""#;
    assert_eq!(eval(get_path), r#""""#);
    let unpinned = [
        r#"builtins.fetchurl "https://example.org/a""#,
        r#"builtins.fetchTarball { url = "https://example.org/a.tar.gz"; }"#,
        r#"builtins.fetchGit { url = "https://example.org/r.git"; ref = "main"; }"#,
    ];
    for src in unpinned {
        let err = eval_err(src);
        assert!(err.contains("pure evaluation needs"), "{src}: {err}");
    }

    let mut ev = Evaluator::new();
    ev.pure = false;
    let path = ev.eval_str(get_path, Path::new("/project")).unwrap();
    assert_ne!(ev.print(&path).unwrap(), r#""""#);
    for src in unpinned {
        // Allowed now; it fails later only because there's no store.
        let err = ev.eval_str(src, Path::new("/project")).unwrap_err();
        assert!(!err.to_string().contains("pure evaluation"), "{src}: {err}");
    }
}
