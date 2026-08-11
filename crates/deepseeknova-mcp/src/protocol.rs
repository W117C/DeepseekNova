//! MCP protocol-version negotiation.
//!
//! The MCP protocol evolves through versioned releases with additive
//! capabilities. During `initialize` the client offers the newest version it
//! understands; if the server rejects it (JSON-RPC `-32602` with a
//! `data.supported` array), the client retries with the highest mutually
//! supported version. The version the server echoes back in
//! `InitializeResult.protocolVersion` is authoritative.
//!
//! The pure helpers here are shared by both the stdio and HTTP transports.

use serde_json::Value;

/// MCP protocol version 2024-11-05 (initial release; legacy SSE transport).
pub const PROTOCOL_VERSION_2024_11_05: &str = "2024-11-05";

/// MCP protocol version 2025-03-26 (introduces streamable HTTP).
pub const PROTOCOL_VERSION_2025_03_26: &str = "2025-03-26";

/// MCP protocol version 2025-06-18 (current).
pub const PROTOCOL_VERSION_2025_06_18: &str = "2025-06-18";

/// Protocol versions this client understands, newest first. A server may
/// support a subset; the highest shared version wins.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    PROTOCOL_VERSION_2025_06_18,
    PROTOCOL_VERSION_2025_03_26,
    PROTOCOL_VERSION_2024_11_05,
];

/// The version offered on the first `initialize` attempt.
pub fn preferred_protocol_version() -> &'static str {
    SUPPORTED_PROTOCOL_VERSIONS[0]
}

/// Whether `version` is in the client's supported set.
pub fn is_supported(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// The highest version in `server_supported` that the client also supports,
/// or `None` when the two sets are disjoint.
pub fn highest_mutual(server_supported: &[String]) -> Option<String> {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|v| server_supported.iter().any(|s| s.as_str() == *v))
        .map(|v| v.to_string())
}

/// True when a JSON-RPC error response signals an unsupported protocol version
/// (code `-32602` with a `data.supported` array).
pub fn is_version_mismatch(response: &Value) -> bool {
    let Some(err) = response.get("error") else {
        return false;
    };
    err.get("code").and_then(|c| c.as_i64()) == Some(-32602)
        && err
            .get("data")
            .and_then(|d| d.get("supported"))
            .and_then(|s| s.as_array())
            .is_some()
}

/// Extract the server's supported protocol versions from a version-mismatch
/// error response.
pub fn extract_supported_versions(response: &Value) -> Vec<String> {
    response["error"]["data"]["supported"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a JSON-RPC message from the server is a *request* directed at the
/// client rather than a response to one of our pending requests or a
/// notification.
///
/// A server request carries both `method` and `id` and no `result`/`error`
/// field. MCP clients that advertise the `roots` capability must answer
/// `roots/list`; other server requests may be answered with an error or
/// ignored — but never mistaken for a response to a pending request.
pub fn is_server_request(message: &Value) -> bool {
    message.get("method").is_some()
        && message.get("id").is_some()
        && message.get("result").is_none()
        && message.get("error").is_none()
}

/// Build a JSON-RPC response object answering a server request with `result`,
/// echoing the request's `id` (which may be any JSON value).
pub fn build_server_response(request: &Value, result: Value) -> Value {
    let mut resp = serde_json::json!({ "jsonrpc": "2.0", "result": result });
    if let Value::Object(map) = &mut resp {
        if let Some(id) = request.get("id") {
            map.insert("id".into(), id.clone());
        }
    }
    resp
}

/// Build a JSON-RPC error response object answering a server request.
pub fn build_server_error(request: &Value, code: i64, message: String) -> Value {
    let mut resp =
        serde_json::json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message } });
    if let Value::Object(map) = &mut resp {
        if let Some(id) = request.get("id") {
            map.insert("id".into(), id.clone());
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preferred_version_is_newest() {
        assert_eq!(preferred_protocol_version(), PROTOCOL_VERSION_2025_06_18);
    }

    #[test]
    fn is_supported_accepts_known_versions_only() {
        assert!(is_supported(PROTOCOL_VERSION_2025_06_18));
        assert!(is_supported(PROTOCOL_VERSION_2025_03_26));
        assert!(is_supported(PROTOCOL_VERSION_2024_11_05));
        assert!(!is_supported("2020-01-01"));
    }

    #[test]
    fn highest_mutual_picks_newest_shared() {
        let server = vec![
            PROTOCOL_VERSION_2024_11_05.to_string(),
            PROTOCOL_VERSION_2025_03_26.to_string(),
        ];
        assert_eq!(
            highest_mutual(&server).as_deref(),
            Some(PROTOCOL_VERSION_2025_03_26)
        );
    }

    #[test]
    fn highest_mutual_returns_none_when_disjoint() {
        let server = vec!["2099-01-01".to_string()];
        assert_eq!(highest_mutual(&server), None);
    }

    #[test]
    fn version_mismatch_detection_and_extraction() {
        let mismatch = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {
                "code": -32602,
                "message": "Unsupported protocol version",
                "data": {"supported": ["2025-03-26", "2024-11-05"]}
            }
        });
        assert!(is_version_mismatch(&mismatch));
        assert_eq!(
            extract_supported_versions(&mismatch),
            vec!["2025-03-26".to_string(), "2024-11-05".to_string()]
        );

        // A generic server error is not a version mismatch.
        let other = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32000, "message": "boom"}
        });
        assert!(!is_version_mismatch(&other));
        assert!(extract_supported_versions(&other).is_empty());
    }

    #[test]
    fn server_request_classification_distinguishes_the_three_message_kinds() {
        // A server request (roots/list) — id + method, no result/error.
        let request = json!({
            "jsonrpc": "2.0", "id": 42, "method": "roots/list", "params": {}
        });
        assert!(is_server_request(&request));

        // A response to one of our pending requests — id + result, no method.
        let response = json!({"jsonrpc": "2.0", "id": 1, "result": {"pong": true}});
        assert!(!is_server_request(&response));

        // An error response — id + error, no method.
        let error = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "x"}});
        assert!(!is_server_request(&error));

        // A notification — method, no id.
        let notif = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(!is_server_request(&notif));
    }

    #[test]
    fn server_response_builders_echo_the_request_id() {
        let request = json!({
            "jsonrpc": "2.0", "id": "abc-7", "method": "roots/list", "params": {}
        });
        let resp = build_server_response(&request, json!({"roots": []}));
        assert_eq!(resp["id"], "abc-7");
        assert_eq!(resp["result"], json!({"roots": []}));
        assert!(resp.get("error").is_none());

        let err = build_server_error(&request, -32601, "Method not found".into());
        assert_eq!(err["id"], "abc-7");
        assert_eq!(err["error"]["code"], -32601);
        assert!(err.get("result").is_none());
    }
}
