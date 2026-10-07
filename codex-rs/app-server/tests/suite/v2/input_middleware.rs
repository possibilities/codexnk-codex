use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InputMiddlewareAttachParams;
use codex_app_server_protocol::InputMiddlewareAttachResponse;
use codex_app_server_protocol::InputMiddlewareCompleteParams;
use codex_app_server_protocol::InputMiddlewareCompleteResponse;
use codex_app_server_protocol::InputMiddlewareDetachParams;
use codex_app_server_protocol::InputMiddlewareDetachResponse;
use codex_app_server_protocol::InputMiddlewareDisposition;
use codex_app_server_protocol::InputMiddlewareEffectReceipt;
use codex_app_server_protocol::InputMiddlewareEffectStatus;
use codex_app_server_protocol::InputMiddlewareOrigin;
use codex_app_server_protocol::InputMiddlewareReadParams;
use codex_app_server_protocol::InputMiddlewareReadResponse;
use codex_app_server_protocol::InputMiddlewareRecord;
use codex_app_server_protocol::InputMiddlewareUnavailablePolicy;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartResponse;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use tempfile::TempDir;
use test_case::test_matrix;

async fn attach(
    app_server: &mut TestAppServer,
    thread_id: &str,
) -> Result<InputMiddlewareAttachResponse> {
    let response: InputMiddlewareAttachResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareAttach {
            request_id,
            params: InputMiddlewareAttachParams {
                thread_id: thread_id.to_string(),
                timeout_ms: 500,
                on_unavailable: InputMiddlewareUnavailablePolicy::Reject,
            },
        })
        .await?;
    assert_eq!(response.thread_id, thread_id);
    Ok(response)
}

async fn submit(app_server: &mut TestAppServer, thread_id: &str) -> Result<i64> {
    app_server
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": thread_id,
                "clientUserMessageId": "middleware-test-1",
                "input": [{"type":"text", "text":"Original prompt", "text_elements":[]}]
            })),
        )
        .await
}

#[tokio::test]
async fn intercept_claims_typed_input_without_starting_a_turn() -> Result<()> {
    let mut app_server = TestAppServer::builder().build_initialized().await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let attachment = attach(&mut app_server, &thread.id).await?;
    let original_request = submit(&mut app_server, &thread.id).await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { request_id, params } = request else {
        panic!("expected input middleware request: {request:?}");
    };
    assert_eq!(params.origin, InputMiddlewareOrigin::Client);
    assert_eq!(params.text, "Original prompt");
    app_server
        .send_response(request_id, json!({"type":"intercept","operationId":"op-1"}))
        .await?;
    let mut resolved = false;
    let mut errored = false;
    while !resolved || !errored {
        match app_server.read_next_message().await? {
            JSONRPCMessage::Notification(notification)
                if notification.method == "thread/input/resolved" =>
            {
                let params = notification.params.expect("resolution params");
                assert_eq!(params["disposition"]["type"], "intercepted");
                assert_eq!(params["disposition"]["operationId"], "op-1");
                resolved = true;
            }
            JSONRPCMessage::Error(error) if error.id == RequestId::Integer(original_request) => {
                assert!(error.error.message.contains("input intercepted"));
                errored = true;
            }
            _ => {}
        }
    }
    let read: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
            },
        })
        .await?;
    assert_eq!(
        read.record.expect("committed resolution").disposition,
        InputMiddlewareDisposition::Intercepted {
            operation_id: "op-1".to_string()
        },
    );
    let completed_id = app_server
        .send_request(
            "thread/input/complete",
            Some(serde_json::to_value(InputMiddlewareCompleteParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
                operation_id: "op-1".to_string(),
                receipt: InputMiddlewareEffectReceipt {
                    status: InputMiddlewareEffectStatus::Succeeded,
                    summary: "Handled externally".to_string(),
                },
            })?),
        )
        .await?;
    let mut completed = None;
    let mut effect_notice = false;
    while completed.is_none() || !effect_notice {
        match app_server.read_next_message().await? {
            JSONRPCMessage::Response(response)
                if response.id == RequestId::Integer(completed_id) =>
            {
                completed = Some(serde_json::from_value::<InputMiddlewareCompleteResponse>(
                    response.result,
                )?);
            }
            JSONRPCMessage::Notification(notification)
                if notification.method == "thread/input/resolved" =>
            {
                effect_notice = notification
                    .params
                    .as_ref()
                    .is_some_and(|params| params["effect"]["status"] == "succeeded");
            }
            _ => {}
        }
    }
    let completed = completed.expect("effect completion response");
    assert_eq!(completed.record.original_text, "Original prompt");
    assert_eq!(completed.record.selected_text, None);
    assert_eq!(
        completed.record.effect.expect("effect receipt").status,
        InputMiddlewareEffectStatus::Succeeded
    );
    let _: InputMiddlewareDetachResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareDetach {
            request_id,
            params: InputMiddlewareDetachParams {
                thread_id: thread.id.clone(),
                owner_id: attachment.owner_id,
            },
        })
        .await?;
    attach(&mut app_server, &thread.id).await?;
    Ok(())
}

