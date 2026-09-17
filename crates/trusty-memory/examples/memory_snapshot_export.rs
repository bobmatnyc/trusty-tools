//! Read-only current-corpus export for the private memory evaluation.
//!
//! Why: Real-data evaluation must not alter the live memory store.
//! What: One read transaction exports decoded drawer and graph rows to a private file.
//! Test: `export_preserves_database_and_counts`, `invalid_inputs_fail_closed`.

use anyhow::{Context, Result, ensure};
use clap::Parser;
use redb::ReadableTable;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use trusty_common::memory_core::store::{
    ReadOnlyRedb,
    kg_store::{
        DRAWERS, DrawerRecord, TRIPLES, TripleValue, decode_triple_key, decode_value, encode_value,
    },
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    private_output: PathBuf,
    #[arg(long)]
    max_rows: usize,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

type LegacyFields = (String, String, f32, Vec<String>, Option<String>, i64);
type PreTaskFields = (
    String,
    String,
    f32,
    Vec<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<i64>,
);
type PreFactKeyFields = (
    String,
    String,
    f32,
    Vec<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<i64>,
    Option<i64>,
);
const DRAWER_FIELDS: [&str; 10] = [
    "room_id",
    "content",
    "importance",
    "tags",
    "source_file",
    "created_at_ms",
    "drawer_type",
    "expires_at_ms",
    "completed_at_ms",
    "fact_key",
];

fn exact_decode<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Option<T> {
    let value = decode_value::<T>(bytes).ok()?;
    (encode_value(&value).ok()?.as_slice() == bytes).then_some(value)
}

fn legacy_values<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Option<Vec<Value>> {
    serde_json::to_value(exact_decode::<T>(bytes)?)
        .ok()?
        .as_array()
        .cloned()
}

/// Why: Older postcard rows lack trailing fields and must retain that distinction.
/// What: Mirror the four shapes in kg_redb/types.rs; reject trailing or unknown bytes.
/// Test: `historical_drawers_preserve_present_fields`, `unknown_drawer_bytes_are_rejected`.
fn decode_drawer(bytes: &[u8]) -> Result<(DrawerRecord, &'static str, Vec<&'static str>)> {
    if let Some(record) = exact_decode::<DrawerRecord>(bytes) {
        ensure!(record.importance.is_finite(), "invalid drawer importance");
        return Ok((record, "current", vec![]));
    }
    let versions = [
        ("pre_fact_key", legacy_values::<PreFactKeyFields>(bytes)),
        ("pre_task", legacy_values::<PreTaskFields>(bytes)),
        ("legacy", legacy_values::<LegacyFields>(bytes)),
    ];
    for (version, values) in versions {
        if let Some(values) = values {
            let absent = DRAWER_FIELDS[values.len()..].to_vec();
            let mut fields: serde_json::Map<String, Value> = DRAWER_FIELDS
                .iter()
                .map(|name| (name.to_string(), Value::Null))
                .collect();
            for (name, value) in DRAWER_FIELDS.iter().zip(values) {
                fields.insert(name.to_string(), value);
            }
            return Ok((
                serde_json::from_value(Value::Object(fields))?,
                version,
                absent,
            ));
        }
    }
    anyhow::bail!("unknown or malformed drawer encoding")
}

/// Why: Content-bearing exports must not become tracked files accidentally.
/// What: Resolve the existing parent and reject every enclosing Git worktree.
/// Test: `invalid_inputs_fail_closed`.
fn output_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("output needs a parent directory")?
        .canonicalize()?;
    ensure!(
        !parent.ancestors().any(|p| p.join(".git").exists()),
        "private output is inside a Git worktree"
    );
    Ok(parent.join(path.file_name().context("output needs a filename")?))
}

