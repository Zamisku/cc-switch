//! DeepSeek Harness 的 MCP 同步。
//!
//! DSH 没有单独的 `mcp.json`：**一台服务器一条 plugin 行**，写进 profile 的
//! `cordis.patch.yml`，行名固定 `@deepseek-ai/dsh-mcp-client`，配置里用
//! `serverName` 做工具命名空间。DSH 只支持 stdio 与 Streamable HTTP，不支持 SSE。
//!
//! 行 id 用 `mcp-ccs-<服务器 id>`；行里含 `!!js` 表达式时只允许禁用/删除，不改写配置。

use crate::app_config::{McpApps, McpServer};
use crate::dsh_config::{self, DshPaths};
use crate::error::AppError;
use crate::mcp::validation::{spec_transport_type, validate_server_spec};
use serde_json::{json, Value};
use std::sync::Mutex;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn paths() -> DshPaths {
    DshPaths::resolve()
}

/// CC Switch 统一规范 → DSH 原生配置（去掉 `type`，换成 `transport`）。
pub(crate) fn native_config(spec: &Value) -> Result<Value, AppError> {
    let transport = spec_transport_type(spec);
    // 只有 url、没写 type 的规范按 http 处理；补上 type 再交给通用校验，
    // 否则校验器会按 stdio 要求 command。
    let mut normalized = spec.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.insert(
            "type".into(),
            Value::String(match transport {
                "stdio" => "stdio".into(),
                "sse" => "sse".into(),
                _ => "http".into(),
            }),
        );
    }
    validate_server_spec(&normalized)?;
    match transport {
        "stdio" => {
            let mut native = json!({
                "transport": "stdio",
                "command": spec.get("command").cloned().unwrap_or(Value::Null),
            });
            for field in ["args", "env", "cwd"] {
                if let Some(value) = spec.get(field) {
                    let empty = match value {
                        Value::Array(items) => items.is_empty(),
                        Value::Object(map) => map.is_empty(),
                        Value::String(text) => text.trim().is_empty(),
                        Value::Null => true,
                        _ => false,
                    };
                    if !empty {
                        native[field] = value.clone();
                    }
                }
            }
            Ok(native)
        }
        "http" | "streamable-http" | "streamable_http" => {
            let url = spec
                .get("url")
                .or_else(|| spec.get("serverUrl"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if url.is_empty() {
                return Err(AppError::McpValidation(
                    "DSH 的 Streamable HTTP MCP 服务器缺少 url 字段".into(),
                ));
            }
            let mut native = json!({ "transport": "streamable-http", "url": url });
            if let Some(headers) = spec.get("headers") {
                if headers.as_object().is_some_and(|map| !map.is_empty()) {
                    native["headers"] = headers.clone();
                }
            }
            Ok(native)
        }
        "sse" => Err(AppError::McpValidation(
            "DSH 不支持 SSE 连接方式的 MCP 服务器，请改用 HTTP（Streamable HTTP）".into(),
        )),
        other => Err(AppError::McpValidation(format!(
            "DSH 不支持 MCP 传输类型 '{other}'"
        ))),
    }
}

/// DSH 原生配置 → CC Switch 统一规范（导入用）。
pub(crate) fn unified_spec(config: &Value) -> Result<Value, AppError> {
    let transport = config
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("stdio");
    match transport {
        "stdio" => {
            let command = config
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if command.is_empty() {
                return Err(AppError::McpValidation("DSH MCP 行缺少 command".into()));
            }
            let mut spec = json!({ "type": "stdio", "command": command });
            for field in ["args", "env", "cwd"] {
                if let Some(value) = config.get(field) {
                    spec[field] = value.clone();
                }
            }
            Ok(spec)
        }
        "streamable-http" => {
            let url = config
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if url.is_empty() {
                return Err(AppError::McpValidation("DSH MCP 行缺少 url".into()));
            }
            let mut spec = json!({ "type": "http", "url": url });
            if let Some(headers) = config.get("headers") {
                if headers.as_object().is_some_and(|map| !map.is_empty()) {
                    spec["headers"] = headers.clone();
                }
            }
            Ok(spec)
        }
        other => Err(AppError::McpValidation(format!(
            "DSH MCP 行的 transport '{other}' 无法导入"
        ))),
    }
}

/// 启用（或更新）一台服务器：写入对应行并去掉 `disabled`。
pub fn sync(id: &str, spec: Option<&Value>) -> Result<(), AppError> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|error| AppError::Message(error.to_string()))?;
    let paths = paths();
    match spec {
        Some(spec) => {
            let native = native_config(spec)?;
            dsh_config::upsert_mcp_server(&paths, id, id, &native)?;
        }
        None => dsh_config::disable_mcp_server(&paths, id, id)?,
    }
    Ok(())
}