#[tokio::test]
async fn replacement_is_the_text_delivered_to_the_agent() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        create_final_assistant_message_sse_response("Done")?,
    )
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    attach(&mut app_server, &thread.id).await?;
    let original_request = app_server
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": thread.id,
                "clientUserMessageId": "middleware-lineage",
                "parentTurnId": "initiating-turn",
                "rootTurnId": "causal-root",
                "input": [{"type":"text", "text":"Original prompt", "text_elements":[]}]
            })),
        )
        .await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { request_id, params } = request else {
        panic!("expected input middleware request: {request:?}");
    };
    assert_eq!(params.text, "Original prompt");
    app_server
        .send_response(
            request_id,
            json!({"type":"replace","text":"Replacement prompt"}),
        )
        .await?;
    let response: TurnStartResponse = app_server.read_response(original_request).await?;
    app_server
        .read_stream_until_notification_message("turn/completed")
        .await?;
    assert!(!response.turn.id.is_empty());
    assert_eq!(response.turn.root_turn_id.as_deref(), Some("causal-root"));
    let request = response_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("Replacement prompt"))
    );
    assert!(
        !request
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("Original prompt"))
    );
    Ok(())
}

#[tokio::test]
async fn strict_timeout_rejects_without_dispatch_and_records_resolution() -> Result<()> {
    let mut app_server = TestAppServer::builder().build_initialized().await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let _: InputMiddlewareAttachResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareAttach {
            request_id,
            params: InputMiddlewareAttachParams {
                thread_id: thread.id.clone(),
                timeout_ms: 100,
                on_unavailable: InputMiddlewareUnavailablePolicy::Reject,
            },
        })
        .await?;
    let original_request = submit(&mut app_server, &thread.id).await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { params, .. } = request else {
        panic!("expected input middleware request: {request:?}");
    };
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(original_request))
        .await?;
    assert!(error.error.message.contains("input middleware unavailable"));
    let recovered: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
            },
        })
        .await?;
    assert_eq!(
        recovered.record.expect("timeout resolution").disposition,
        InputMiddlewareDisposition::Rejected
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum MalformedReplacement {
    Empty,
    OverLimit,
}

#[test_matrix([MalformedReplacement::Empty, MalformedReplacement::OverLimit], [InputMiddlewareUnavailablePolicy::Pass, InputMiddlewareUnavailablePolicy::Reject])]
#[tokio::test]
async fn malformed_replacement_follows_unavailable_policy_and_is_journaled(
    replacement: MalformedReplacement,
    policy: InputMiddlewareUnavailablePolicy,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        create_final_assistant_message_sse_response("Done")?,
    )
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let _: InputMiddlewareAttachResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareAttach {
            request_id,
            params: InputMiddlewareAttachParams {
                thread_id: thread.id.clone(),
                timeout_ms: 500,
                on_unavailable: policy,
            },
        })
        .await?;
    let submitted = submit(&mut app_server, &thread.id).await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { request_id, params } = request else {
        panic!("expected middleware request: {request:?}");
    };
    let text = match replacement {
        MalformedReplacement::Empty => String::new(),
        MalformedReplacement::OverLimit => "é".repeat(32769),
    };
    app_server
        .send_response(request_id, json!({"type":"replace", "text":text}))
        .await?;
    let (disposition, selected_text) = match policy {
        InputMiddlewareUnavailablePolicy::Pass => {
            let _: TurnStartResponse = app_server.read_response(submitted).await?;
            app_server
                .read_stream_until_notification_message("turn/completed")
                .await?;
            assert!(
                mock.single_request()
                    .message_input_texts("user")
                    .iter()
                    .any(|text| text.contains("Original prompt"))
            );
            (
                InputMiddlewareDisposition::Passed,
                Some("Original prompt".into()),
            )
        }
        InputMiddlewareUnavailablePolicy::Reject => {
            let error = app_server
                .read_stream_until_error_message(RequestId::Integer(submitted))
                .await?;
            assert!(error.error.message.contains("input middleware unavailable"));
            assert!(mock.requests().is_empty());
            (InputMiddlewareDisposition::Rejected, None)
        }
    };
    let recovered: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
            },
        })
        .await?;
    assert_eq!(
        recovered.record,
        Some(InputMiddlewareRecord {
            thread_id: thread.id.clone(),
            input_id: params.input_id,
            original_text: "Original prompt".into(),
            selected_text,
            disposition,
            effect: None,
        })
    );
    let duplicate = submit(&mut app_server, &thread.id).await?;
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(duplicate))
        .await?;
    assert!(error.error.message.contains("already offered"));
    Ok(())
}

