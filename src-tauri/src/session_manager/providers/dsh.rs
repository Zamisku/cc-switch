//! DeepSeek Harness 会话：列表来自投影缓存，正文来自压缩事件日志。
//!
//! DSH 把每个会话写成 `$DSH_HOME/sessions/<编码工作区>/session-<id>/session.v4.jsonl.zstd`
//! （Zstandard 压缩的 JSONL v4 事件流），同时把标题、用量、轮次摘要投影到
//! `$DSH_HOME/storages/session_projcache/sessions/session-<id>.json`。
//!
//! - 列表：只读投影缓存（快，且带标题与 token 统计）；
//! - 正文：流式解压事件日志，把 `user/message`、`assistant/message`、`tool/result`
//!   还原成阅读页的 blocks；
//! - 删除：交给 DSH 自己（日志、投影缓存与会话锁要一起维护）。

use super::blocks::{assign_turn_ids, thinking_block, tool_call_block, ToolSource};
use crate::session_manager::model::{SessionBlock, SessionMessage, ToolStatus};
use crate::session_manager::SessionMeta;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

/// DSH 的压缩事件日志根目录。
pub fn session_roots() -> Vec<PathBuf> {
    vec![crate::dsh_config::DshPaths::resolve()
        .root()
        .join("sessions")]
}

fn projection_cache_dir() -> PathBuf {
    crate::dsh_config::DshPaths::resolve()
        .root()
        .join("storages")
        .join("session_projcache")
        .join("sessions")
}

fn rows<'a>(record: &'a Value, key: &str) -> Option<&'a Value> {
    record
        .get("rows")?
        .get(key)?
        .get("val")
        .filter(|value| !value.is_null())
}

fn text_at(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn timestamp(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|value| *value > 0)
}

/// 在日志根目录里找某个会话的压缩日志，优先最高版本。
fn find_session_log(logs_root: &Path, session_id: &str) -> Option<PathBuf> {
    let workspaces = fs::read_dir(logs_root).ok()?;
    for workspace in workspaces.flatten() {
        let dir = workspace.path().join(session_id);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut candidates: Vec<(u32, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let version = name
                    .strip_prefix("session.v")?
                    .strip_suffix(".jsonl.zstd")?
                    .parse::<u32>()
                    .ok()?;
                Some((version, entry.path()))
            })
            .collect();
        candidates.sort_by_key(|(version, _)| *version);
        if let Some((_, path)) = candidates.pop() {
            return Some(path);
        }
    }
    None
}

fn scan_cache_dir(cache_dir: &Path, logs_root: &Path) -> Vec<SessionMeta> {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(session_id) = file_name
            .strip_prefix("session-")
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let session_id = format!("session-{session_id}");
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(document) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(record) = document.get("record") else {
            continue;
        };
        if rows(record, "sessionListMetadata")
            .and_then(|value| value.get("blank"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            continue;
        }
        let Some(log_path) = find_session_log(logs_root, &session_id) else {
            continue;
        };

        let identity = record.get("identity");
        let title = text_at(rows(record, "title")).or_else(|| {
            text_at(
                rows(record, "titleInput")
                    .and_then(|value| value.get("first"))
                    .and_then(|value| value.get("text")),
            )
        });
        let first_prompt = text_at(
            rows(record, "titleInput")
                .and_then(|value| value.get("first"))
                .and_then(|value| value.get("text")),
        );
        let created_at = timestamp(identity.and_then(|value| value.get("createdAt")));
        let last_active_at = timestamp(
            rows(record, "sessionListMetadata").and_then(|value| value.get("lastPromptAt")),
        )
        .or_else(|| {
            timestamp(rows(record, "timeContext").and_then(|value| value.get("lastMessageTime")))
        })
        .or(created_at);

        sessions.push(SessionMeta {
            provider_id: "dsh".to_string(),
            session_id,
            title: title.map(|title| super::utils::truncate_summary(&title, 120)),
            summary: first_prompt.map(|text| super::utils::truncate_summary(&text, 200)),
            project_dir: text_at(identity.and_then(|value| value.get("cwd")))
                .or_else(|| text_at(rows(record, "workingDirectory"))),
            created_at,
            last_active_at,
            source_path: Some(log_path.to_string_lossy().into_owned()),
            resume_command: None,
        });
    }
    sessions
}

