//! Parity check from PARITY.md: a fresh DB made by Python and one made by Rust must have byte-equal
//! `sqlite_master` dumps and `user_version` 24, and Python must open the Rust file with no migration to run.
//! Rerun: `cargo test --manifest-path rust/Cargo.toml --test store_base_parity`
//! (needs `.venv/bin/python` in the repo root, or `PARITY_PYTHON`).
use std::path::{Path, PathBuf};
use std::process::Command;

use lightningccp::store::base::Store;
use serde_json::{Value, json};

const DUMP: &str = "SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY name";

/// Python: optionally open `path` through `Store.connect` (migrating), then print the dump and user_version as JSON.
const PY: &str = r#"
import asyncio, json, sqlite3, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from src.db.store import Store
async def main():
    path = Path(sys.argv[2])
    s = Store(path)
    await s.connect()
    await s.close()
    db = sqlite3.connect(path)
    rows = db.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY name").fetchall()
    version = db.execute("PRAGMA user_version").fetchone()[0]
    db.close()
    print(json.dumps({"version": version, "master": rows}))
asyncio.run(main())
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn python(path: &Path) -> Value {
    let root = repo_root();
    let exe = std::env::var("PARITY_PYTHON").map(PathBuf::from).unwrap_or_else(|_| root.join(".venv/bin/python"));
    assert!(exe.exists(), "Python parity check needs {} (or PARITY_PYTHON)", exe.display());
    let out = Command::new(exe).args(["-I", "-c", PY]).arg(&root).arg(path).output().unwrap();
    assert!(out.status.success(), "python failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

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
async fn fresh_schema_matches_python_and_python_opens_the_rust_file() {
    let dir = std::env::temp_dir().join(format!("lccp-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (py_path, rs_path) = (dir.join("py.db"), dir.join("rs.db"));

    let from_python = python(&py_path); // Python creates its fresh file
    Store::connect(&rs_path).await.unwrap().close().await.unwrap();
    let from_rust = rust_dump(&rs_path);
    assert_eq!(from_rust["version"], 24);
    assert_eq!(from_python["version"], 24);
    assert_eq!(from_rust["master"], from_python["master"], "sqlite_master differs");

    // Python opens the Rust-created file: its migrations are a no-op, the dump and version are unchanged.
    let reopened = python(&rs_path);
    assert_eq!(reopened, from_rust);
    let _ = std::fs::remove_dir_all(&dir);
}
