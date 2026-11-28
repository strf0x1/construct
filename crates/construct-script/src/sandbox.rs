//! The confined QuickJS runtime every script role runs in (ADR 0079): a fresh
//! runtime per call with a memory ceiling, a stack ceiling and a wall-clock
//! interrupt, and a context with no ambient authority.
//!
//! The design choice: a script is compiled once to bytecode in a throwaway
//! context that has QuickJS's `Eval` intrinsic, and runs from that bytecode
//! in a context without it. With no `Eval` there is no compiler at run time,
//! so `eval`, `new Function` and `(async () => {}).constructor` all throw;
//! with no module loader `import` fails; and QuickJS itself has no `fs`,
//! `net`, `process`, `env` or timers. Everything a script can reach is what
//! the caller puts on the object it hands in (`bridge.rs`).

use rquickjs::context::intrinsic;
use rquickjs::module::WriteOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Module, Object, Runtime, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What one call may use. Defaults suit a wake filter that runs per event.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Heap ceiling for the runtime, in bytes.
    pub memory: usize,
    /// Wall-clock budget for loading the module and running the call.
    pub timeout: Duration,
    /// The most bytes a call may hand back (a wake's summary and data).
    pub output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { memory: 16 << 20, timeout: Duration::from_millis(500), output: 16 << 10 }
    }
}

const STACK_MAX: usize = 512 << 10;

/// The intrinsics a running script gets: the language, JSON, RegExp, Map/Set,
/// typed arrays, Date and promises. Not `Eval`, `Proxy`, `WeakRef` or
/// `Performance`.
type Confined = (
    intrinsic::Date,
    intrinsic::RegExpCompiler,
    intrinsic::RegExp,
    intrinsic::Json,
    intrinsic::MapSet,
    intrinsic::TypedArrays,
    intrinsic::Promise,
);

/// A script compiled to QuickJS bytecode, cheap to clone and to run again.
#[derive(Clone)]
pub struct Compiled {
    name: Arc<str>,
    bytecode: Arc<[u8]>,
}

impl Compiled {
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Compile `source` as an ES module named `name`. A syntax error is the
/// error's text, for a note.
pub fn compile(name: &str, source: &str) -> Result<Compiled, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(Limits::default().memory);
    rt.set_max_stack_size(STACK_MAX);
    let ctx = Context::custom::<(intrinsic::Eval, Confined)>(&rt).map_err(|e| e.to_string())?;
    let bytecode = ctx.with(|ctx| {
        Module::declare(ctx.clone(), name, source)
            .and_then(|m| m.write(WriteOptions::default()))
            .catch(&ctx)
            .map_err(|e| caught(&e))
    })?;
    Ok(Compiled { name: name.into(), bytecode: bytecode.into() })
}

/// Load `script` in a fresh confined runtime, run its top level, and hand its
/// exports to `call`. Anything `call` throws or leaves pending, a broken
/// limit included, comes back as the error's text.
pub fn run<R>(
    script: &Compiled,
    limits: &Limits,
    call: impl for<'js> FnOnce(&Ctx<'js>, Object<'js>) -> rquickjs::Result<R>,
) -> Result<R, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(limits.memory);
    rt.set_max_stack_size(STACK_MAX);
    let deadline = Instant::now() + limits.timeout;
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let ctx = Context::custom::<Confined>(&rt).map_err(|e| e.to_string())?;
    let out = ctx.with(|ctx| {
        let res = (|| {
            // SAFETY: the bytes come from `compile` in this process, by the
            // same QuickJS build, and are never read from disk or the wire.
            let declared = unsafe { Module::load(ctx.clone(), &script.bytecode) }?;
            let (module, done) = declared.eval()?;
            done.finish::<()>()?;
            call(&ctx, module.namespace()?)
        })();
        res.catch(&ctx).map_err(|e| caught(&e))
    });
    let timed_out = Instant::now() >= deadline;
    out.map_err(|e| if timed_out { format!("timed out after {:?}: {e}", limits.timeout) } else { e })
}

/// Settle what a call returned: a promise is driven to its end (nothing
/// outside QuickJS can resolve it, so one still pending is an error), any
/// other value is returned as it is.
pub fn settle<'js>(value: Value<'js>) -> rquickjs::Result<Value<'js>> {
    match value.as_promise() {
        Some(p) => p.finish::<Value>(),
        None => Ok(value),
    }
}

fn caught(e: &CaughtError<'_>) -> String {
    match e {
        CaughtError::Error(rquickjs::Error::WouldBlock) => "awaited something that never settles".into(),
        CaughtError::Exception(ex) => {
            let msg = ex.message().unwrap_or_default();
            if msg.is_empty() { "uncaught exception".into() } else { msg }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str, limits: Limits) -> Result<String, String> {
        let c = compile("t.js", src)?;
        run(&c, &limits, |_, ns| {
            let f: rquickjs::Function = ns.get("main")?;
            let v = settle(f.call(())?)?;
            Ok(v.as_string().map(|s| s.to_string()).transpose()?.unwrap_or_default())
        })
    }

    #[test]
    fn runs_async_exports() {
        let out = eval("export async function main() { await null; return 'ok'; }", Limits::default());
        assert_eq!(out.as_deref(), Ok("ok"));
    }

    #[test]
    fn has_no_compiler_at_run_time() {
        for body in
            ["return eval('1')", "return new Function('return 1')()", "return (async () => {}).constructor('return 1')"]
        {
            let src = format!("export function main() {{ {body} }}");
            assert!(eval(&src, Limits::default()).is_err(), "{body} ran");
        }
    }

    #[test]
    fn has_no_ambient_io() {
        let src = "export function main() { return [typeof require, typeof process, typeof std, typeof os, \
                   typeof fetch, typeof setTimeout, typeof console].join(','); }";
        let out = eval(src, Limits::default());
        assert_eq!(out.as_deref(), Ok("undefined,undefined,undefined,undefined,undefined,undefined,undefined"));
        let import = "import * as os from 'os'; export function main() { return 'x'; }";
        assert!(eval(import, Limits::default()).is_err());
    }

    #[test]
    fn interrupts_an_endless_loop() {
        let limits = Limits { timeout: Duration::from_millis(50), ..Limits::default() };
        let err = eval("export function main() { for (;;) {} }", limits).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn stops_at_the_memory_ceiling() {
        let limits = Limits { memory: 4 << 20, ..Limits::default() };
        let src = "export function main() { const a = []; for (;;) a.push('x'.repeat(1024)); }";
        assert!(eval(src, limits).is_err());
    }

    #[test]
    fn reports_syntax_errors() {
        let err = compile("bad.js", "export function main( {").err().unwrap_or_default();
        assert!(!err.is_empty());
    }
}