pub fn scan_sessions() -> Vec<SessionMeta> {
    scan_cache_dir(&projection_cache_dir(), &session_roots()[0])
}

fn blocks_from_content(items: &[Value]) -> Vec<SessionBlock> {
    let mut blocks = Vec::new();
    for item in items {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        blocks.push(SessionBlock::text(text));
                    }
                }
            }
            "reasoning" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    blocks.push(thinking_block(text, None, None, || None));
                }
            }
            "tool-call" => {
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                let input = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                blocks.push(tool_call_block(
                    ToolSource::Generic,
                    id,
                    name,
                    &input,
                    || None,
                ));
            }
            _ => {}
        }
    }
    blocks
}

fn message_blocks(value: &Value) -> Vec<SessionBlock> {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|items| blocks_from_content(items))
        .unwrap_or_default()
}

fn push_message(
    messages: &mut Vec<SessionMessage>,
    role: &str,
    blocks: Vec<SessionBlock>,
    ts: Option<i64>,
    id: Option<String>,
    turn: Option<String>,
) {
    if blocks.is_empty() {
        return;
    }
    let mut message = SessionMessage::from_blocks(role, ts, blocks);
    message.id = id;
    message.turn_id = turn;
    messages.push(message);
}

/// 把一条 JSONL 事件折成阅读页消息。
fn apply_event(messages: &mut Vec<SessionMessage>, event: &Value, current_turn: &mut u32) {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let time = timestamp(event.get("time"));
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    match event_type {
        "turn/start" => {
            if let Some(turn) = data.get("turn").and_then(Value::as_u64) {
                *current_turn = turn as u32;
            }
        }
        "user/message" => {
            let blocks = message_blocks(&data);
            let id = data.get("id").and_then(Value::as_str).map(str::to_string);
            push_message(
                messages,
                "user",
                blocks,
                time,
                id,
                Some(format!("t{current_turn}")),
            );
        }
        "assistant/message" => {
            let Some(message) = data.get("message") else {
                return;
            };
            let turn = data
                .get("turn")
                .and_then(Value::as_u64)
                .map(|turn| turn as u32)
                .unwrap_or(*current_turn);
            let blocks = message_blocks(message);
            push_message(
                messages,
                "assistant",
                blocks,
                time,
                None,
                Some(format!("t{turn}")),
            );
        }
        "tool/result" => {
            let Some(message) = data.get("message") else {
                return;
            };
            let call_id = message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let text = message
                .get("content")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.trim().is_empty() && call_id.is_empty() {
                return;
            }
            let is_error = message
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if is_error {
                ToolStatus::Error
            } else {
                ToolStatus::Success
            };
            let turn = data
                .get("turn")
                .and_then(Value::as_u64)
                .map(|turn| format!("t{turn}"));
            push_message(
                messages,
                "tool",
                vec![super::blocks::tool_result_block(
                    call_id,
                    status,
                    &text,
                    || None,
                )],
                time,
                None,
                turn,
            );
        }
        _ => {}
    }
}

