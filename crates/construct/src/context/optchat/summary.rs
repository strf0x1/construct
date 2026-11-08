//! One summary line from the summary model (ADR 0043 decision 11): a fixed
//! system prompt, a clipped input, up to five tries while the reply is over
//! `LINE` bytes, and a `[unsummarized]` fallback that always yields a line.
//!
//! The one choice a reader must know: the strategy measures bytes itself and
//! never trusts the model's count, and a failed or refused call still returns
//! text, so `fit` can advance the view without a hole (decision 10).

use super::tree::{LINE, flatten};
use crate::config::Config;
use crate::context::Env;
use crate::llm::{Message, Usage};

/// The summary model's fixed prompt (ADR 0043 decision 11).
pub const SUMMARY_SYSTEM: &str = "Compress conversation history into one index line. \
Write one line of at most 512 bytes and never a newline. Keep names, ids, numbers, paths and decisions. \
No preamble, no labels. When merging two lines, cover both inputs, oldest first.";

/// Tries for a line that comes back too long. Also the budget for empty replies.
pub const TRIES: usize = 5;
const MAX_TOKENS: u32 = 1024;
/// The input's character cap: head and tail, with a marker between (decision 2).
const CLIP: usize = 30_000;
const HEAD: usize = 15_000;
const FALLBACK: &str = "[unsummarized] ";

/// What a summary call produced, and whether it is a fallback.
pub struct Summary {
    pub text: String,
    pub fallback: bool,
    pub usage: Usage,
}

/// What a leaf or a merge summarizes.
pub enum Input {
    /// One entry's flattened text, clipped for the model.
    Leaf(String),
    /// Two children's lines (`ref|text`), oldest first.
    Merge(String, String),
}

impl Input {
    /// What the fallback line keeps: the input's head, unclipped.
    fn text(&self) -> String {
        match self {
            Self::Leaf(text) => clip(text),
            Self::Merge(first, second) => format!("{first}\n{second}"),
        }
    }
}

/// The summary model: `[context.optchat] model`, else `[compaction] model`,
/// else the empty selector that means the run's model.
pub fn model(cfg: &Config) -> &str {
    let optchat = &cfg.context.optchat.model;
    if optchat.is_empty() { &cfg.compaction.model } else { optchat }
}

/// Keep the first and last `HEAD` characters of an over-long input.
fn clip(text: &str) -> String {
    let count = text.chars().count();
    if count <= CLIP {
        return text.to_string();
    }
    let head: String = text.chars().take(HEAD).collect();
    let tail: String = text.chars().skip(count - HEAD).collect();
    format!("{head}\n[…]\n{tail}")
}

/// The longest prefix of `text` at or below `max` bytes that ends on a char
/// boundary. Never cuts inside a character.
pub(super) fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or("")
}

/// One line for a leaf that needs the model (the entry is over `LINE` bytes).
pub async fn leaf(env: &Env<'_>, text: String) -> Summary {
    write(env, Input::Leaf(text)).await
}

/// One line merging two children, oldest first.
pub async fn merge(env: &Env<'_>, first: String, second: String) -> Summary {
    write(env, Input::Merge(first, second)).await
}

/// Ask the summary model, retrying a missing or over-long reply, then fall
/// back to the input's head. Every call's usage is summed.
pub async fn write(env: &Env<'_>, input: Input) -> Summary {
    let text = input.text();
    let request = vec![Message::system(SUMMARY_SYSTEM), Message::user(text.clone())];
    let mut usage = Usage::default();
    let mut shortest: Option<String> = None;
    for _ in 0..TRIES {
        let (reply, u) = env.llm.complete(model(env.cfg), request.clone(), Some(MAX_TOKENS)).await;
        usage.add(&u);
        let completion = match reply {
            Ok(c) => c,
            Err(_) => return fallback(&text, usage),
        };
        if completion.finish_reason == "content_filter" {
            return fallback(&text, usage);
        }
        let line = flatten(&completion.message.content.unwrap_or_default()).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.len() <= LINE {
            return Summary { text: line, fallback: false, usage };
        }
        if shortest.as_ref().is_none_or(|best| line.len() < best.len()) {
            shortest = Some(line);
        }
    }
    match shortest {
        Some(line) => Summary { text: cut(&line, LINE).to_string(), fallback: false, usage },
        None => fallback(&text, usage),
    }
}

