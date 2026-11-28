//! The Host Bridge: the `ctx` object a script receives, and the only way out
//! of the sandbox (ADR 0079). Each role gets the capabilities it is allowed;
//! today that is the wake role's `ctx.state`, `ctx.wake` and `ctx.log`.
//!
//! The design choice: the bridge never acts during the call. It records what
//! the script asked for in a `Call`, and the caller acts on it only after the
//! call returns cleanly, so a script that throws after `ctx.wake(...)` wakes
//! nothing and writes no state.

use crate::state::Txn;
use rquickjs::convert::Coerced;
use rquickjs::{Ctx, Exception, Function, Object, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// The most `ctx.log` lines one call keeps, and the longest, in bytes.
const LOG_LINES: usize = 20;
const LOG_LINE_MAX: usize = 500;

/// What one call asked the host for.
pub struct Call {
    /// `None` when the home's store could not be opened (the reason).
    pub state: Result<Txn, String>,
    /// The argument of `ctx.wake(...)`, as JSON.
    pub wake: Option<serde_json::Value>,
    pub logs: Vec<String>,
    /// The output cap the wake argument must fit, in bytes.
    output: usize,
}

impl Call {
    pub fn new(state: Result<Txn, String>, output: usize) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { state, wake: None, logs: Vec::new(), output }))
    }
}

/// `ctx` for a wake script: `state.get/set/delete`, `wake(opts)`, `log(msg)`.
pub fn wake_ctx<'js>(ctx: &Ctx<'js>, call: &Rc<RefCell<Call>>) -> rquickjs::Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;
    obj.set("state", state_obj(ctx, call)?)?;

    let c = call.clone();
    let wake = Function::new(ctx.clone(), move |ctx: Ctx<'js>, opts: Value<'js>| -> rquickjs::Result<()> {
        let mut call = c.borrow_mut();
        if call.wake.is_some() {
            return Err(Exception::throw_message(&ctx, "ctx.wake may be called once per event"));
        }
        let text = to_json(&ctx, opts)?.unwrap_or_else(|| "{}".into());
        if text.len() > call.output {
            let msg = format!("ctx.wake argument is {} bytes, over the {}-byte limit", text.len(), call.output);
            return Err(Exception::throw_message(&ctx, &msg));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| throw(&ctx, &e.to_string()))?;
        if !v.is_object() {
            return Err(Exception::throw_message(
                &ctx,
                "ctx.wake takes an object: { summary?, data?, session?, untrusted? }",
            ));
        }
        call.wake = Some(v);
        Ok(())
    })?;
    obj.set("wake", wake)?;

    let c = call.clone();
    let log = Function::new(ctx.clone(), move |msg: Coerced<String>| {
        let mut call = c.borrow_mut();
        if call.logs.len() < LOG_LINES {
            call.logs.push(cap(&msg.0, LOG_LINE_MAX));
        }
    })?;
    obj.set("log", log)?;
    Ok(obj)
}

fn state_obj<'js>(ctx: &Ctx<'js>, call: &Rc<RefCell<Call>>) -> rquickjs::Result<Object<'js>> {
    let state = Object::new(ctx.clone())?;
    let c = call.clone();
    let get = Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<Value<'js>> {
        let call = c.borrow();
        let txn = call.state.as_ref().map_err(|e| throw(&ctx, &format!("ctx.state is unavailable: {e}")))?;
        match txn.get(&key).map_err(|e| throw(&ctx, &e))? {
            Some(json) => ctx.json_parse(json),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    })?;
    state.set("get", get)?;

    let c = call.clone();
    let set =
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String, value: Value<'js>| -> rquickjs::Result<()> {
            let json = to_json(&ctx, value)?.ok_or_else(|| throw(&ctx, "ctx.state.set needs a JSON value"))?;
            let mut call = c.borrow_mut();
            let txn = call.state.as_mut().map_err(|e| throw(&ctx, &format!("ctx.state is unavailable: {e}")))?;
            txn.set(&key, json).map_err(|e| throw(&ctx, &e))
        })?;
    state.set("set", set)?;

    let c = call.clone();
    let delete = Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<()> {
        let mut call = c.borrow_mut();
        let txn = call.state.as_mut().map_err(|e| throw(&ctx, &format!("ctx.state is unavailable: {e}")))?;
        txn.delete(&key).map_err(|e| throw(&ctx, &e))
    })?;
    state.set("delete", delete)?;
    Ok(state)
}

/// A JS value as JSON text; `None` for what JSON cannot hold (`undefined`).
fn to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Option<String>> {
    ctx.json_stringify(value)?.map(|s| s.to_string()).transpose()
}

fn throw(ctx: &Ctx<'_>, msg: &str) -> rquickjs::Error {
    Exception::throw_message(ctx, msg)
}

/// `s` cut to at most `max` bytes on a char boundary.
pub fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).map(|t| format!("{t}…")).unwrap_or_default()
}
