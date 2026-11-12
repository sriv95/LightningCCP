//! The fresh schema must equal Python's: `sqlite_master` dump and `user_version` 25 (24 from Python, 25 added in Rust). The expected dump is frozen in
//! tests/fixtures/store_base_schema.json (made by the Python original's `Store.connect` on a fresh file; the Python
//! original is gone, so it is not regenerated). The check that Python opens a Rust-made file ran before the cut-over.
use std::path::Path;

use lightningccp::store::base::Store;
use serde_json::{Value, json};

const DUMP: &str = "SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY name";

fn rust_dump(path: &Path) -> Value {
    let db = rusqlite::Connection::open(path).unwrap();
    let mut stmt = db.prepare(DUMP).unwrap();
    let rows: Vec<Value> = stmt
        .query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?])))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    json!({"version": version, "master": rows})
}

#[tokio::test]
async fn fresh_schema_matches_the_frozen_python_schema() {
    let dir = std::env::temp_dir().join(format!("lccp-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rs_path = dir.join("rs.db");
    Store::connect(&rs_path).await.unwrap().close().await.unwrap();
    let from_rust = rust_dump(&rs_path);
    let from_python: Value = serde_json::from_str(include_str!("fixtures/store_base_schema.json")).unwrap();
    assert_eq!(from_rust["version"], 25);
    assert_eq!(from_python["version"], 25);
    assert_eq!(from_rust["master"], from_python["master"], "sqlite_master differs");
    let _ = std::fs::remove_dir_all(&dir);
}
