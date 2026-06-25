use serde_json::{Map, Value};

use crate::event::{AgentEvent, EventAdapter};
use crate::tmux::PI_AGENT;
use crate::tool_name::CanonicalTool;

use super::{json_str, json_value_or_null, optional_str};

pub struct PiAdapter;

/// pi tool IDs are lowercase (`bash`, `read`, …) but the internal label
/// extractor in `src/cli/label.rs` keys off Claude-style PascalCase names.
/// Normalise here so the activity log and its strategy table share a single
/// vocabulary across agents (mirrors `opencode::normalize_tool_name`).
fn normalize_tool_name(raw: &str) -> String {
    let canonical = match raw {
        "bash" => CanonicalTool::Bash,
        "read" => CanonicalTool::Read,
        "write" => CanonicalTool::Write,
        "edit" => CanonicalTool::Edit,
        "glob" => CanonicalTool::Glob,
        "grep" => CanonicalTool::Grep,
        "fetch_content" => CanonicalTool::WebFetch,
        "web_search" => CanonicalTool::WebSearch,
        "todo" => CanonicalTool::TodoWrite,
        other => return other.to_string(),
    };
    canonical.as_str().to_string()
}

/// Translate pi's tool arguments into the snake_case keys the Claude-style
/// label extractor expects. Keys are added alongside the originals rather
/// than replacing them so downstream consumers that want the raw payload
/// still see it.
fn normalize_tool_input(tool_name: &str, input: Value) -> Value {
    let Value::Object(mut map) = input else {
        return input;
    };
    let rewrites: &[(&str, &str)] = match tool_name {
        "Read" | "Write" | "Edit" => &[("path", "file_path"), ("filePath", "file_path")],
        _ => &[],
    };
    copy_keys(&mut map, rewrites);
    Value::Object(map)
}

fn copy_keys(map: &mut Map<String, Value>, pairs: &[(&str, &str)]) {
    for (src, dst) in pairs {
        if map.contains_key(*dst) {
            continue;
        }
        if let Some(value) = map.get(*src).cloned() {
            map.insert((*dst).to_string(), value);
        }
    }
}

impl EventAdapter for PiAdapter {
    fn parse(&self, event_name: &str, input: &Value) -> Option<AgentEvent> {
        match event_name {
            "session-start" => Some(AgentEvent::SessionStart {
                agent: PI_AGENT.into(),
                cwd: json_str(input, "cwd").into(),
                permission_mode: String::new(),
                source: json_str(input, "source").into(),
                worktree: None,
                agent_id: None,
                session_id: optional_str(input, "session_id"),
            }),
            // pi fires `session_shutdown` on quit/reload/replace, which the
            // extension bridges to `session-end`. Like Claude (and unlike
            // Codex/OpenCode), this drives pane teardown so pi panes are not
            // routed through the process-scan cull in `tmux::query`.
            "session-end" => Some(AgentEvent::SessionEnd {
                end_reason: json_str(input, "end_reason").into(),
            }),
            "user-prompt-submit" => Some(AgentEvent::UserPromptSubmit {
                agent: PI_AGENT.into(),
                cwd: json_str(input, "cwd").into(),
                permission_mode: String::new(),
                prompt: json_str(input, "prompt").into(),
                worktree: None,
                agent_id: None,
                session_id: optional_str(input, "session_id"),
            }),
            "stop" => Some(AgentEvent::Stop {
                agent: PI_AGENT.into(),
                cwd: json_str(input, "cwd").into(),
                permission_mode: String::new(),
                last_message: json_str(input, "last_message").into(),
                response: None,
                worktree: None,
                agent_id: None,
                session_id: optional_str(input, "session_id"),
            }),
            "activity-log" => {
                let raw_name = json_str(input, "tool_name");
                if raw_name.is_empty() {
                    return None;
                }
                let tool_name = normalize_tool_name(raw_name);
                let tool_input =
                    normalize_tool_input(&tool_name, json_value_or_null(input, "tool_input"));
                Some(AgentEvent::ActivityLog {
                    tool_name,
                    tool_input,
                    tool_response: json_value_or_null(input, "tool_response"),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::resolve_adapter;
    use serde_json::json;

    #[test]
    fn session_start_sets_pi_agent_and_session_id() {
        let adapter = PiAdapter;
        let event = adapter
            .parse(
                "session-start",
                &json!({"cwd": "/home/user", "source": "new", "session_id": "pi-1"}),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionStart {
                agent: PI_AGENT.into(),
                cwd: "/home/user".into(),
                permission_mode: "".into(),
                source: "new".into(),
                worktree: None,
                agent_id: None,
                session_id: Some("pi-1".into()),
            }
        );
    }

    #[test]
    fn user_prompt_submit() {
        let adapter = PiAdapter;
        let event = adapter
            .parse(
                "user-prompt-submit",
                &json!({"cwd": "/tmp", "prompt": "fix the bug", "session_id": "pi-2"}),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::UserPromptSubmit {
                agent: PI_AGENT.into(),
                cwd: "/tmp".into(),
                permission_mode: "".into(),
                prompt: "fix the bug".into(),
                worktree: None,
                agent_id: None,
                session_id: Some("pi-2".into()),
            }
        );
    }

    #[test]
    fn stop_carries_last_message() {
        let adapter = PiAdapter;
        let event = adapter
            .parse(
                "stop",
                &json!({"cwd": "/tmp", "last_message": "done", "session_id": "pi-3"}),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::Stop {
                agent: PI_AGENT.into(),
                cwd: "/tmp".into(),
                permission_mode: "".into(),
                last_message: "done".into(),
                response: None,
                worktree: None,
                agent_id: None,
                session_id: Some("pi-3".into()),
            }
        );
    }

    #[test]
    fn session_end_supported() {
        let adapter = PiAdapter;
        let event = adapter
            .parse("session-end", &json!({"end_reason": "quit"}))
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionEnd {
                end_reason: "quit".into(),
            }
        );
    }

    #[test]
    fn activity_log_normalizes_lowercase_read_and_path() {
        let adapter = PiAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "tool_name": "read",
                    "tool_input": {"path": "/repo/src/main.rs"},
                    "tool_response": {"ok": true}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog {
                tool_name,
                tool_input,
                ..
            } => {
                assert_eq!(tool_name, "Read");
                assert_eq!(
                    tool_input.get("file_path").and_then(|v| v.as_str()),
                    Some("/repo/src/main.rs")
                );
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn activity_log_empty_tool_name_rejected() {
        assert!(PiAdapter.parse("activity-log", &json!({})).is_none());
    }

    #[test]
    fn unknown_event_ignored() {
        assert!(PiAdapter.parse("something-else", &json!({})).is_none());
    }

    #[test]
    fn resolve_adapter_routes_pi() {
        let adapter = resolve_adapter("pi").expect("pi adapter should resolve");
        match adapter.parse("user-prompt-submit", &json!({"prompt": "hi"})) {
            Some(AgentEvent::UserPromptSubmit { agent, .. }) => assert_eq!(agent, "pi"),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
    }
}
