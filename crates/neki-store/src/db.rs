//! The store's metadata DB (SQLite): one row per valid path plus the
//! references between paths, like Nix's `db.sqlite`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use neki_hash::{Hash, StorePath};
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::{Error, PathInfo, Result};

/// Version stored in `PRAGMA user_version`; bump it with a migration when the
/// schema changes.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE ValidPaths (
    id                INTEGER PRIMARY KEY,
    path              TEXT NOT NULL UNIQUE, -- <hash>-<name>, without the store dir
    nar_hash          TEXT NOT NULL,        -- SRI
    nar_size          INTEGER NOT NULL,
    deriver           TEXT,
    registration_time INTEGER NOT NULL,     -- Unix seconds
    signatures        TEXT NOT NULL         -- space-separated
);
CREATE TABLE Refs (
    referrer  INTEGER NOT NULL REFERENCES ValidPaths(id) ON DELETE CASCADE,
    reference INTEGER NOT NULL REFERENCES ValidPaths(id) ON DELETE RESTRICT,
    PRIMARY KEY (referrer, reference)
);
CREATE INDEX RefsReference ON Refs(reference);
CREATE TABLE Roots (
    name TEXT PRIMARY KEY,
    path TEXT NOT NULL, -- <hash>-<name>
    link TEXT           -- indirect roots: a link that must still point at the path
);
-- A path may refer to itself; that reference must not block its deletion.
CREATE TRIGGER DeleteSelfRefs BEFORE DELETE ON ValidPaths BEGIN
    DELETE FROM Refs WHERE referrer = old.id AND reference = old.id;
END;
";

/// Open (creating if needed) the DB at `file`.
pub fn open(file: &Path) -> Result<Connection> {
    let mut conn = Connection::open(file)?;
    // Other processes may hold the write lock briefly; wait instead of failing.
    conn.busy_timeout(Duration::from_secs(60))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    match tx.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? {
        0 => {
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        SCHEMA_VERSION => {}
        version => {
            return Err(Error::Corrupt(format!(
                "store DB schema version {version} is newer than this version of Neki supports ({SCHEMA_VERSION})"
            )));
        }
    }
    tx.commit()?;
    Ok(conn)
}

pub fn query(conn: &Connection, path: &StorePath) -> Result<Option<PathInfo>> {
    let row = conn
        .query_row(
            "SELECT id, nar_hash, nar_size, deriver, registration_time, signatures
             FROM ValidPaths WHERE path = ?1",
            [path.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((id, nar_hash, nar_size, deriver, registration_time, signatures)) = row else {
        return Ok(None);
    };
    let mut references = BTreeSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT p.path FROM Refs r JOIN ValidPaths p ON p.id = r.reference WHERE r.referrer = ?1",
    )?;
    for reference in stmt.query_map([id], |row| row.get::<_, String>(0))? {
        references.insert(StorePath::parse(&reference?)?);
    }
    Ok(Some(PathInfo {
        path: path.clone(),
        nar_hash: Hash::parse(&nar_hash)?,
        nar_size: u64::try_from(nar_size)
            .map_err(|_| Error::Corrupt(format!("{path}: negative NAR size")))?,
        references,
        deriver: deriver.as_deref().map(StorePath::parse).transpose()?,
        registration_time,
        signatures: signatures.split_whitespace().map(String::from).collect(),
    }))
}

/// Every valid path, sorted.
pub fn valid_paths(conn: &Connection) -> Result<Vec<StorePath>> {
    let mut stmt = conn.prepare("SELECT path FROM ValidPaths ORDER BY path")?;
    stmt.query_map([], |row| row.get::<_, String>(0))?
        .map(|path| Ok(StorePath::parse(&path?)?))
        .collect()
}

/// Record `infos` as valid paths. Their references must be valid already
/// or be among `infos`.
pub fn register(tx: &Transaction, infos: &[PathInfo]) -> Result<()> {
    for info in infos {
        let nar_size = i64::try_from(info.nar_size)
            .map_err(|_| Error::Corrupt(format!("{}: NAR too large", info.path)))?;
        tx.execute(
            "INSERT INTO ValidPaths (path, nar_hash, nar_size, deriver, registration_time, signatures)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                info.path.to_string(),
                info.nar_hash.to_sri(),
                nar_size,
                info.deriver.as_ref().map(ToString::to_string),
                info.registration_time,
                info.signatures.join(" "),
            ),
        )?;
    }
    for info in infos {
        let id = path_id(tx, &info.path)?.expect("inserted above");
        for reference in &info.references {
            let reference_id =
                path_id(tx, reference)?.ok_or_else(|| Error::NotValid(reference.to_string()))?;
            tx.execute(
                "INSERT INTO Refs (referrer, reference) VALUES (?1, ?2)",
                (id, reference_id),
            )?;
        }
    }
    Ok(())
}

fn path_id(conn: &Connection, path: &StorePath) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM ValidPaths WHERE path = ?1",
            [path.to_string()],
            |row| row.get(0),
        )
        .optional()?)
}