/// 删除服务器对应的行。
pub fn remove(id: &str) -> Result<(), AppError> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|error| AppError::Message(error.to_string()))?;
    dsh_config::remove_mcp_server(&paths(), id, id)
}

/// 把 profile 里现有的 MCP 行导入数据库。
///
/// 服务器 id 取 `serverName`（cc-switch 与 DSH 都靠它认同一台服务器）；含 `!!js`
/// 表达式的行跳过——表达式没法投影成 JSON，硬导入会丢内容。
pub fn import_from_dsh(config: &mut crate::app_config::MultiAppConfig) -> Result<usize, AppError> {
    let entries = dsh_config::read_mcp_servers(&paths())?;
    if entries.is_empty() {
        return Ok(0);
    }
    let servers = config.mcp.servers.get_or_insert_with(Default::default);
    let mut changed = 0usize;

    for entry in entries {
        if entry.has_js {
            log::warn!(
                "Skip DSH MCP row '{}': it uses !!js expressions that cannot be imported",
                entry.row_id
            );
            continue;
        }
        let Some(server_name) = entry.server_name.clone() else {
            log::warn!("Skip DSH MCP row '{}': no serverName", entry.row_id);
            continue;
        };
        let unified = match unified_spec(&entry.config) {
            Ok(spec) => spec,
            Err(error) => {
                log::warn!("Skip DSH MCP row '{}': {error}", entry.row_id);
                continue;
            }
        };
        if let Err(error) = validate_server_spec(&unified) {
            log::warn!("Skip DSH MCP row '{}': {error}", entry.row_id);
            continue;
        }

        if let Some(existing) = servers.get_mut(&server_name) {
            if entry.disabled {
                continue;
            }
            if !existing.apps.dsh {
                existing.apps.dsh = true;
                changed += 1;
            }
            continue;
        }

        servers.insert(
            server_name.clone(),
            McpServer {
                id: server_name.clone(),
                name: server_name.clone(),
                server: unified,
                apps: McpApps {
                    claude: false,
                    codex: false,
                    gemini: false,
                    grokbuild: false,
                    opencode: false,
                    hermes: false,
                    mcode: false,
                    pi: false,
                    agy: false,
                    dsh: !entry.disabled,
                },
                description: None,
                homepage: None,
                docs: None,
                tags: Vec::new(),
            },
        );
        changed += 1;
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_spec_becomes_a_dsh_stdio_config() {
        let native = native_config(&json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": {"GITHUB_TOKEN": "x"},
        }))
        .expect("stdio config");
        assert_eq!(native["transport"], "stdio");
        assert_eq!(native["command"], "npx");
        assert_eq!(native["args"][0], "-y");
        assert_eq!(native["env"]["GITHUB_TOKEN"], "x");
    }

    #[test]
    fn http_spec_becomes_streamable_http_and_sse_is_rejected() {
        let native = native_config(&json!({
            "type": "http",
            "url": "https://example.test/mcp",
            "headers": {"Authorization": "Bearer x"},
        }))
        .expect("http config");
        assert_eq!(native["transport"], "streamable-http");
        assert_eq!(native["url"], "https://example.test/mcp");
        assert_eq!(native["headers"]["Authorization"], "Bearer x");

        let error = native_config(&json!({"type": "sse", "url": "https://example.test/sse"}))
            .expect_err("sse must be rejected");
        assert!(error.to_string().contains("SSE"), "{error}");
    }

    #[test]
    fn url_only_spec_is_treated_as_http() {
        let native =
            native_config(&json!({"url": "https://example.test/mcp"})).expect("url-only config");
        assert_eq!(native["transport"], "streamable-http");
    }

    #[test]
    fn unified_spec_roundtrips_both_transports() {
        let stdio = unified_spec(&json!({
            "transport": "stdio",
            "command": "uvx",
            "args": ["mcp-server-fetch"],
            "env": {},
        }))
        .expect("stdio spec");
        assert_eq!(stdio["type"], "stdio");
        assert_eq!(stdio["command"], "uvx");

        let http = unified_spec(&json!({
            "transport": "streamable-http",
            "url": "https://example.test/mcp",
            "headers": {"X-Test": "1"},
        }))
        .expect("http spec");
        assert_eq!(http["type"], "http");
        assert_eq!(http["headers"]["X-Test"], "1");
    }
}
