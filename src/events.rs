use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 使用 serde tag 按 hook_event_name 自动分发
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum HookEvent {
    SessionStart(SessionStartData),
    InstructionsLoaded(InstructionsLoadedData),
    UserPromptSubmit(UserPromptSubmitData),
    PreToolUse(PreToolUseData),
    PermissionRequest(PermissionRequestData),
    PostToolUse(PostToolUseData),
    PostToolUseFailure(PostToolUseFailureData),
    Notification(NotificationData),
    SubagentStart(SubagentStartData),
    SubagentStop(SubagentStopData),
    Stop(StopData),
    TeammateIdle(TeammateIdleData),
    TaskCompleted(TaskCompletedData),
    ConfigChange(ConfigChangeData),
    WorktreeCreate(WorktreeCreateData),
    WorktreeRemove(WorktreeRemoveData),
    PreCompact(PreCompactData),
    SessionEnd(SessionEndData),
}

// ============================================================
// 各事件的数据结构
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionsLoadedData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub load_reason: Option<String>,
    #[serde(default)]
    pub globs: Option<Vec<String>>,
    #[serde(default)]
    pub trigger_file_path: Option<String>,
    #[serde(default)]
    pub parent_file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPromptSubmitData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreToolUseData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequestData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub permission_suggestions: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUseData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUseFailureData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub is_interrupt: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStartData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    // 特有字段（agent_id/agent_type 在这里是必有的）
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStopData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    // 特有字段
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub agent_transcript_path: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateIdleData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub teammate_name: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletedData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub task_subject: Option<String>,
    #[serde(default)]
    pub task_description: Option<String>,
    #[serde(default)]
    pub teammate_name: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeCreateData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRemoveData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreCompactData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub custom_instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEndData {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    // 特有字段
    #[serde(default)]
    pub reason: Option<String>,
}

// ============================================================
// 辅助方法：提取事件摘要
// ============================================================

impl HookEvent {
    /// 事件类型名称
    pub fn event_type(&self) -> &str {
        match self {
            HookEvent::SessionStart(_) => "SessionStart",
            HookEvent::InstructionsLoaded(_) => "InstructionsLoaded",
            HookEvent::UserPromptSubmit(_) => "UserPromptSubmit",
            HookEvent::PreToolUse(_) => "PreToolUse",
            HookEvent::PermissionRequest(_) => "PermissionRequest",
            HookEvent::PostToolUse(_) => "PostToolUse",
            HookEvent::PostToolUseFailure(_) => "PostToolUseFailure",
            HookEvent::Notification(_) => "Notification",
            HookEvent::SubagentStart(_) => "SubagentStart",
            HookEvent::SubagentStop(_) => "SubagentStop",
            HookEvent::Stop(_) => "Stop",
            HookEvent::TeammateIdle(_) => "TeammateIdle",
            HookEvent::TaskCompleted(_) => "TaskCompleted",
            HookEvent::ConfigChange(_) => "ConfigChange",
            HookEvent::WorktreeCreate(_) => "WorktreeCreate",
            HookEvent::WorktreeRemove(_) => "WorktreeRemove",
            HookEvent::PreCompact(_) => "PreCompact",
            HookEvent::SessionEnd(_) => "SessionEnd",
        }
    }

    /// 会话 ID
    pub fn session_id(&self) -> &str {
        match self {
            HookEvent::SessionStart(d) => &d.session_id,
            HookEvent::InstructionsLoaded(d) => &d.session_id,
            HookEvent::UserPromptSubmit(d) => &d.session_id,
            HookEvent::PreToolUse(d) => &d.session_id,
            HookEvent::PermissionRequest(d) => &d.session_id,
            HookEvent::PostToolUse(d) => &d.session_id,
            HookEvent::PostToolUseFailure(d) => &d.session_id,
            HookEvent::Notification(d) => &d.session_id,
            HookEvent::SubagentStart(d) => &d.session_id,
            HookEvent::SubagentStop(d) => &d.session_id,
            HookEvent::Stop(d) => &d.session_id,
            HookEvent::TeammateIdle(d) => &d.session_id,
            HookEvent::TaskCompleted(d) => &d.session_id,
            HookEvent::ConfigChange(d) => &d.session_id,
            HookEvent::WorktreeCreate(d) => &d.session_id,
            HookEvent::WorktreeRemove(d) => &d.session_id,
            HookEvent::PreCompact(d) => &d.session_id,
            HookEvent::SessionEnd(d) => &d.session_id,
        }
    }

