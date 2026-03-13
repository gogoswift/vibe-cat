use axum::{
    body::Bytes,
    http::StatusCode,
    routing::post,
    Router,
};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, KeyValue};
use prost::Message;
use serde_json::{json, Value};
use std::net::SocketAddr;

use crate::logger;

/// 从 OTel 日志属性中提取字符串值
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

/// 映射 Codex 事件名称到 LogEntry 的 event_type 和 summary
fn map_codex_event(
    event_name: &str,
    tool_name: Option<&str>,
    decision: Option<&str>,
    _call_id: Option<&str>,
) -> (String, String, Option<String>) {
    // 返回 (event_type, summary, adjusted_tool_name)
    match event_name {
        "codex.conversation_starts" => (
            "conversation_starts".to_string(),
            "Codex session started".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.api_request" => (
            "api_request".to_string(),
            "Codex API request".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.sse_event" => (
            "sse_event".to_string(),
            "Codex SSE event".to_string(),
            tool_name.map(|s| s.to_string()),
        ),
        "codex.tool_decision" => {
            match tool_name {
                Some("spawn_agent") => (
                    "SubagentStart".to_string(),
                    "SubagentStart".to_string(),
                    Some("spawn_agent".to_string()),
                ),
                Some("close_agent") => (
                    "SubagentStop".to_string(),
                    "SubagentStop".to_string(),
                    Some("close_agent".to_string()),
                ),
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

/// OTLP HTTP 接收器：POST /v1/logs
async fn receive_logs(body: Bytes) -> StatusCode {
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

                let event_name = extract_attr(attrs, "event.name")
                    .unwrap_or_else(|| "unknown".to_string());
                let conversation_id = extract_attr(attrs, "conversation.id")
                    .unwrap_or_else(|| "unknown".to_string());
                let tool_name = extract_attr(attrs, "tool_name");
                let decision = extract_attr(attrs, "decision");
                let call_id = extract_attr(attrs, "call_id");

                let (event_type, summary, adjusted_tool_name) = map_codex_event(
                    &event_name,
                    tool_name.as_deref(),
                    decision.as_deref(),
                    call_id.as_deref(),
                );

                // 构建 raw JSON，包含所有属性
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

                // SubagentStart/SubagentStop 需要 agent_id
                if event_type == "SubagentStart" || event_type == "SubagentStop" {
                    if let Some(ref cid) = call_id {
                        raw_map.insert("agent_id".to_string(), json!(cid));
                    }
                }

                let raw = Value::Object(raw_map);

                if let Err(e) = logger::write_codex_event(
                    &event_type,
                    &conversation_id,
                    adjusted_tool_name.as_deref(),
                    &summary,
                    raw,
                ) {
                    eprintln!("[server] write event error: {}", e);
                }
            }
        }
    }

    StatusCode::OK
}

/// 启动 OTel HTTP 接收服务器
pub async fn run_server(port: u16) {
    let app = Router::new().route("/v1/logs", post(receive_logs));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    eprintln!("[server] Codex OTel receiver listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Cannot bind to address");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}
