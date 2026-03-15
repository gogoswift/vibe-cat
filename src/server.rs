use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, KeyValue};
use prost::Message;
use serde_json::{json, Value};
use std::net::SocketAddr;

use crate::logger;

/// 从 protobuf OTel 日志属性中提取字符串值
fn extract_attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs.iter().find(|kv| kv.key == key).and_then(|kv| {
        kv.value.as_ref().and_then(|v| {
            v.value.as_ref().and_then(|val| match val {
                any_value::Value::StringValue(s) => Some(s.clone()),
                _ => None,
            })
        })
    })
}

/// 从 JSON OTel 日志属性中提取字符串值
fn extract_json_attr(attrs: &[Value], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|kv| kv.get("key").and_then(|k| k.as_str()) == Some(key))
        .and_then(|kv| {
            kv.get("value")
                .and_then(|v| v.get("stringValue"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
}

/// 映射 Codex 事件名称到 LogEntry 的 event_type 和 summary
fn map_codex_event(
    event_name: &str,
    tool_name: Option<&str>,
    decision: Option<&str>,
    _call_id: Option<&str>,
    event_kind: Option<&str>,
) -> (String, String, Option<String>) {
    // 返回 (event_type, summary, adjusted_tool_name)
    match event_name {
        "codex.conversation_starts" => (
            "SessionStart".to_string(),
            "Codex session started".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.user_prompt" => (
            "UserPromptSubmit".to_string(),
            "Codex user prompt".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.api_request" => (
            "api_request".to_string(),
            "Codex API request".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.sse_event" | "codex.websocket_event" => match event_kind {
            Some("response.completed") | Some("[DONE]") => (
                "Stop".to_string(),
                format!("Codex response completed"),
                tool_name.map(|s| s.to_string()),
            ),
            Some("response.failed") => (
                "Stop".to_string(),
                format!("Codex response failed"),
                tool_name.map(|s| s.to_string()),
            ),
            Some(kind) => (
                "sse_event".to_string(),
                format!("Codex: {}", kind),
                tool_name.map(|s| s.to_string()),
            ),
            None => (
                "sse_event".to_string(),
                "Codex SSE event".to_string(),
                tool_name.map(|s| s.to_string()),
            ),
        },
        "codex.websocket_request" => (
            "api_request".to_string(),
            "Codex WebSocket request".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.tool_decision" => {
            // denied/abort → PermissionRequest（等待用户介入）
            match decision {
                Some("denied") | Some("abort") => {
                    let tn = tool_name.unwrap_or("unknown");
                    (
                        "PermissionRequest".to_string(),
                        format!("{}: {}", tn, decision.unwrap()),
                        Some(tn.to_string()),
                    )
                }
                _ => match tool_name {
                    Some("spawn_agent") => {
                        let dec = decision.unwrap_or("unknown");
                        (
                            "SubagentStart".to_string(),
                            format!("Codex subagent spawned ({})", dec),
                            Some("spawn_agent".to_string()),
                        )
                    }
                    Some("close_agent") => {
                        let dec = decision.unwrap_or("unknown");
                        (
                            "SubagentStop".to_string(),
                            format!("Codex subagent stopped ({})", dec),
                            Some("close_agent".to_string()),
                        )
                    }
                    Some(tn) => {
                        let dec = decision.unwrap_or("unknown");
                        (
                            "tool_decision".to_string(),
                            format!("{}: {}", tn, dec),
                            Some(tn.to_string()),
                        )
                    }
                    None => {
                        let dec = decision.unwrap_or("unknown");
                        (
                            "tool_decision".to_string(),
                            format!("unknown: {}", dec),
                            None,
                        )
                    }
                },
            }
        }
        "codex.tool_result" => {
            let tn = tool_name.unwrap_or("unknown");
            (
                "tool_result".to_string(),
                format!("{} completed", tn),
                Some(tn.to_string()),
            )
        }
        _ => (
            event_name.to_string(),
            format!("Codex event: {}", event_name),
            tool_name.map(|s| s.to_string()),
        ),
    }
}

/// 处理提取出的事件字段，写入日志
fn process_event(
    event_name: &str,
    conversation_id: &str,
    tool_name: Option<&str>,
    decision: Option<&str>,
    call_id: Option<&str>,
    event_kind: Option<&str>,
    mut raw_map: serde_json::Map<String, Value>,
) {
    let (event_type, summary, adjusted_tool_name) =
        map_codex_event(event_name, tool_name, decision, call_id, event_kind);

    // SubagentStart/SubagentStop 需要 agent_id
    if event_type == "SubagentStart" || event_type == "SubagentStop" {
        if let Some(cid) = call_id {
            raw_map.insert("agent_id".to_string(), json!(cid));
        }
    }
    // cx PermissionRequest 不设 agent_id（保持空字符串）
    // 这样同一 conversation 的后续事件会自然覆盖，解除阻塞

    let raw = Value::Object(raw_map);

    if let Err(e) = logger::write_codex_event(
        &event_type,
        conversation_id,
        adjusted_tool_name.as_deref(),
        &summary,
        raw,
    ) {
        eprintln!("[server] write event error: {}", e);
    }
}

/// 处理 protobuf 编码的 OTLP 日志
fn receive_logs_protobuf(body: Bytes) -> StatusCode {
    let request = match ExportLogsServiceRequest::decode(body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[server] protobuf decode error: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    for resource_logs in &request.resource_logs {
        for scope_logs in &resource_logs.scope_logs {
            for log_record in &scope_logs.log_records {
                let attrs = &log_record.attributes;

                let event_name =
                    extract_attr(attrs, "event.name").unwrap_or_else(|| "unknown".to_string());
                let conversation_id =
                    extract_attr(attrs, "conversation.id").unwrap_or_else(|| "unknown".to_string());
                let tool_name = extract_attr(attrs, "tool_name");
                let decision = extract_attr(attrs, "decision");
                let call_id = extract_attr(attrs, "call_id");
                let event_kind = extract_attr(attrs, "event.kind");

                let mut raw_map = serde_json::Map::new();
                raw_map.insert("event_name".to_string(), json!(event_name));
                raw_map.insert("conversation_id".to_string(), json!(conversation_id));
                for kv in attrs {
                    if let Some(ref v) = kv.value {
                        if let Some(ref val) = v.value {
                            let json_val = match val {
                                any_value::Value::StringValue(s) => json!(s),
                                any_value::Value::IntValue(i) => json!(i),
                                any_value::Value::DoubleValue(d) => json!(d),
                                any_value::Value::BoolValue(b) => json!(b),
                                _ => json!(format!("{:?}", val)),
                            };
                            raw_map.insert(kv.key.clone(), json_val);
                        }
                    }
                }

                process_event(
                    &event_name,
                    &conversation_id,
                    tool_name.as_deref(),
                    decision.as_deref(),
                    call_id.as_deref(),
                    event_kind.as_deref(),
                    raw_map,
                );
            }
        }
    }

    StatusCode::OK
}

/// 处理 JSON 编码的 OTLP 日志（Codex 使用此格式）
fn receive_logs_json(body: &Bytes) -> StatusCode {
    let json: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[server] JSON decode error: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    let resource_logs = match json.get("resourceLogs").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return StatusCode::OK,
    };

    for rl in resource_logs {
        let scope_logs = match rl.get("scopeLogs").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => continue,
        };
        for sl in scope_logs {
            let log_records = match sl.get("logRecords").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };
            for lr in log_records {
                let empty_attrs = vec![];
                let attrs = lr
                    .get("attributes")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty_attrs);

                let event_name =
                    extract_json_attr(attrs, "event.name").unwrap_or_else(|| "unknown".to_string());
                let conversation_id = extract_json_attr(attrs, "conversation.id")
                    .unwrap_or_else(|| "unknown".to_string());
                let tool_name = extract_json_attr(attrs, "tool_name");
                let decision = extract_json_attr(attrs, "decision");
                let call_id = extract_json_attr(attrs, "call_id");
                let event_kind = extract_json_attr(attrs, "event.kind");

                let mut raw_map = serde_json::Map::new();
                raw_map.insert("event_name".to_string(), json!(event_name));
                raw_map.insert("conversation_id".to_string(), json!(conversation_id));
                for kv in attrs {
                    let key = match kv.get("key").and_then(|k| k.as_str()) {
                        Some(k) => k,
                        None => continue,
                    };
                    let value = match kv.get("value") {
                        Some(v) => v,
                        None => continue,
                    };
                    // OTLP JSON: stringValue, intValue (as string), doubleValue, boolValue
                    let json_val =
                        if let Some(s) = value.get("stringValue").and_then(|x| x.as_str()) {
                            json!(s)
                        } else if let Some(s) = value.get("intValue").and_then(|x| x.as_str()) {
                            // OTLP JSON 中 int64 编码为字符串
                            match s.parse::<i64>() {
                                Ok(i) => json!(i),
                                Err(_) => json!(s),
                            }
                        } else if let Some(i) = value.get("intValue").and_then(|x| x.as_i64()) {
                            json!(i)
                        } else if let Some(d) = value.get("doubleValue").and_then(|x| x.as_f64()) {
                            json!(d)
                        } else if let Some(b) = value.get("boolValue").and_then(|x| x.as_bool()) {
                            json!(b)
                        } else {
                            continue;
                        };
                    raw_map.insert(key.to_string(), json_val);
                }

                process_event(
                    &event_name,
                    &conversation_id,
                    tool_name.as_deref(),
                    decision.as_deref(),
                    call_id.as_deref(),
                    event_kind.as_deref(),
                    raw_map,
                );
            }
        }
    }

    StatusCode::OK
}

/// OTLP HTTP 接收器：POST /v1/logs（支持 protobuf 和 JSON）
async fn receive_logs(headers: HeaderMap, body: Bytes) -> StatusCode {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/x-protobuf") {
        receive_logs_protobuf(body)
    } else {
        // 默认尝试 JSON（Codex 发送 JSON 但可能不带正确的 Content-Type）
        receive_logs_json(&body)
    }
}

/// 启动 OTel HTTP 接收服务器
pub async fn run_server(port: u16) {
    let app = Router::new().route("/v1/logs", post(receive_logs));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[server] Cannot bind to 127.0.0.1:{} ({}). Another instance may be running.",
                port, e
            );
            return;
        }
    };

    eprintln!("[server] Codex OTel receiver listening on http://{}", addr);

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("[server] Server error: {}", e);
    }
}
