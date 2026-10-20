//! Serving the local store as a binary cache over HTTP.

use std::fs;
use std::io::{self, Read, Write};
use std::net::SocketAddr;

use neki_hash::StorePath;
use neki_store::{LocalStore, Store};

use crate::{Error, NarInfo, PublicKey, Result, SecretKey, fingerprint};

/// An HTTP server that offers every valid path of a store, with narinfos
/// generated (and signed) on request and NARs streamed uncompressed.
///
/// With uploads allowed, clients (`neki cache push http://…`) add paths
/// with `PUT /nar/<hash part>.nar.zst` (or `.nar`) followed by
/// `PUT /<hash part>.narinfo`. A path is imported only if its narinfo has a
/// valid signature from one of the server's trusted keys, its NAR has the
/// signed hash, and the server already has its references.
pub struct CacheServer {
    server: tiny_http::Server,
}

impl CacheServer {
    /// Listen on `addr` (e.g. `127.0.0.1:8080`; port 0 picks a free one).
    pub fn bind(addr: &str) -> Result<Self> {
        let server =
            tiny_http::Server::http(addr).map_err(|e| Error::Http(format!("{addr}: {e}")))?;
        Ok(Self { server })
    }

    pub fn addr(&self) -> Option<SocketAddr> {
        self.server.server_addr().to_ip()
    }

    /// Stop [`CacheServer::run`] (from another thread).
    pub fn stop(&self) {
        self.server.unblock();
    }

    /// Answer requests until [`CacheServer::stop`] is called. Paths are
    /// signed with `key` if given; otherwise clients must already trust them.
    /// Uploads are accepted if `uploads` gives the keys to trust.
    pub fn run(&self, store: &LocalStore, key: Option<&SecretKey>, uploads: Option<&[PublicKey]>) {
        for mut request in self.server.incoming_requests() {
            let url = request.url().to_string();
            if *request.method() == tiny_http::Method::Put {
                let (status, message) = match uploads {
                    None => (405, "this server doesn't accept uploads".to_string()),
                    Some(keys) => match receive(store, keys, &url, request.as_reader()) {
                        Ok(()) => (201, "stored".to_string()),
                        Err(rejected) => rejected,
                    },
                };
                let _ = request
                    .respond(tiny_http::Response::from_string(message).with_status_code(status));
                continue;
            }
            let result = match respond(store, key, &url) {
                Ok(Some(Body::Text(text))) => {
                    request.respond(tiny_http::Response::from_string(text))
                }
                Ok(Some(Body::Nar(path))) => {
                    let (reader, writer) = match io::pipe() {
                        Ok(pipe) => pipe,
                        Err(_) => continue,
                    };
                    let location = store.store_dir().to_path(&path);
                    std::thread::spawn(move || {
                        let mut out = io::BufWriter::new(writer);
                        let _ = neki_nar::dump(&location, &mut out);
                        let _ = out.flush();
                    });
                    request.respond(tiny_http::Response::new(
                        tiny_http::StatusCode(200),
                        Vec::new(),
                        reader,
                        None,
                        None,
                    ))
                }
                Ok(None) => request.respond(tiny_http::Response::empty(404)),
                Err(e) => request
                    .respond(tiny_http::Response::from_string(e.to_string()).with_status_code(500)),
            };
            // A client that went away isn't the server's problem.
            let _ = result;
        }
    }
}

enum Body {
    Text(String),
    Nar(neki_hash::StorePath),
}

