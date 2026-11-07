//! The shared ACP extension contract (`CONTRACT.lock`): the constants the code
//! uses and the negotiation it implements must match the vendored matrix.
//! A mismatch means either the contract moved without this repo or the code
//! moved without the contract; both are the same bug.

use construct::acp::map;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn lock() -> HashMap<String, String> {
    let text = std::fs::read_to_string(root().join("CONTRACT.lock")).unwrap();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && l.contains('='))
        .map(|l| {
            let (k, v) = l.split_once('=').unwrap();
            (k.trim().to_string(), v.trim().trim_matches('"').to_string())
        })
        .collect()
}

fn list(v: &Value) -> Vec<&str> {
    v.as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect()
}

fn vendored() -> PathBuf {
    let l = lock();
    root().join(&l["vendored"])
}

fn matrix() -> Value {
    serde_json::from_str(&std::fs::read_to_string(vendored().join("matrix.json")).unwrap()).unwrap()
}

#[test]
fn the_vendored_copy_is_the_pinned_version() {
    let version = std::fs::read_to_string(vendored().join("VERSION")).unwrap();
    assert_eq!(version.trim(), lock()["version"]);
    assert_eq!(matrix()["contract"], json!(lock()["version"]));
    assert_eq!(matrix()["acp"], json!("1"));
}

#[test]
fn the_constants_are_the_contracts_names() {
    let names = &matrix()["names"];
    assert_eq!(names["construct_meta"], map::META_KEY);
    assert_eq!(names["construct_meta_legacy"], map::LEGACY_META_KEY);
    assert_eq!(names["deck_meta"], map::DECK_KEY);
    assert_eq!(names["deck_meta_legacy"], map::LEGACY_DECK_KEY);
    assert_eq!(names["event_method"], map::EVENT_METHOD);
    assert_eq!(names["event_method_legacy"], map::LEGACY_EVENT_METHOD);
    assert_eq!(names["turn_method"], map::TURN_METHOD);
    assert_eq!(names["turn_method_legacy"], map::LEGACY_TURN_METHOD);
    assert_eq!(names["boundary_method"], map::BOUNDARY_METHOD);
    assert_eq!(names["boundary_method_legacy"], map::LEGACY_BOUNDARY_METHOD);
}

#[test]
fn the_github_capability_is_the_contracts() {
    let m = matrix();
    let capability = &m["github"]["capability"];
    assert_eq!(capability["meta_key"], map::DECK_KEY);
    assert_eq!(capability["field"], map::GITHUB_OPERATIONS);
    assert_eq!(capability["version"], json!(map::GITHUB_OPERATIONS_VERSION));
    assert_eq!(capability["legacy_key"], json!(false), "advertised under the current key only (acp/deck.rs)");
    assert_eq!(m["github"]["environment"], "DECK_GITHUB_URL");
}

#[test]
fn every_negotiation_case_matches_the_code() {
    for case in matrix()["negotiation"].as_array().unwrap() {
        let wire = map::Wire::for_client(&case["client_meta"]);
        let expect = &case["expect"];
        assert_eq!(wire.lossless, expect["lossless"], "{}", case["case"]);
        assert_eq!(wire.event, expect["event_method"].as_str().unwrap(), "{}", case["case"]);
        assert_eq!(wire.turn, expect["turn_method"].as_str().unwrap(), "{}", case["case"]);
    }
}

#[test]
fn the_accepted_forms_are_the_legacy_and_current_constants() {
    let accept = &matrix()["accepts"];
    assert_eq!(list(&accept["boundary_methods"]), [map::BOUNDARY_METHOD, map::LEGACY_BOUNDARY_METHOD]);
    assert_eq!(list(&accept["start_meta_keys"]), [map::META_KEY, map::LEGACY_META_KEY]);
    assert_eq!(list(&accept["load_snapshot_keys"]), [map::META_KEY, map::LEGACY_META_KEY]);
}
