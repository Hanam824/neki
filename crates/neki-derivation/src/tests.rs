use super::*;

fn store() -> StoreDir {
    StoreDir::new("/opt/neki/store")
}

fn source(name: &str) -> StorePath {
    let hash = Hash::of_bytes(HashAlgo::Sha256, name.as_bytes());
    store().make_path("source", &hash, name).unwrap()
}

fn drv(name: &str) -> Derivation {
    Derivation {
        outputs: BTreeMap::from([("out".into(), DerivationOutput::Deferred)]),
        input_drvs: BTreeMap::new(),
        input_srcs: BTreeSet::from([source("src")]),
        system: "x86_64-linux".into(),
        builder: "builtin:run".into(),
        args: vec!["a \"quoted\"\\arg".into(), "line\nbreak\ttab\r".into()],
        env: BTreeMap::from([("name".into(), name.into())]),
    }
}

fn no_inputs(_: &StorePath) -> Result<Derivation> {
    Err(Error::Invalid("no inputs expected".into()))
}

#[test]
fn host_system_is_arch_dash_os() {
    let system = host_system();
    let (arch, os) = system.split_once('-').unwrap();
    assert_eq!(arch, std::env::consts::ARCH);
    assert!(["linux", "darwin", "windows"].contains(&os), "{system}");
}

#[test]
fn aterm_round_trips() {
    let mut d = drv("hello");
    d.finalize(&store(), &no_inputs).unwrap();
    let text = d.to_aterm(&store()).unwrap();
    assert_eq!(Derivation::parse_aterm(&text, &store()).unwrap(), d);
    assert!(text.starts_with("Derive([(\"out\",\"/opt/neki/store/"));
    assert!(text.contains(r#""a \"quoted\"\\arg","line\nbreak\ttab\r""#));
}

/// Golden vector: the exact text and paths of a finalized derivation. If this
/// changes, every derivation and output path changes (a breaking change). Cross-checked
/// against an independent Python implementation of the same algorithm.
#[test]
fn golden_derivation() {
    let mut d = drv("hello");
    d.finalize(&store(), &no_inputs).unwrap();
    let out = d.output_paths(&store()).unwrap()["out"].clone();
    let expected_out = "/opt/neki/store/87b3pf6zfv7apbwhbikzfpgrvmbzw3l4-hello";
    assert_eq!(store().to_path(&out).display().to_string(), expected_out);
    assert_eq!(d.env["out"], expected_out);
    assert_eq!(
        d.to_aterm(&store()).unwrap(),
        format!(
            "Derive([(\"out\",\"{expected_out}\",\"\",\"\")],[],[\"/opt/neki/store/{}\"],\"x86_64-linux\",\"builtin:run\",[\"a \\\"quoted\\\"\\\\arg\",\"line\\nbreak\\ttab\\r\"],[(\"name\",\"hello\"),(\"out\",\"{expected_out}\")])",
            source("src")
        )
    );
}

#[test]
fn rejects_malformed_aterm() {
    for bad in [
        "",
        "Derive(",
        "Derive([],[],[],\"s\",\"b\",[],[])x",
        "Derive([],[],[],\"s\",\"b\",[],[(\"a\")])",
        "Derive([],[],[],\"unterminated,\"b\",[],[])",
    ] {
        assert!(Derivation::parse_aterm(bad, &store()).is_err(), "{bad:?}");
    }
    assert!(Derivation::parse_aterm("Derive([],[],[],\"s\",\"b\",[],[])", &store()).is_ok());
}

/// Changing how a fixed-output input is fetched (its builder or URL) must
/// not change the output paths of derivations that use it.
#[test]
fn hash_modulo_ignores_how_fixed_outputs_are_made() {
    let hash = Hash::of_bytes(HashAlgo::Sha256, b"tarball");
    let fetch = |url: &str| {
        let mut f = drv("src.tar.gz");
        f.input_srcs.clear();
        f.outputs = BTreeMap::from([(
            "out".into(),
            DerivationOutput::Fixed {
                hash: hash.clone(),
                recursive: false,
            },
        )]);
        f.builder = "builtin:fetchurl".into();
        f.env.insert("url".into(), url.into());
        f.finalize(&store(), &no_inputs).unwrap();
        f
    };
    let (a, b) = (fetch("https://a.example/x"), fetch("https://b.example/x"));
    assert_ne!(a.drv_path(&store()).unwrap(), b.drv_path(&store()).unwrap());
    assert_eq!(
        a.output_paths(&store()).unwrap(),
        b.output_paths(&store()).unwrap()
    );

    let user = |fetcher: &Derivation| {
        let fetcher = fetcher.clone();
        let drv_path = fetcher.drv_path(&store()).unwrap();
        let mut d = drv("app");
        d.input_drvs = BTreeMap::from([(drv_path, BTreeSet::from(["out".to_string()]))]);
        d.finalize(&store(), &move |_| Ok(fetcher.clone())).unwrap();
        d.output_paths(&store()).unwrap()
    };
    assert_eq!(user(&a), user(&b));

    // An input-addressed input does change the dependent's output.
    let mut other = drv("dep");
    other.finalize(&store(), &no_inputs).unwrap();
    let mut other2 = drv("dep");
    other2.args.push("changed".into());
    other2.finalize(&store(), &no_inputs).unwrap();
    assert_ne!(user(&other), user(&other2));
}

#[test]
fn fixed_output_paths() {
    let hash = Hash::of_bytes(HashAlgo::Sha256, b"x");
    // A recursive SHA-256 output is at the same path as an added source.
    assert_eq!(
        fixed_output_path(&store(), "x", &hash, true).unwrap(),
        store().make_path("source", &hash, "x").unwrap()
    );
    assert_ne!(
        fixed_output_path(&store(), "x", &hash, false).unwrap(),
        fixed_output_path(&store(), "x", &hash, true).unwrap()
    );
}

#[test]
fn extra_outputs_get_suffixed_names() {
    let mut d = drv("lib");
    d.outputs.insert("dev".into(), DerivationOutput::Deferred);
    d.finalize(&store(), &no_inputs).unwrap();
    let paths = d.output_paths(&store()).unwrap();
    assert_eq!(paths["out"].name(), "lib");
    assert_eq!(paths["dev"].name(), "lib-dev");
    assert!(d.env.contains_key("dev"));
}