pub fn load_messages(path: &Path) -> Result<Vec<SessionMessage>, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("Cannot read DeepSeek Harness session log: {error}"))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|error| format!("Cannot decompress DeepSeek Harness session log: {error}"))?;
    let mut messages = Vec::new();
    let mut current_turn = 0u32;
    for line in BufReader::new(decoder).lines() {
        let line =
            line.map_err(|error| format!("Cannot read DeepSeek Harness session log: {error}"))?;
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        apply_event(&mut messages, &event, &mut current_turn);
    }
    assign_turn_ids(&mut messages);
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_log(dir: &Path, events: &[Value]) -> PathBuf {
        let path = dir.join("session.v4.jsonl.zstd");
        let text = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = zstd::encode_all(text.as_bytes(), 0).expect("compress");
        fs::write(&path, compressed).expect("write log");
        path
    }

    #[test]
    fn reads_messages_from_a_compressed_event_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_log(
            dir.path(),
            &[
                json!({"type":"session","version":4,"id":"session-x","createdAt":1,"cwd":"/tmp","isSeeded":false,"delegationDepth":0}),
                json!({"type":"turn/start","seq":0,"time":1000,"data":{"turn":1}}),
                json!({"type":"user/message","seq":1,"time":1001,"data":{"id":"u1","content":[{"type":"text","text":"hi"}]}}),
                json!({"type":"assistant/message","seq":2,"time":1002,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[
                    {"type":"reasoning","text":"think"},
                    {"type":"text","text":"hello"},
                    {"type":"tool-call","id":"c1","name":"bash","arguments":"{\"command\":\"ls\"}"}
                ]}}}),
                json!({"type":"tool/result","seq":3,"time":1003,"data":{"turn":1,"step":1,"message":{"role":"tool","toolCallId":"c1","content":[{"type":"text","text":"out"}]}}}),
            ],
        );

        let messages = load_messages(&path).expect("load");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hi");
        assert_eq!(messages[0].turn_id.as_deref(), Some("t1"));

        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1]
            .blocks
            .iter()
            .any(|block| matches!(block, SessionBlock::Thinking { .. })));
        assert!(messages[1]
            .blocks
            .iter()
            .any(|block| matches!(block, SessionBlock::Text { text, .. } if text == "hello")));
        assert!(messages[1]
            .blocks
            .iter()
            .any(|block| matches!(block, SessionBlock::ToolCall { id, .. } if id == "c1")));

        assert_eq!(messages[2].role, "tool");
        assert!(messages[2].blocks.iter().any(
            |block| matches!(block, SessionBlock::ToolResult { call_id, .. } if call_id == "c1")
        ));
    }

    #[test]
    fn scans_the_projection_cache_and_skips_blank_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_dir = dir.path().join("cache");
        let logs_dir = dir.path().join("logs");
        fs::create_dir_all(&cache_dir).unwrap();
        let workspace = logs_dir.join("--workspace-demo--");
        fs::create_dir_all(workspace.join("session-live")).unwrap();
        fs::write(
            workspace.join("session-live").join("session.v4.jsonl.zstd"),
            b"",
        )
        .unwrap();

        fs::write(
            cache_dir.join("session-live.json"),
            json!({
                "version": 7,
                "record": {
                    "identity": {"formatVersion": 4, "createdAt": 1700000000000u64, "cwd": "/workspace/demo"},
                    "rows": {
                        "title": {"val": "Demo session"},
                        "titleInput": {"val": {"first": {"text": "hello there"}}},
                        "sessionListMetadata": {"val": {"blank": false, "lastPromptAt": 1700000009999u64}},
                        "workingDirectory": {"val": "/workspace/demo"}
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            cache_dir.join("session-blank.json"),
            json!({
                "version": 7,
                "record": {
                    "identity": {"formatVersion": 4, "createdAt": 1700000000000u64, "cwd": "/workspace/demo"},
                    "rows": {"sessionListMetadata": {"val": {"blank": true}}}
                }
            })
            .to_string(),
        )
        .unwrap();

        let sessions = scan_cache_dir(&cache_dir, &logs_dir);
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.provider_id, "dsh");
        assert_eq!(session.session_id, "session-live");
        assert_eq!(session.title.as_deref(), Some("Demo session"));
        assert_eq!(session.project_dir.as_deref(), Some("/workspace/demo"));
        assert_eq!(session.created_at, Some(1700000000000));
        assert_eq!(session.last_active_at, Some(1700000009999));
        assert!(session
            .source_path
            .as_deref()
            .is_some_and(|path| path.ends_with("session.v4.jsonl.zstd")));
    }
}
