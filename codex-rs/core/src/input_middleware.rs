//! The app-server's optional, thread-local human-input admission channel.
//!
//! Core owns the gate so a realtime handoff cannot bypass an app-server client.
//! Without a registered receiver, submission follows the existing path.

use codex_protocol::ThreadId;
use tokio::sync::oneshot;

pub(crate) const MAX_MIDDLEWARE_INPUTS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HumanInputOrigin {
    Client,
    Realtime,
}

#[derive(Clone, Debug)]
pub struct HumanInputCandidate {
    pub thread_id: ThreadId,
    pub input_id: String,
    pub origin: HumanInputOrigin,
    pub text: String,
}

#[derive(Debug)]
pub enum HumanInputDecision {
    Pass,
    Replace(String),
    Intercept { operation_id: String },
    Reject,
}

pub struct HumanInputMiddlewareRequest {
    pub candidate: HumanInputCandidate,
    pub reply: oneshot::Sender<HumanInputDecision>,
    /// Sent only once Core has committed its pre-dispatch decision.
    pub committed: oneshot::Receiver<HumanInputCommit>,
    /// The app-server must durably record the decision before Core proceeds.
    pub stored: oneshot::Sender<bool>,
}

#[derive(Debug)]
pub enum HumanInputCommit {
    Pass,
    Replace,
    Intercept { operation_id: String },
    Reject,
}
