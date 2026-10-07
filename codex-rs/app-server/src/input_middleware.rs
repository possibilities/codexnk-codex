//! Bounded, independent disposition requests with serialized durable commitments.
//!
//! Queue pressure must not turn the registered pass fallback into Core rejection.
//! The owner timeout belongs to each directed request; journal sync and receipt
//! mutation share the existing resolution lock, outside the decision wait.

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::InputMiddlewareDisposition;
use codex_app_server_protocol::InputMiddlewareOrigin;
use codex_app_server_protocol::InputMiddlewareRecord;
use codex_app_server_protocol::InputMiddlewareRequestParams;
use codex_app_server_protocol::InputMiddlewareRequestResponse;
use codex_app_server_protocol::InputMiddlewareResolvedNotification;
use codex_app_server_protocol::InputMiddlewareUnavailablePolicy;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequestPayload;
use codex_core::HumanInputCommit;
use codex_core::HumanInputDecision;
use codex_core::HumanInputMiddlewareRequest;
use codex_core::HumanInputOrigin;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

const MAX_CONCURRENT_DECISIONS: usize = 16;

#[derive(Clone)]
pub(crate) struct AdmissionWorker {
    pub(crate) owner: ConnectionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) outgoing: Arc<OutgoingMessageSender>,
    pub(crate) resolutions: Arc<Mutex<HashMap<ThreadId, HashMap<String, InputMiddlewareRecord>>>>,
    pub(crate) journal: PathBuf,
    pub(crate) timeout: Duration,
    pub(crate) on_unavailable: InputMiddlewareUnavailablePolicy,
}

impl AdmissionWorker {
    pub(crate) async fn run(self, mut receiver: mpsc::Receiver<HumanInputMiddlewareRequest>) {
        let mut decisions = JoinSet::new();
        let mut accepting = true;
        while accepting || !decisions.is_empty() {
            tokio::select! {
                Some(result) = decisions.join_next(), if !decisions.is_empty() => {
                    if let Err(error) = result {
                        tracing::error!(?error, "input middleware decision task failed");
                    }
                }
                request = receiver.recv(), if accepting && decisions.len() < MAX_CONCURRENT_DECISIONS => {
                    match request {
                        Some(request) => {
                            let worker = self.clone();
                            decisions.spawn(async move { worker.admit(request).await });
                        }
                        None => accepting = false,
                    }
                }
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "journal appends and effect receipt updates must be serialized"
    )]
    async fn admit(&self, request: HumanInputMiddlewareRequest) {
        let candidate = request.candidate;
        let input_id = candidate.input_id.clone();
        let original_text = candidate.text.clone();
        let payload = InputMiddlewareRequestParams {
            thread_id: candidate.thread_id.to_string(),
            input_id: candidate.input_id,
            origin: match candidate.origin {
                HumanInputOrigin::Client => InputMiddlewareOrigin::Client,
                HumanInputOrigin::Realtime => InputMiddlewareOrigin::Realtime,
            },
            text: candidate.text,
        };
        let (server_request_id, answer) = self
            .outgoing
            .send_request_to_connections(
                Some(&[self.owner]),
                ServerRequestPayload::InputMiddlewareRequest(payload),
                Some(self.thread_id),
            )
            .await;
        let fallback = || match self.on_unavailable {
            InputMiddlewareUnavailablePolicy::Pass => HumanInputDecision::Pass,
            InputMiddlewareUnavailablePolicy::Reject => HumanInputDecision::Reject,
        };
        let decision = match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(Ok(value))) => {
                match serde_json::from_value::<InputMiddlewareRequestResponse>(value) {
                    Ok(InputMiddlewareRequestResponse::Pass) => HumanInputDecision::Pass,
                    Ok(InputMiddlewareRequestResponse::Replace { text })
                        if !text.is_empty()
                            && text.chars().count() <= codex_core::MAX_MIDDLEWARE_TEXT_CHARS =>
                    {
                        HumanInputDecision::Replace(text)
                    }
                    Ok(InputMiddlewareRequestResponse::Intercept { operation_id })
                        if !operation_id.is_empty() =>
                    {
                        HumanInputDecision::Intercept { operation_id }
                    }
                    _ => fallback(),
                }
            }
            _ => fallback(),
        };
        self.outgoing.cancel_request(&server_request_id).await;
        let _ = request.reply.send(decision);
        let Ok(commit) = request.committed.await else {
            return;
        };
        // The durable record reflects Core's winning commitment, including a
        // defensive rejection, rather than the pre-commit proposed decision.
        let (disposition, selected_text) = match commit {
            HumanInputCommit::Pass => (
                InputMiddlewareDisposition::Passed,
                Some(original_text.clone()),
            ),
            HumanInputCommit::Replace { text } => {
                (InputMiddlewareDisposition::Replaced, Some(text))
            }
            HumanInputCommit::Intercept { operation_id } => (
                InputMiddlewareDisposition::Intercepted { operation_id },
                None,
            ),
            HumanInputCommit::Reject => (InputMiddlewareDisposition::Rejected, None),
        };
        let resolved = InputMiddlewareResolvedNotification {
            thread_id: self.thread_id.to_string(),
            input_id: input_id.clone(),
            disposition,
            effect: None,
        };
        let record = InputMiddlewareRecord {
            thread_id: resolved.thread_id.clone(),
            input_id: resolved.input_id.clone(),
            original_text,
            selected_text,
            disposition: resolved.disposition.clone(),
            effect: None,
        };
        let mut resolutions = self.resolutions.lock().await;
        if let Err(error) =
            crate::input_middleware_journal::append(self.journal.clone(), &record).await
        {
            tracing::error!(?error, "failed to persist input middleware resolution");
            let _ = request.stored.send(false);
            return;
        }
        resolutions
            .entry(self.thread_id)
            .or_default()
            .insert(input_id, record);
        drop(resolutions);
        let _ = request.stored.send(true);
        self.outgoing
            .send_server_notification_to_connections(
                &[self.owner],
                ServerNotification::InputMiddlewareResolved(resolved),
            )
            .await;
    }
}