fn respond(store: &LocalStore, key: Option<&SecretKey>, url: &str) -> Result<Option<Body>> {
    let store_dir = store.store_dir();
    if url == "/nix-cache-info" {
        return Ok(Some(Body::Text(format!(
            "StoreDir: {}\nWantMassQuery: 1\nPriority: 30\n",
            store_dir.path().display()
        ))));
    }
    if let Some(hash) = url
        .strip_prefix('/')
        .and_then(|u| u.strip_suffix(".narinfo"))
    {
        let Some(path) = store.query_path_from_hash_part(hash)? else {
            return Ok(None);
        };
        let Some(mut info) = store.query_path_info(&path)? else {
            return Ok(None);
        };
        if let Some(key) = key {
            let signature = key.sign(&fingerprint(store_dir, &info));
            if !info.signatures.contains(&signature) {
                info.signatures.push(signature);
            }
        }
        let narinfo = NarInfo {
            info,
            url: format!("nar/{hash}.nar"),
            compression: "none".into(),
            file_hash: None,
            file_size: None,
        };
        return Ok(Some(Body::Text(narinfo.to_text(store_dir))));
    }
    if let Some(hash) = url
        .strip_prefix("/nar/")
        .and_then(|u| u.strip_suffix(".nar"))
    {
        return Ok(store.query_path_from_hash_part(hash)?.map(Body::Nar));
    }
    Ok(None)
}

/// Handle an upload; on failure, the HTTP status and why.
fn receive(
    store: &LocalStore,
    keys: &[PublicKey],
    url: &str,
    body: &mut dyn Read,
) -> std::result::Result<(), (u16, String)> {
    let bad = |m: String| (400, m);
    let dir = store.state_dir().join("uploads");
    fs::create_dir_all(&dir).map_err(|e| (500, e.to_string()))?;
    // Only a real hash part may name a file here.
    let hash_part = |s: &str| {
        StorePath::parse(&format!("{s}-x"))
            .map(|_| s.to_string())
            .map_err(|_| bad(format!("{s:?} is not a store path hash")))
    };
    if let Some(rest) = url.strip_prefix("/nar/") {
        let (hash, compression) = match (rest.strip_suffix(".nar.zst"), rest.strip_suffix(".nar")) {
            (Some(h), _) => (h, "zstd"),
            (None, Some(h)) => (h, "none"),
            _ => return Err((404, "unknown upload".into())),
        };
        let hash = hash_part(hash)?;
        let part = dir.join(format!("{hash}.part"));
        let mut file = fs::File::create(&part).map_err(|e| (500, e.to_string()))?;
        io::copy(body, &mut file).map_err(|e| bad(format!("reading the upload: {e}")))?;
        drop(file);
        fs::rename(&part, dir.join(format!("{hash}.{compression}")))
            .map_err(|e| (500, e.to_string()))?;
        return Ok(());
    }
    let Some(hash) = url
        .strip_prefix('/')
        .and_then(|u| u.strip_suffix(".narinfo"))
    else {
        return Err((404, "unknown upload".into()));
    };
    let hash = hash_part(hash)?;
    let mut text = String::new();
    body.take(1 << 20)
        .read_to_string(&mut text)
        .map_err(|e| bad(format!("reading the narinfo: {e}")))?;
    let narinfo = NarInfo::parse(&text, store.store_dir()).map_err(|e| bad(e.to_string()))?;
    let path = &narinfo.info.path;
    if path.hash_part() != hash {
        return Err(bad(format!("the narinfo is for {path}, not {hash}")));
    }
    if !narinfo.is_trusted(store.store_dir(), keys) {
        return Err((
            403,
            format!("{path} has no signature from a key this server trusts"),
        ));
    }
    if store
        .is_valid_path(path)
        .map_err(|e| (500, e.to_string()))?
    {
        return Ok(());
    }
    let compression = match narinfo.compression.as_str() {
        "zstd" => "zstd",
        "none" | "" => "none",
        other => return Err(bad(format!("unsupported compression {other:?}"))),
    };
    let nar_file = dir.join(format!("{hash}.{compression}"));
    let file =
        fs::File::open(&nar_file).map_err(|_| bad(format!("upload the NAR of {path} first")))?;
    let mut reader: Box<dyn Read> = if compression == "zstd" {
        Box::new(zstd::stream::read::Decoder::new(file).map_err(|e| bad(e.to_string()))?)
    } else {
        Box::new(io::BufReader::new(file))
    };
    let imported = store.import_nar(&narinfo.info, &mut reader);
    drop(reader);
    let _ = fs::remove_file(&nar_file);
    imported.map_err(|e| bad(e.to_string()))
}