    /// 工具名（仅工具相关事件）
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            HookEvent::PreToolUse(d) => d.tool_name.as_deref(),
            HookEvent::PermissionRequest(d) => d.tool_name.as_deref(),
            HookEvent::PostToolUse(d) => d.tool_name.as_deref(),
            HookEvent::PostToolUseFailure(d) => d.tool_name.as_deref(),
            _ => None,
        }
    }

    /// 生成人类可读的摘要
    pub fn summary(&self) -> String {
        fn truncate(s: &str, max: usize) -> String {
            if s.len() <= max {
                s.to_string()
            } else {
                format!("{}...", &s[..max])
            }
        }

        match self {
            HookEvent::SessionStart(d) => {
                format!(
                    "Session started ({})",
                    d.source.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::InstructionsLoaded(d) => {
                format!(
                    "Instructions loaded: {}",
                    d.file_path.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::UserPromptSubmit(d) => {
                format!(
                    "User prompt: {}",
                    truncate(d.prompt.as_deref().unwrap_or(""), 80)
                )
            }
            HookEvent::PreToolUse(d) => {
                let tool = d.tool_name.as_deref().unwrap_or("unknown");
                let detail = d
                    .tool_input
                    .as_ref()
                    .and_then(|v| {
                        v.get("command")
                            .or_else(|| v.get("file_path"))
                            .or_else(|| v.get("pattern"))
                            .or_else(|| v.get("url"))
                            .or_else(|| v.get("query"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("");
                format!("{}: {}", tool, truncate(detail, 80))
            }
            HookEvent::PermissionRequest(d) => {
                format!(
                    "Permission requested for {}",
                    d.tool_name.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::PostToolUse(d) => {
                format!(
                    "{} completed",
                    d.tool_name.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::PostToolUseFailure(d) => {
                format!(
                    "{} FAILED: {}",
                    d.tool_name.as_deref().unwrap_or("unknown"),
                    truncate(d.error.as_deref().unwrap_or(""), 80)
                )
            }
            HookEvent::Notification(d) => {
                format!(
                    "[{}] {}",
                    d.notification_type.as_deref().unwrap_or("unknown"),
                    truncate(d.message.as_deref().unwrap_or(""), 80)
                )
            }
            HookEvent::SubagentStart(d) => {
                format!(
                    "Subagent started: {}",
                    d.agent_type.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::SubagentStop(d) => {
                format!(
                    "Subagent stopped: {}",
                    d.agent_type.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::Stop(_) => "Agent stopped".to_string(),
            HookEvent::TeammateIdle(d) => {
                format!(
                    "Teammate idle: {}",
                    d.teammate_name.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::TaskCompleted(d) => {
                format!(
                    "Task completed: {}",
                    truncate(d.task_subject.as_deref().unwrap_or(""), 80)
                )
            }
            HookEvent::ConfigChange(d) => {
                format!(
                    "Config changed: {}",
                    d.source.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::WorktreeCreate(d) => {
                format!(
                    "Worktree created: {}",
                    d.name.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::WorktreeRemove(d) => {
                format!(
                    "Worktree removed: {}",
                    d.worktree_path.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::PreCompact(d) => {
                format!(
                    "Compaction ({})",
                    d.trigger.as_deref().unwrap_or("unknown")
                )
            }
            HookEvent::SessionEnd(d) => {
                format!(
                    "Session ended ({})",
                    d.reason.as_deref().unwrap_or("unknown")
                )
            }
        }
    }
}
