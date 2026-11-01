//! The script vocabulary: what a scripted model answers with. A step is one
//! reply of the agent loop's; a side reply answers a call without tools (a
//! summarizer, a title), and is plain text unless it needs a step's knobs.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Text {
        text: String,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        reasoning: Option<String>,
        /// Unparseable `data:` lines sent before the words, the way a flaky
        /// proxy garbles a stream.
        #[serde(default)]
        bad_chunks: usize,
        /// End the stream after the words: no finish reason, usage or `[DONE]`.
        #[serde(default)]
        cut: bool,
        /// Finish with `length`, as a model stopped by its output limit.
        #[serde(default)]
        length: bool,
    },
    Call {
        call: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        delay_ms: u64,
    },
    Calls {
        calls: Vec<Value>,
    },
    /// Answer with this HTTP status instead of a stream: 429 and 5xx are
    /// retried by clients, 400 is a provider rejecting a parameter.
    Fail {
        status: u16,
        #[serde(default)]
        retry_after: Option<u64>,
        #[serde(default)]
        body: String,
    },
}

/// A reply to a side call: plain text, or a step for anything more.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Side {
    Text(String),
    Step(Step),
}
