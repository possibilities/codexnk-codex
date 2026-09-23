use super::connection_handling_websocket::connect_websocket;
use super::connection_handling_websocket::read_error_for_id;
use super::connection_handling_websocket::read_jsonrpc_message;
use super::connection_handling_websocket::read_response_for_id;
use super::connection_handling_websocket::send_jsonrpc;
use super::connection_handling_websocket::send_request;
use super::connection_handling_websocket::spawn_websocket_server;
use anyhow::Context;
use anyhow::Result;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn websocket_owner_decides_for_another_client_and_cannot_be_spoofed() -> Result<()> {
    let codex_home = TempDir::new()?;
    let (mut process, bind_addr) = spawn_websocket_server(codex_home.path()).await?;
    let mut owner = connect_websocket(bind_addr).await?;
    let mut submitter = connect_websocket(bind_addr).await?;
    for (client, id) in [(&mut owner, 1), (&mut submitter, 2)] {
        send_request(
            client,
            "initialize",
            id,
            Some(json!({
                "clientInfo": {"name": "middleware-test", "version": "0.1.0"},
                "capabilities": {"experimentalApi": true}
            })),
        )
        .await?;
        read_response_for_id(client, id).await?;
    }
    send_request(&mut owner, "thread/start", 3, Some(json!({}))).await?;
    let thread_id = read_response_for_id(&mut owner, 3).await?.result["thread"]["id"]
        .as_str()
        .context("thread id")?
        .to_string();
    send_request(
        &mut owner,
        "thread/input/middleware/attach",
        4,
        Some(json!({
            "threadId": thread_id,
            "timeoutMs": 1000,
            "onUnavailable": "reject"
        })),
    )
    .await?;
    read_response_for_id(&mut owner, 4).await?;
    send_request(
        &mut submitter,
        "turn/start",
        5,
        Some(json!({
            "threadId": thread_id,
            "clientUserMessageId": "ws-owner-test-1",
            "input": [{"type":"text","text":"Do not deliver","text_elements":[]}]
        })),
    )
    .await?;
    let request = loop {
        if let JSONRPCMessage::Request(request) = read_jsonrpc_message(&mut owner).await?
            && request.method == "thread/input/requestDisposition"
        {
            break request;
        }
    };
    assert_eq!(
        request.params.as_ref().context("candidate params")?["origin"],
        "client"
    );
    send_jsonrpc(
        &mut submitter,
        JSONRPCMessage::Response(JSONRPCResponse {
            id: request.id.clone(),
            result: json!({"type":"pass"}),
        }),
    )
    .await?;
    send_jsonrpc(
        &mut owner,
        JSONRPCMessage::Response(JSONRPCResponse {
            id: request.id,
            result: json!({"type":"intercept","operationId":"op-owner"}),
        }),
    )
    .await?;
    let error = read_error_for_id(&mut submitter, 5).await?;
    assert!(error.error.message.contains("input intercepted"));
    process
        .kill()
        .await
        .context("failed to stop websocket app-server")?;
    Ok(())
}