/// Why: Silent decode loss invalidates corpus coverage measurements.
/// What: Read both tables atomically, reject corruption or excessive row counts.
/// Test: `export_preserves_database_and_counts`, `invalid_inputs_fail_closed`.
fn snapshot(database: &Path, max_rows: usize) -> Result<Value> {
    ensure!(max_rows > 0, "max-rows must be positive");
    let db = ReadOnlyRedb::open(database).context("read-only database open failed")?;
    let txn = db.begin_read()?;
    let captured_at_ms = chrono::Utc::now().timestamp_millis();
    let mut drawers = Vec::new();
    let mut triples = Vec::new();
    let mut drawer_decode_versions = BTreeMap::from([
        ("current", 0usize),
        ("pre_fact_key", 0),
        ("pre_task", 0),
        ("legacy", 0),
    ]);
    for entry in txn.open_table(DRAWERS)?.iter()? {
        let (key, value) = entry?;
        ensure!(drawers.len() < max_rows, "row limit exceeded");
        let (record, version, absent) = decode_drawer(value.value()).with_context(|| {
            format!(
                "drawer decode failed at row {} key digest {}",
                drawers.len(),
                hex(&Sha256::digest(key.value()))
            )
        })?;
        *drawer_decode_versions.entry(version).or_default() += 1;
        drawers.push(json!({"key_hex":hex(key.value()), "record":record,
            "decode_version":version, "absent_fields":absent}));
    }
    for entry in txn.open_table(TRIPLES)?.iter()? {
        let (key, value) = entry?;
        ensure!(
            drawers.len() + triples.len() < max_rows,
            "row limit exceeded"
        );
        let value = exact_decode::<TripleValue>(value.value())
            .context("unknown or malformed triple value encoding")?;
        let raw = key.value();
        let (kind, core) = if let Some(rest) = raw.strip_prefix(b"hist:") {
            ensure!(rest.len() >= 8, "malformed history key");
            ("history", &rest[..rest.len() - 8])
        } else {
            ("active", raw)
        };
        let (subject, predicate, object) =
            decode_triple_key(core).context("triple key decode failed")?;
        ensure!(object == value.object, "triple key/value object mismatch");
        ensure!(value.confidence.is_finite(), "invalid triple confidence");
        triples.push(json!({"key_hex":hex(raw), "row_kind":kind, "subject":subject,
            "predicate":predicate, "object":object, "valid_from_ms":value.valid_from_ms,
            "valid_to_ms":value.valid_to_ms, "confidence":value.confidence, "provenance":value.provenance}));
    }
    Ok(
        json!({"version":"memory-real-snapshot-v1", "captured_at_ms":captured_at_ms,
        "drawer_decode_versions":drawer_decode_versions,
        "recovered_copy":db.is_snapshot(), "counts":{
        "drawers":{"read":drawers.len(), "decoded":drawers.len(), "errors":0},
        "triples":{"read":triples.len(), "decoded":triples.len(), "errors":0}},
        "drawers":drawers, "triples":triples}),
    )
}

/// Why: Failed extraction must not leave a plausible partial export.
/// What: Decode first, exclusively create a mode-0600 file, then persist and report its digest.
/// Test: `export_preserves_database_and_counts`, `invalid_inputs_fail_closed`.
fn export(args: &Args) -> Result<Value> {
    let output = output_path(&args.private_output)?;
    ensure!(!output.exists(), "output already exists");
    let data = snapshot(&args.database, args.max_rows)?;
    let bytes = serde_json::to_vec(&data)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        std::fs::remove_file(&output).context("remove incomplete export")?;
        return Err(error.into());
    }
    Ok(
        json!({"counts":data["counts"], "drawer_decode_versions":data["drawer_decode_versions"],
        "sha256":hex(&Sha256::digest(&bytes)), "recovered_copy":data["recovered_copy"]}),
    )
}

