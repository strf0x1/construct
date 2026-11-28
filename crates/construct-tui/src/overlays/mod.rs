//! Modal overlays and peer views that sit above the transcript. Each overlay
//! owns its own state, key handling and rendering, and reports decisions back
//! to `app` through small action enums without depending on `app`.

pub(crate) mod ask;
pub(crate) mod peers;
pub(crate) mod picker;
pub(crate) mod sessions;

/// What a key did to a list-picking modal (the model picker or session browser).
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Stay,
    Close,
    /// User picked this id.
    Pick(String),
}
