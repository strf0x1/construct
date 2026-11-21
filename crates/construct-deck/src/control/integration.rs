//! Request fields for contract 1.6.0 integration. The client checks shape
//! before sending. Deck owns repository resolution and exact commit pins.

use serde_json::{Value, json};
use std::collections::HashSet;

pub const MAX_CONTRIBUTORS: usize = 16;
pub const MAX_INTEGRATION_REASON: usize = 512;
const LOCAL_BRANCH: &str = "local_branch";
const NEW_PULL_REQUEST: &str = "new_pull_request";
const EXISTING_PULL_REQUEST: &str = "existing_pull_request";
const BRANCH_PATTERN: &str = "^[A-Za-z0-9._/-]{1,200}$";

pub(super) fn destination_schema() -> Value {
    json!({
        "type": "object",
        "description": "integrate: choose a local branch, a new draft PR against the project's configured base, or an existing PR to use as the starting point.",
        "oneOf": [
            { "type": "object", "properties": {
                "kind": { "const": LOCAL_BRANCH },
                "branch": { "type": "string", "pattern": BRANCH_PATTERN }
            }, "required": ["kind", "branch"], "additionalProperties": false },
            { "type": "object", "properties": {
                "kind": { "const": NEW_PULL_REQUEST }
            }, "required": ["kind"], "additionalProperties": false },
            { "type": "object", "properties": {
                "kind": { "const": EXISTING_PULL_REQUEST },
                "number": { "type": "integer", "minimum": 1 }
            }, "required": ["kind", "number"], "additionalProperties": false }
        ]
    })
}

pub(super) fn destination(value: &Value) -> Result<Value, String> {
    let object = value.as_object().ok_or("must be an object")?;
    let branch = value["branch"].as_str().unwrap_or_default();
    let valid_branch = || {
        (1..=200).contains(&branch.len()) && branch.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
    };
    let valid = match value["kind"].as_str() {
        Some(LOCAL_BRANCH) => object.len() == 2 && valid_branch(),
        Some(NEW_PULL_REQUEST) => object.len() == 1,
        Some(EXISTING_PULL_REQUEST) => object.len() == 2 && value["number"].as_u64().is_some_and(|n| n > 0),
        _ => false,
    };
    if valid {
        Ok(value.clone())
    } else {
        Err(format!("use {LOCAL_BRANCH} with branch, {NEW_PULL_REQUEST}, or {EXISTING_PULL_REQUEST} with number"))
    }
}

pub(super) fn contributors(value: &Value) -> Result<Value, String> {
    let invalid = || format!("must be an ordered list of 1 to {MAX_CONTRIBUTORS} distinct task IDs");
    let array = value.as_array().filter(|a| (1..=MAX_CONTRIBUTORS).contains(&a.len())).ok_or_else(invalid)?;
    let mut ids = HashSet::with_capacity(array.len());
    for id in array {
        let id = id.as_str().filter(|id| (1..=128).contains(&id.chars().count())).ok_or_else(invalid)?;
        if !ids.insert(id) {
            return Err(invalid());
        }
    }
    Ok(value.clone())
}