#[tokio::test]
async fn full_input_middleware_journal_refuses_admission_and_preserves_replay() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        create_final_assistant_message_sse_response("Unexpected dispatch")?,
    )
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let attachment = attach(&mut app_server, &thread.id).await?;
    let submitted = submit(&mut app_server, &thread.id).await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { request_id, params } = request else {
        panic!("expected middleware request: {request:?}");
    };
    app_server
        .send_response(
            request_id,
            json!({
                "type": "intercept", "operationId": "retained-effect"
            }),
        )
        .await?;
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(submitted))
        .await?;
    assert!(error.error.message.contains("input intercepted"));
    app_server
        .read_stream_until_notification_message("thread/input/resolved")
        .await?;
    let recovered: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
            },
        })
        .await?;
    let retained = recovered
        .record
        .expect("real committed record before filling journal");

    // Fill storage without fabricating admission records: JSON permits trailing
    // whitespace on the actual owner's record. This remains valid replay input
    // at the existing byte limit, unlike a sparse NUL-filled or corrupt fixture.
    const JOURNAL_BYTE_LIMIT: u64 = 160 * 1024 * 1024;
    let journal = home
        .path()
        .join("input-middleware")
        .join(format!("{}.jsonl", thread.id));
    let mut file = std::fs::OpenOptions::new().write(true).open(&journal)?;
    let persisted_len = file.metadata()?.len();
    assert!(persisted_len < JOURNAL_BYTE_LIMIT);
    file.seek(SeekFrom::End(-1))?;
    let padding = [b' '; 8192];
    let mut remaining = JOURNAL_BYTE_LIMIT - persisted_len;
    while remaining > 0 {
        let count = remaining.min(padding.len() as u64) as usize;
        file.write_all(&padding[..count])?;
        remaining -= count as u64;
    }
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    assert_eq!(std::fs::metadata(&journal)?.len(), JOURNAL_BYTE_LIMIT);

    let refused = app_server
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": thread.id,
                "clientUserMessageId": "full-journal",
                "input": [{"type":"text", "text":"Must not dispatch", "text_elements":[]}]
            })),
        )
        .await?;
    let request = app_server.read_stream_until_request_message().await?;
    let ServerRequest::InputMiddlewareRequest { request_id, .. } = request else {
        panic!("expected middleware request: {request:?}");
    };
    app_server
        .send_response(request_id, json!({"type":"pass"}))
        .await?;
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(refused))
        .await?;
    assert!(error.error.message.contains("input middleware unavailable"));
    assert!(
        mock.requests().is_empty(),
        "storage refusal must precede native dispatch"
    );
    let recovered: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id.clone(),
                input_id: "client:full-journal".into(),
            },
        })
        .await?;
    assert_eq!(recovered.record, None);

    // Receipt mutation shares the real append boundary and must not overwrite
    // the retained effect state when its additional record cannot fit either.
    let completion = app_server
        .send_request(
            "thread/input/complete",
            Some(serde_json::to_value(InputMiddlewareCompleteParams {
                thread_id: thread.id.clone(),
                input_id: params.input_id.clone(),
                operation_id: "retained-effect".into(),
                receipt: InputMiddlewareEffectReceipt {
                    status: InputMiddlewareEffectStatus::Succeeded,
                    summary: "External operation completed".into(),
                },
            })?),
        )
        .await?;
    let error = app_server
        .read_stream_until_error_message(RequestId::Integer(completion))
        .await?;
    assert!(
        error
            .error
            .message
            .contains("input middleware journal is full")
    );
    assert_eq!(std::fs::metadata(&journal)?.len(), JOURNAL_BYTE_LIMIT);
    let _: InputMiddlewareDetachResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareDetach {
            request_id,
            params: InputMiddlewareDetachParams {
                thread_id: thread.id.clone(),
                owner_id: attachment.owner_id,
            },
        })
        .await?;
    attach(&mut app_server, &thread.id).await?;
    let replayed: InputMiddlewareReadResponse = app_server
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread.id,
                input_id: params.input_id,
            },
        })
        .await?;
    assert_eq!(replayed.record, Some(retained));
    Ok(())
}

