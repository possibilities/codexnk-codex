//! The app-server's optional, thread-local human-input admission channel.
//!
//! Core owns the gate so a realtime handoff cannot bypass an app-server client.
//! Without a registered receiver, submission follows the existing path.

use crate::session::session::Session;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use tokio::sync::oneshot;

pub(crate) const MAX_MIDDLEWARE_INPUTS: usize = 4096;
/// Shared by the Core gate and app-server reply validation (Unicode scalar values).
pub const MAX_MIDDLEWARE_TEXT_CHARS: usize = 32768;

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
    Replace { text: String },
    Intercept { operation_id: String },
    Reject,
}

/// Selects and journals human text without starting, steering, or recording a turn.
/// Both native submission and history-only realtime tails use this admission gate.
pub(crate) async fn admit(
    session: &Session,
    request: &mut TurnInputRequest,
) -> Result<(), NotSubmittedReason> {
    let owner = session
        .human_input_middleware
        .read()
        .expect("human-input middleware lock poisoned")
        .clone();
    let (Some(owner), Some(source)) = (owner, request.human_input_source.as_ref()) else {
        return Ok(());
    };
    if source.id.is_empty() {
        return Err(NotSubmittedReason::InputMiddlewareUnavailable);
    }
    let input_id = format!(
        "{}:{}",
        if source.realtime { "realtime" } else { "client" },
        source.id
    );
    let TurnInput::UserInput { content, .. } = &mut request.input else {
        return Err(NotSubmittedReason::InputMiddlewareUnavailable);
    };
    let [UserInput::Text { text, text_elements }] = content.as_mut_slice() else {
        return Err(NotSubmittedReason::InputMiddlewareUnavailable);
    };
    if !text_elements.is_empty() || text.chars().count() > MAX_MIDDLEWARE_TEXT_CHARS {
        return Err(NotSubmittedReason::InputMiddlewareUnavailable);
    }
    {
        let mut seen = session.admitted_human_inputs.lock().await;
        if seen.contains(&input_id) {
            return Err(NotSubmittedReason::DuplicateHumanInput { input_id });
        }
        if seen.len() >= MAX_MIDDLEWARE_INPUTS {
            return Err(NotSubmittedReason::InputMiddlewareUnavailable);
        }
        seen.insert(input_id.clone());
    }
    let (reply, response) = oneshot::channel();
    let (commit_sender, committed) = oneshot::channel();
    let (stored, storage_ack) = oneshot::channel();
    let candidate = HumanInputCandidate {
        thread_id: session.thread_id,
        input_id: input_id.clone(),
        origin: if source.realtime {
            HumanInputOrigin::Realtime
        } else {
            HumanInputOrigin::Client
        },
        text: text.clone(),
    };
    let decision = if owner
        .send(HumanInputMiddlewareRequest {
            candidate,
            reply,
            committed,
            stored,
        })
        .await
        .is_ok()
    {
        // The app-server bounds each directed decision and owns its registered
        // fallback. A second, submission-wide deadline would race that policy
        // while candidates wait for bounded worker/journal capacity.
        response.await.ok()
    } else {
        None
    };
    match decision.unwrap_or(HumanInputDecision::Reject) {
        HumanInputDecision::Pass => {
            let _ = commit_sender.send(HumanInputCommit::Pass);
            if !matches!(storage_ack.await, Ok(true)) {
                return Err(NotSubmittedReason::InputMiddlewareUnavailable);
            }
        }
        HumanInputDecision::Replace(replacement) => {
            if replacement.is_empty() || replacement.chars().count() > MAX_MIDDLEWARE_TEXT_CHARS {
                // The app-server validates replies, but other Core owners may
                // still send malformed decisions. Never abandon a reserved ID.
                let _ = commit_sender.send(HumanInputCommit::Reject);
                let _ = storage_ack.await;
                return Err(NotSubmittedReason::InputMiddlewareUnavailable);
            }
            let _ = commit_sender.send(HumanInputCommit::Replace {
                text: replacement.clone(),
            });
            if !matches!(storage_ack.await, Ok(true)) {
                return Err(NotSubmittedReason::InputMiddlewareUnavailable);
            }
            *text = replacement;
        }
        HumanInputDecision::Intercept { operation_id } => {
            let _ = commit_sender.send(HumanInputCommit::Intercept {
                operation_id: operation_id.clone(),
            });
            if !matches!(storage_ack.await, Ok(true)) {
                return Err(NotSubmittedReason::InputMiddlewareUnavailable);
            }
            return Err(NotSubmittedReason::InputIntercepted {
                input_id,
                operation_id,
            });
        }
        HumanInputDecision::Reject => {
            let _ = commit_sender.send(HumanInputCommit::Reject);
            let _ = storage_ack.await;
            return Err(NotSubmittedReason::InputMiddlewareUnavailable);
        }
    }
    Ok(())
}
