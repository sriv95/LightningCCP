//! Frozen-vector fixtures. `UPDATE_VECTORS=1 cargo test` rewrites the expected values from the actual output instead of asserting.
#![allow(dead_code)]
use std::io::Write;
use std::sync::{Mutex, Once};

use serde_json::Value;

static LOCK: Mutex<()> = Mutex::new(());
static SAID: Once = Once::new();

pub fn regen() -> bool {
    std::env::var("UPDATE_VECTORS").is_ok_and(|v| v == "1")
}

fn path(file: &str) -> String {
    format!("{}/tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"))
}

pub fn load(file: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path(file)).expect("fixture")).expect("fixture json")
}

/// Sets the JSON pointer `ptr` in the fixture to `v` (compact JSON plus a newline, so an unchanged regen leaves no diff).
pub fn put(file: &str, ptr: &str, v: Value) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    SAID.call_once(|| {
        // direct stderr write bypasses test output capture
        let _ = writeln!(std::io::stderr(), "UPDATE_VECTORS=1: rewriting tests/fixtures/{file} instead of asserting");
    });
    let mut all = load(file);
    *all.pointer_mut(ptr).unwrap_or_else(|| panic!("no vector {ptr}")) = v;
    std::fs::write(path(file), serde_json::to_string(&all).unwrap() + "\n").unwrap();
}