#[tokio::test]
async fn intercepted_decision_survives_app_server_restart() -> Result<()> {
    let codex_home = TempDir::new()?;
    let thread_id = {
        let mut app_server = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
        let ThreadStartResponse { thread, .. } = app_server
            .start_thread(ThreadStartParams::default())
            .await?;
        attach(&mut app_server, &thread.id).await?;
        let original_request = submit(&mut app_server, &thread.id).await?;
        let request = app_server.read_stream_until_request_message().await?;
        let ServerRequest::InputMiddlewareRequest { request_id, .. } = request else {
            panic!("expected input middleware request: {request:?}");
        };
        app_server
            .send_response(
                request_id,
                json!({"type":"intercept","operationId":"op-restart"}),
            )
            .await?;
        app_server
            .read_stream_until_error_message(RequestId::Integer(original_request))
            .await?;
        app_server.shutdown_gracefully().await?;
        thread.id
    };
    let mut resumed = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let request_id = resumed
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse = resumed.read_response(request_id).await?;
    attach(&mut resumed, &thread_id).await?;
    let record: InputMiddlewareReadResponse = resumed
        .request(|request_id| ClientRequest::InputMiddlewareRead {
            request_id,
            params: InputMiddlewareReadParams {
                thread_id: thread_id.clone(),
                input_id: "client:middleware-test-1".into(),
            },
        })
        .await?;
    assert_eq!(
        record.record.expect("recovered journal record").disposition,
        InputMiddlewareDisposition::Intercepted {
            operation_id: "op-restart".into()
        }
    );
    let duplicate = submit(&mut resumed, &thread_id).await?;
    let error = resumed
        .read_stream_until_error_message(RequestId::Integer(duplicate))
        .await?;
    assert!(error.error.message.contains("already offered"));
    Ok(())
}