/// `[unsummarized] ` plus the input's head, the whole line at most `LINE` bytes.
fn fallback(text: &str, usage: Usage) -> Summary {
    let head = cut(text, LINE.saturating_sub(FALLBACK.len()));
    Summary { text: format!("{FALLBACK}{head}"), fallback: true, usage }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Complete;
    use crate::llm::Completion;
    use std::sync::Mutex;

    /// One scripted reply per call, in order; each call costs 3+2 tokens.
    struct Fake {
        replies: Mutex<Vec<Result<String, String>>>,
        calls: Mutex<usize>,
    }

    impl Fake {
        fn new(replies: Vec<Result<String, String>>) -> Self {
            Self { replies: Mutex::new(replies), calls: Mutex::new(0) }
        }
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl Complete for Fake {
        async fn complete(&self, _: &str, _: Vec<Message>, _: Option<u32>) -> (Result<Completion, String>, Usage) {
            *self.calls.lock().unwrap() += 1;
            let reply = self.replies.lock().unwrap().remove(0);
            let usage = Usage { prompt_tokens: 3, completion_tokens: 2, ..Usage::default() };
            match reply {
                Ok(text) => (
                    Ok(Completion {
                        message: Message::text("assistant", text),
                        usage: usage.clone(),
                        finish_reason: "stop".into(),
                    }),
                    usage,
                ),
                Err(e) => (Err(e), usage),
            }
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    fn env<'a>(cfg: &'a Config, llm: &'a dyn Complete) -> Env<'a> {
        Env { size: &|_: &[Message]| 0, budget: 1000, bytes_per_token: 4.0, cfg, llm }
    }

    #[tokio::test]
    async fn a_short_reply_is_used_as_is_with_its_usage() {
        let cfg = cfg();
        let fake = Fake::new(vec![Ok("decision: use SQLite\nfor the journal".into())]);
        let s = leaf(&env(&cfg, &fake), "long entry".into()).await;
        assert_eq!(s.text, "decision: use SQLite for the journal");
        assert!(!s.fallback);
        assert_eq!(fake.calls(), 1);
        assert_eq!((s.usage.prompt_tokens, s.usage.completion_tokens), (3, 2));
    }

    #[tokio::test]
    async fn an_over_long_reply_is_retried_and_the_second_one_kept() {
        let cfg = cfg();
        let fake = Fake::new(vec![Ok("x".repeat(600)), Ok("short enough".into())]);
        let s = leaf(&env(&cfg, &fake), "long entry".into()).await;
        assert_eq!(s.text, "short enough");
        assert!(!s.fallback);
        assert_eq!(fake.calls(), 2);
        assert_eq!(s.usage.prompt_tokens, 6, "every call is counted");
    }

    #[tokio::test]
    async fn five_over_long_replies_keep_the_shortest_cut_on_a_char_boundary() {
        let cfg = cfg();
        // 511 'a's then 'é's: byte 512 falls inside the first 'é'.
        let reply = format!("{}ééé", "a".repeat(511));
        assert_eq!(reply.len(), 511 + 6);
        let fake = Fake::new((0..5).map(|_| Ok(reply.clone())).collect());
        let s = leaf(&env(&cfg, &fake), "long entry".into()).await;
        assert_eq!(fake.calls(), 5);
        assert!(s.text.len() <= LINE, "{} bytes", s.text.len());
        assert_eq!(s.text, "a".repeat(511), "cut before the split character");
        assert!(std::str::from_utf8(s.text.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn an_error_gives_the_fallback_head_and_sums_usage() {
        let cfg = cfg();
        let fake = Fake::new(vec![Err("the provider said no".into())]);
        let s = leaf(&env(&cfg, &fake), format!("{} rest", "h".repeat(5000))).await;
        assert!(s.fallback);
        assert!(s.text.starts_with(FALLBACK));
        assert!(s.text.len() <= LINE);
        assert!(s.text.contains("hhhh"), "the head is kept");
        assert_eq!(s.usage.prompt_tokens, 3, "a failed call was still paid for");
    }

    #[tokio::test]
    async fn an_empty_reply_after_the_tries_is_a_fallback() {
        let cfg = cfg();
        let fake = Fake::new((0..5).map(|_| Ok(String::new())).collect());
        let s = leaf(&env(&cfg, &fake), "entry".into()).await;
        assert!(s.fallback);
        assert_eq!(s.text, "[unsummarized] entry");
        assert_eq!(fake.calls(), 5);
    }

    #[tokio::test]
    async fn a_content_filter_refusal_falls_back_immediately() {
        let cfg = cfg();
        struct Filter;
        #[async_trait::async_trait]
        impl Complete for Filter {
            async fn complete(&self, _: &str, _: Vec<Message>, _: Option<u32>) -> (Result<Completion, String>, Usage) {
                (
                    Ok(Completion {
                        message: Message::text("assistant", "refused"),
                        usage: Usage::default(),
                        finish_reason: "content_filter".into(),
                    }),
                    Usage::default(),
                )
            }
        }
        let s = merge(&env(&cfg, &Filter), "0+1|a".into(), "1+1|b".into()).await;
        assert!(s.fallback);
        assert_eq!(s.text, "[unsummarized] 0+1|a\n1+1|b");
    }

    #[test]
    fn the_clip_keeps_the_head_and_the_tail_of_a_40000_character_input() {
        let text: String = std::iter::repeat_with(|| 'a').take(39_999).collect::<String>() + "END";
        let clipped = clip(&format!("START{text}"));
        assert!(clipped.starts_with("START"), "the head is kept");
        assert!(clipped.ends_with("END"), "the tail is kept");
        assert!(clipped.contains("\n[…]\n"));
        assert_eq!(clipped.chars().count(), HEAD * 2 + 5);
    }

    #[test]
    fn the_model_falls_back_from_optchat_to_compaction_to_the_run() {
        let mut cfg = Config::default();
        assert_eq!(model(&cfg), "");
        cfg.compaction.model = "compact-model".into();
        assert_eq!(model(&cfg), "compact-model");
        cfg.context.optchat.model = "optchat-model".into();
        assert_eq!(model(&cfg), "optchat-model");
    }
}