fn main() -> Result<()> {
    println!("{}", export(&Args::parse())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use trusty_common::memory_core::store::kg_store::{encode_triple_key, encode_value};

    fn fixture(path: &Path, corrupt: bool) {
        let db = redb::Database::create(path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut drawers = txn.open_table(DRAWERS).unwrap();
            let drawer = DrawerRecord {
                room_id: "room".into(),
                content: "Toy memory".into(),
                importance: 0.5,
                tags: vec![],
                source_file: None,
                created_at_ms: 1,
                drawer_type: None,
                expires_at_ms: None,
                completed_at_ms: None,
                fact_key: None,
            };
            let bytes = if corrupt {
                vec![255]
            } else {
                encode_value(&drawer).unwrap()
            };
            drawers.insert(&[1u8; 16][..], bytes.as_slice()).unwrap();
            let mut triples = txn.open_table(TRIPLES).unwrap();
            let key = encode_triple_key("Alpha", "uses", "Beta");
            let mut value = TripleValue {
                object: "Beta".into(),
                valid_from_ms: 1,
                valid_to_ms: None,
                confidence: 1.0,
                provenance: Some("toy".into()),
            };
            triples
                .insert(key.as_slice(), encode_value(&value).unwrap().as_slice())
                .unwrap();
            value.valid_to_ms = Some(2);
            let historical = [b"hist:".as_slice(), key.as_slice(), &[0; 8]].concat();
            triples
                .insert(
                    historical.as_slice(),
                    encode_value(&value).unwrap().as_slice(),
                )
                .unwrap();
        }
        txn.commit().unwrap();
    }

    #[test]
    fn export_preserves_database_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let args = Args {
            database: dir.path().join("toy.redb"),
            private_output: dir.path().join("out.json"),
            max_rows: 3,
        };
        fixture(&args.database, false);
        let before = std::fs::read(&args.database).unwrap();
        let report = export(&args).unwrap();
        assert_eq!(report["counts"]["triples"]["decoded"], 2);
        assert_eq!(before, std::fs::read(&args.database).unwrap());
        assert_eq!(
            std::fs::metadata(&args.private_output)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(export(&args).is_err());
    }

    #[test]
    fn invalid_inputs_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = Args {
            database: dir.path().join("toy.redb"),
            private_output: dir.path().join("out.json"),
            max_rows: 3,
        };
        assert!(export(&args).is_err());
        assert!(!args.database.exists());
        fixture(&args.database, true);
        assert!(export(&args).is_err());
        assert!(!args.private_output.exists());
        std::fs::remove_file(&args.database).unwrap();
        fixture(&args.database, false);
        args.max_rows = 2;
        assert!(export(&args).is_err());
        assert!(!args.private_output.exists());
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(output_path(&args.private_output).is_err());
    }

    #[test]
    fn historical_drawers_preserve_present_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        fixture(&path, false);
        let rows = [
            encode_value(&("room", "old", 0.5f32, vec!["tag"], Some("source"), 7i64)).unwrap(),
            encode_value(&(
                "room",
                "task",
                0.5f32,
                vec!["tag"],
                Some("source"),
                7i64,
                Some("Task"),
                Some(500i64),
            ))
            .unwrap(),
            encode_value(&(
                "room",
                "completed",
                0.5f32,
                vec!["tag"],
                Some("source"),
                7i64,
                Some("Task"),
                Some(500i64),
                Some(123i64),
            ))
            .unwrap(),
        ];
        {
            let db = redb::Database::open(&path).unwrap();
            let txn = db.begin_write().unwrap();
            {
                let mut table = txn.open_table(DRAWERS).unwrap();
                for (i, bytes) in rows.iter().enumerate() {
                    table
                        .insert(&[i as u8 + 2; 16][..], bytes.as_slice())
                        .unwrap();
                }
            }
            txn.commit().unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let result = snapshot(&path, 6).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(result["drawer_decode_versions"]["legacy"], 1);
        assert_eq!(result["drawer_decode_versions"]["pre_task"], 1);
        assert_eq!(result["drawer_decode_versions"]["pre_fact_key"], 1);
        assert_eq!(result["drawers"][3]["record"]["completed_at_ms"], 123);
        assert_eq!(result["drawers"][3]["absent_fields"], json!(["fact_key"]));
    }

    #[test]
    fn unknown_drawer_bytes_are_rejected() {
        let bytes =
            encode_value(&("room", "old", 0.5f32, vec!["tag"], Some("source"), 7i64)).unwrap();
        assert!(decode_drawer(&bytes).is_ok());
        assert!(decode_drawer(&[bytes.as_slice(), &[255]].concat()).is_err());
        assert!(decode_drawer(&[255]).is_err());
    }

    #[test]
    fn invalid_active_and_history_values_leave_no_export() {
        for history in [false, true] {
            for trailing in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let args = Args {
                    database: dir.path().join("toy.redb"),
                    private_output: dir.path().join("out.json"),
                    max_rows: 3,
                };
                fixture(&args.database, false);
                let core = encode_triple_key("Alpha", "uses", "Beta");
                let key = if history {
                    [b"hist:".as_slice(), core.as_slice(), &[0; 8]].concat()
                } else {
                    core
                };
                let value = TripleValue {
                    object: "Beta".into(),
                    valid_from_ms: 1,
                    valid_to_ms: history.then_some(2),
                    confidence: 1.0,
                    provenance: Some("toy".into()),
                };
                let bad = if trailing {
                    [encode_value(&value).unwrap(), vec![255]].concat()
                } else {
                    vec![255]
                };
                {
                    let db = redb::Database::open(&args.database).unwrap();
                    let txn = db.begin_write().unwrap();
                    {
                        let mut table = txn.open_table(TRIPLES).unwrap();
                        table.insert(key.as_slice(), bad.as_slice()).unwrap();
                    }
                    txn.commit().unwrap();
                }
                let before = std::fs::read(&args.database).unwrap();
                assert!(
                    export(&args).is_err(),
                    "history={history}, trailing={trailing}"
                );
                assert!(!args.private_output.exists());
                assert_eq!(std::fs::read(&args.database).unwrap(), before);
            }
        }
    }
}
