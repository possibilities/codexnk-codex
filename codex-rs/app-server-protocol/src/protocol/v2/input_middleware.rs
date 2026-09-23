use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Opt-in owner of human-input decisions for one loaded thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareAttachParams {
    pub thread_id: String,
    pub timeout_ms: u32,
    pub on_unavailable: InputMiddlewareUnavailablePolicy,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum InputMiddlewareUnavailablePolicy {
    Pass,
    Reject,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareAttachResponse {
    pub thread_id: String,
    pub owner_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareDetachParams {
    pub thread_id: String,
    pub owner_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareDetachResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareRequestParams {
    pub thread_id: String,
    pub input_id: String,
    pub origin: InputMiddlewareOrigin,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum InputMiddlewareOrigin {
    Client,
    Realtime,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum InputMiddlewareRequestResponse {
    Pass,
    Replace {
        text: String,
    },
    Intercept {
        #[serde(rename = "operationId")]
        #[ts(rename = "operationId")]
        operation_id: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareResolvedNotification {
    pub thread_id: String,
    pub input_id: String,
    pub disposition: InputMiddlewareDisposition,
    pub effect: Option<InputMiddlewareEffectReceipt>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum InputMiddlewareDisposition {
    Passed,
    Replaced,
    Intercepted {
        #[serde(rename = "operationId")]
        #[ts(rename = "operationId")]
        operation_id: String,
    },
    Rejected,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareReadParams {
    pub thread_id: String,
    pub input_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareReadResponse {
    pub record: Option<InputMiddlewareRecord>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareRecord {
    pub thread_id: String,
    pub input_id: String,
    pub original_text: String,
    pub selected_text: Option<String>,
    pub disposition: InputMiddlewareDisposition,
    pub effect: Option<InputMiddlewareEffectReceipt>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareEffectReceipt {
    pub status: InputMiddlewareEffectStatus,
    pub summary: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum InputMiddlewareEffectStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareCompleteParams {
    pub thread_id: String,
    pub input_id: String,
    pub operation_id: String,
    pub receipt: InputMiddlewareEffectReceipt,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct InputMiddlewareCompleteResponse {
    pub record: InputMiddlewareRecord,
}
