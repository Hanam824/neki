//! Serving the local store as a binary cache over HTTP.

use std::io::{self, Write};
use std::net::SocketAddr;

use neki_store::{LocalStore, Store};

use crate::{Error, NarInfo, Result, SecretKey, fingerprint};

/// An HTTP server that offers every valid path of a store, with narinfos
/// generated (and signed) on request and NARs streamed uncompressed.
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
    pub fn run(&self, store: &LocalStore, key: Option<&SecretKey>) {
        for request in self.server.incoming_requests() {
            let url = request.url().to_string();
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
