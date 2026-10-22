//! DeepSeek Harness（DSH）接入：profile patch 行、凭据与默认模型指针。
//!
//! DSH 没有「一个 JSON 配置文件」，它的配置是 Cordis 插件树的行（entry）：每一行由
//! `id` 定位，`config` 整行替换。与供应商相关的行有三条：
//!
//! - `llm-pi-ai`：多供应商容器，第三方端点写在 `config.providers.<route>`；
//! - `llm-deepseek`：官方 DeepSeek 端点，路由名固定 `deepseek-official`；
//! - `agent-default-model`：当前默认模型，`{ provider, model, reasoningEffort }`。
//!
//! 凭据落在 `$DSH_HOME/.credentials.yaml` 的 `refs.<REF>`（DSH 自己的设置页也写这里）。
//! 所以一次「切换」= 写路由行 + 写 refs + 改默认模型指针。
//!
//! 写入遵循 DSH 自己的语义：按行 id 定位、整行 `config` 替换、原子写。**其它行的文本原样
//! 保留**——用户的注释、`!!js` 表达式都不解析、不改写；只解析目标行，因此别的行里出现
//! 自定义标签也不会让这里失败。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};

/// 官方 DeepSeek 路由名（`dsh-llm-deepseek-api-key` 里写死的 `PROVIDER`）。
pub const OFFICIAL_ROUTE: &str = "deepseek-official";
/// 多供应商容器的行 id。
const PI_AI_ROW: &str = "llm-pi-ai";
/// 官方 Messages 适配器的行 id。
const DEEPSEEK_ROW: &str = "llm-deepseek";
/// 默认模型指针的行 id。
const DEFAULT_MODEL_ROW: &str = "agent-default-model";

const PI_AI_PLUGIN: &str = "@deepseek-ai/dsh-llm-pi-ai";
const DEEPSEEK_PLUGIN: &str = "@deepseek-ai/dsh-llm-deepseek-api-key";
const DEFAULT_MODEL_PLUGIN: &str = "@deepseek-ai/dsh-agent-default-model";

/// DSH 桌面端使用的 profile 目录名。桌面 profile 由 Electron 独占，CC Switch 只改文件。
const DEFAULT_PROFILE: &str = "desktop";
/// 官方端点的默认根（与 `dsh-llm-deepseek` 的 `PUBLIC_BASE_URL` 一致）。
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/anthropic";
/// 官方行的默认凭据引用（与 `dsh-llm-deepseek-api-key` 的 `apiKeyEnv` 默认值一致）。
const DEFAULT_DEEPSEEK_KEY_REF: &str = "DEEPSEEK_API_KEY";
/// `llm-pi-ai` 支持的三种线上协议。
const SUPPORTED_APIS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];
/// 默认指针行允许的思考强度。DSH 对模型级 `reasoningEfforts` 更宽松，这里只管默认模型。
const REASONING_EFFORTS: [&str; 4] = ["off", "low", "high", "max"];

/// 串行化本进程对 patch / credentials 的写入，避免两次点击交错。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ============================================================================
// 路径
// ============================================================================

/// DSH 数据目录与 profile。
///
/// 解析顺序与 DSH 自己一致：CC Switch 的设备级覆盖 → `DSH_HOME` → `~/.dsh`。
#[derive(Debug, Clone)]
pub struct DshPaths {
    root: PathBuf,
    profile: String,
}

impl DshPaths {
    pub fn resolve() -> Self {
        let root = crate::settings::get_dsh_override_dir()
            .or_else(dsh_home_from_env)
            .unwrap_or_else(|| crate::config::get_home_dir().join(".dsh"));
        Self {
            root,
            profile: DEFAULT_PROFILE.to_string(),
        }
    }

    /// 固定根目录。只在测试里用：生产代码必须走 [`DshPaths::resolve`]，
    /// 否则会绕过 `DSH_HOME` 与设备级覆盖。
    #[cfg(test)]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            profile: DEFAULT_PROFILE.to_string(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn profile_dir(&self) -> PathBuf {
        self.root.join("profiles").join(&self.profile)
    }

    pub fn patch_path(&self) -> PathBuf {
        self.profile_dir().join("cordis.patch.yml")
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.root.join(".credentials.yaml")
    }

    /// 用户全局提示词（`$DSH_HOME/AGENTS.md`）。
    pub fn agents_path(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }
}

fn dsh_home_from_env() -> Option<PathBuf> {
    std::env::var_os("DSH_HOME").and_then(|raw| {
        let value = raw.to_string_lossy();
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    })
}

// ============================================================================
// 供应商配置（存进 CC Switch 的 `settingsConfig`）
// ============================================================================

/// DSH 路由里的一只模型。除 `id` 外都可省略，DSH 会按路由默认值补齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DshModelConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        rename = "contextWindow",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<u64>,
    #[serde(default, rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
}

/// CC Switch 里 DSH 供应商的 `settingsConfig`。
///
/// `kind == "deepseek-official"` 表示写官方 `llm-deepseek` 行（只能有一条）；
/// 其余按第三方路由写进 `llm-pi-ai`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DshProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(
        default,
        rename = "displayName",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(
        default,
        rename = "baseURL",
        alias = "baseUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_url: Option<String>,
    #[serde(default, rename = "apiKey", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, rename = "apiKeyEnv", skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub models: Vec<DshModelConfig>,
    #[serde(
        default,
        rename = "reasoningEffort",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteKind {
    Official,
    Custom,
}

/// 校验、规范化后的路由。写文件只认这个结构。
#[derive(Debug, Clone)]
pub struct DshRoute {
    kind: RouteKind,
    route: String,
    display_name: String,
    api: String,
    base_url: String,
    api_key: String,
    api_key_env: String,
    models: Vec<DshModelConfig>,
    reasoning_effort: Option<String>,
}

impl DshRoute {
    fn is_official(&self) -> bool {
        self.kind == RouteKind::Official
    }
}

/// 一次写入的结果，供上层拼 `SwitchResult`。
#[derive(Debug, Clone, Default)]
pub struct DshWriteOutcome {
    pub route: String,
    pub model: String,
    pub warnings: Vec<String>,
}

/// 从本地文件导入出来的路由。
#[derive(Debug, Clone)]
pub struct DshLiveProvider {
    pub provider: Provider,
    pub is_current: bool,
}

fn invalid(zh: impl Into<String>, en: impl Into<String>) -> AppError {
    AppError::localized("dsh.provider.invalid", zh, en)
}

fn is_route_segment_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// DSH 的路由规则（与设置页 `ROUTE_PATTERN` 相同）：小写字母开头，单词用单个 `-` 连接。
fn is_valid_route(route: &str) -> bool {
    if route.is_empty() || !route.starts_with(|c: char| c.is_ascii_lowercase()) {
        return false;
    }
    let mut previous_dash = false;
    for c in route.chars() {
        if c == '-' {
            if previous_dash {
                return false;
            }
            previous_dash = true;
            continue;
        }
        if !is_route_segment_char(c) {
            return false;
        }
        previous_dash = false;
    }
    !previous_dash
}

/// 把任意 provider id / 名称压成合法的 route（`^[a-z][a-z0-9]*(-[a-z0-9]+)*$`）。
pub fn normalize_route(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in raw.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out.insert_str(0, "dsh-");
    }
    out
}

/// 凭据引用名，与 DSH 设置页的 `deriveKeyRef` 一致：`<ROUTE 大写，非字母数字换 _>_API_KEY`。
pub fn derive_key_ref(route: &str) -> String {
    let mut out = String::with_capacity(route.len() + 8);
    for c in route.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out.push_str("_API_KEY");
    out
}

fn is_valid_key_ref(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn normalize_base_url(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim().trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(AppError::localized(
            "dsh.provider.base_url_invalid",
            format!("DSH 端点必须以 http:// 或 https:// 开头：{trimmed}"),
            format!("DSH endpoint must start with http:// or https://: {trimmed}"),
        ));
    }
    if trimmed.contains('?') || trimmed.contains('#') || trimmed.contains('@') {
        return Err(AppError::localized(
            "dsh.provider.base_url_invalid",
            format!("DSH 端点只能是根地址，不能带查询串、片段或内嵌凭据：{trimmed}"),
            format!(
                "DSH endpoint must be a root URL without query, fragment, or embedded credentials: {trimmed}"
            ),
        ));
    }
    Ok(trimmed.to_string())
}

fn normalize_models(models: &[DshModelConfig]) -> Result<Vec<DshModelConfig>, AppError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let id = model.id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let mut normalized = model.clone();
        normalized.id = id.to_string();
        normalized.name = normalized
            .name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        normalized.input = normalized.input.filter(|input| !input.is_empty());
        out.push(normalized);
    }
    if out.is_empty() {
        return Err(AppError::localized(
            "dsh.provider.models_empty",
            "DSH 供应商至少需要一个模型（模型 id 不能为空）",
            "A DSH provider needs at least one model with a non-empty id",
        ));
    }
    Ok(out)
}

fn route_identity(provider: &Provider) -> (RouteKind, String) {
    let config: DshProviderConfig =
        serde_json::from_value(provider.settings_config.clone()).unwrap_or_default();
    let official = config
        .kind
        .as_deref()
        .map(|kind| kind.trim().eq_ignore_ascii_case("official") || kind.trim() == OFFICIAL_ROUTE)
        .unwrap_or(false);
    if official {
        return (RouteKind::Official, OFFICIAL_ROUTE.to_string());
    }
    let route = config
        .route
        .as_deref()
        .map(str::trim)
        .filter(|route| !route.is_empty())
        .map(normalize_route)
        .unwrap_or_else(|| normalize_route(&provider.id));
    (RouteKind::Custom, route)
}

/// 从 CC Switch 的供应商行解析出可直接下盘的 DSH 路由，并做全部校验。
pub fn route_from_provider(provider: &Provider) -> Result<DshRoute, AppError> {
    let (kind, route) = route_identity(provider);
    let config: DshProviderConfig = serde_json::from_value(provider.settings_config.clone())
        .map_err(|error| {
            invalid(
                format!(
                    "供应商「{}」的 DSH 配置不是合法对象：{error}",
                    provider.name
                ),
                format!(
                    "DSH provider '{}' has no valid configuration object: {error}",
                    provider.name
                ),
            )
        })?;

    if !is_valid_route(&route) {
        return Err(AppError::localized(
            "dsh.provider.route_invalid",
            format!(
                "DSH 路由名「{route}」不合法：需要以小写字母开头，只含小写字母、数字和单个连字符"
            ),
            format!(
                "DSH route '{route}' is invalid: it must start with a lowercase letter and contain only lowercase letters, digits, and single hyphens"
            ),
        ));
    }

    let base_url = normalize_base_url(
        config
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                AppError::localized(
                    "dsh.provider.base_url_missing",
                    format!("供应商「{}」没有填写 DSH 端点地址", provider.name),
                    format!("DSH provider '{}' has no endpoint address", provider.name),
                )
            })?,
    )?;

    let api = match kind {
        RouteKind::Official => "anthropic-messages".to_string(),
        RouteKind::Custom => {
            let api = config
                .api
                .as_deref()
                .map(str::trim)
                .filter(|api| !api.is_empty())
                .unwrap_or("anthropic-messages");
            if !SUPPORTED_APIS.contains(&api) {
                return Err(AppError::localized(
                    "dsh.provider.protocol_invalid",
                    format!(
                        "DSH 不支持协议「{api}」，可选：{}",
                        SUPPORTED_APIS.join(" / ")
                    ),
                    format!(
                        "DSH does not support protocol '{api}'; choose one of {}",
                        SUPPORTED_APIS.join(" / ")
                    ),
                ));
            }
            api.to_string()
        }
    };

    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    let api_key_env = config
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match kind {
            RouteKind::Official => DEFAULT_DEEPSEEK_KEY_REF.to_string(),
            RouteKind::Custom => derive_key_ref(&route),
        });
    if !is_valid_key_ref(&api_key_env) {
        return Err(AppError::localized(
            "dsh.provider.api_key_env_invalid",
            format!("凭据引用「{api_key_env}」不合法：只允许字母、数字和下划线，且不能以数字开头"),
            format!(
                "Credential reference '{api_key_env}' is invalid: use letters, digits, and underscores, and do not start with a digit"
            ),
        ));
    }

    let models = normalize_models(&config.models)?;

    let reasoning_effort = config
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
        .map(str::to_string);
    if let Some(effort) = reasoning_effort.as_deref() {
        if !REASONING_EFFORTS.contains(&effort) {
            return Err(AppError::localized(
                "dsh.provider.reasoning_effort_invalid",
                format!(
                    "思考强度「{effort}」不合法，可选：{}",
                    REASONING_EFFORTS.join(" / ")
                ),
                format!(
                    "Reasoning effort '{effort}' is invalid; choose one of {}",
                    REASONING_EFFORTS.join(" / ")
                ),
            ));
        }
    }

    let display_name = config
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if kind == RouteKind::Official {
                "DeepSeek".to_string()
            } else {
                provider.name.clone()
            }
        });

    Ok(DshRoute {
        kind,
        route,
        display_name,
        api,
        base_url,
        api_key,
        api_key_env,
        models,
        reasoning_effort,
    })
}

// ============================================================================
// JSON ↔ YAML
// ============================================================================

fn json_to_yaml(value: &Value) -> Result<serde_yaml::Value, AppError> {
    serde_yaml::to_value(value).map_err(|error| {
        invalid(
            format!("DSH 配置无法转成 YAML：{error}"),
            format!("DSH configuration cannot be represented as YAML: {error}"),
        )
    })
}

fn models_to_yaml(models: &[DshModelConfig]) -> Result<serde_yaml::Value, AppError> {
    let json = serde_json::to_value(models)
        .map_err(|error| invalid(error.to_string(), error.to_string()))?;
    json_to_yaml(&json)
}

fn route_profile_yaml(route: &DshRoute) -> Result<serde_yaml::Value, AppError> {
    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("displayName".into()),
        serde_yaml::Value::String(route.display_name.clone()),
    );
    profile.insert(
        serde_yaml::Value::String("api".into()),
        serde_yaml::Value::String(route.api.clone()),
    );
    profile.insert(
        serde_yaml::Value::String("baseURL".into()),
        serde_yaml::Value::String(route.base_url.clone()),
    );
    if !route.api_key.is_empty() || route.is_official() {
        profile.insert(
            serde_yaml::Value::String("apiKeyEnv".into()),
            serde_yaml::Value::String(route.api_key_env.clone()),
        );
    }
    profile.insert(
        serde_yaml::Value::String("models".into()),
        models_to_yaml(&route.models)?,
    );
    Ok(serde_yaml::Value::Mapping(profile))
}

// ============================================================================
// patch 文本：行定位与整体替换
// ============================================================================

/// 顶层数组里一行的字节区间 `[start, end)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowSpan {
    start: usize,
    end: usize,
}

fn scan_rows(text: &str) -> Vec<RowSpan> {
    let mut rows = Vec::new();
    let mut offset = 0usize;
    let mut current: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let is_row = body == "-" || body.starts_with("- ");
        if is_row {
            if let Some(start) = current.take() {
                rows.push(RowSpan { start, end: offset });
            }
            current = Some(offset);
        }
        offset += line.len();
    }
    if let Some(start) = current {
        if start < text.len() {
            rows.push(RowSpan {
                start,
                end: text.len(),
            });
        }
    }
    rows
}

/// 一行的 `id`。只看行首与缩进 2 的键，避免命中 `models[].id` 这类嵌套键。
fn row_id(block: &str) -> Option<String> {
    for (index, line) in block.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if index > 0 && indent != 2 {
            continue;
        }
        let body = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let Some(rest) = body.strip_prefix("id:") else {
            continue;
        };
        let value = rest.trim().trim_matches(|c| c == '\'' || c == '"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn find_row(text: &str, id: &str) -> Option<RowSpan> {
    scan_rows(text)
        .into_iter()
        .find(|span| row_id(&text[span.start..span.end]).as_deref() == Some(id))
}

fn row_block<'a>(row: &RowSpan, text: &'a str) -> &'a str {
    &text[row.start..row.end]
}

fn parse_row_mapping(block: &str, id: &str) -> Result<serde_yaml::Mapping, AppError> {
    let value: serde_yaml::Value = serde_yaml::from_str(block).map_err(|error| {
        AppError::localized(
            "dsh.patch.unreadable",
            format!(
                "DSH 配置里的 `{id}` 行无法解析（{error}）。请在 DeepSeek Harness 设置页确认这一行，或手动修正 `cordis.patch.yml`。"
            ),
            format!(
                "The `{id}` row in the DSH profile cannot be parsed ({error}). Check it in the DeepSeek Harness settings page or fix `cordis.patch.yml` manually."
            ),
        )
    })?;
    let item = match value {
        serde_yaml::Value::Sequence(mut rows) if rows.len() == 1 => rows.remove(0),
        other => other,
    };
    item.as_mapping().cloned().ok_or_else(|| {
        AppError::localized(
            "dsh.patch.unreadable",
            format!("DSH 配置里的 `{id}` 行不是映射"),
            format!("The `{id}` row in the DSH profile is not a mapping"),
        )
    })
}

fn row_config(text: &str, id: &str) -> Result<Option<serde_yaml::Mapping>, AppError> {
    let Some(span) = find_row(text, id) else {
        return Ok(None);
    };
    let mapping = parse_row_mapping(row_block(&span, text), id)?;
    Ok(mapping
        .get(serde_yaml::Value::String("config".into()))
        .and_then(serde_yaml::Value::as_mapping)
        .cloned())
}

fn serialize_row(id: &str, plugin: &str, config: serde_yaml::Mapping) -> Result<String, AppError> {
    let mut out = String::new();
    out.push_str(&format!("- id: {id}\n"));
    out.push_str(&format!("  name: '{plugin}'\n"));
    if config.is_empty() {
        out.push_str("  config: {}\n");
        return Ok(out);
    }
    out.push_str("  config:\n");
    let body = serde_yaml::to_string(&serde_yaml::Value::Mapping(config)).map_err(|error| {
        invalid(
            format!("DSH 配置无法序列化：{error}"),
            format!("DSH configuration cannot be serialized: {error}"),
        )
    })?;
    for line in body.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

fn upsert_row(
    text: &str,
    id: &str,
    plugin: &str,
    config: serde_yaml::Mapping,
) -> Result<String, AppError> {
    let block = serialize_row(id, plugin, config)?;
    if let Some(span) = find_row(text, id) {
        let mut out = String::with_capacity(text.len() + block.len());
        out.push_str(&text[..span.start]);
        out.push_str(&block);
        out.push_str(&text[span.end..]);
        return Ok(out);
    }
    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(&block);
    Ok(out)
}

fn remove_row(text: &str, id: &str) -> String {
    let Some(span) = find_row(text, id) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..span.start]);
    out.push_str(&text[span.end..]);
    out
}

fn read_text_or_empty(path: &Path) -> Result<String, AppError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(AppError::io(path, error)),
    }
}

fn write_patch(paths: &DshPaths, text: &str) -> Result<(), AppError> {
    let path = paths.patch_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    crate::config::atomic_write_preserving_mode(&path, text.as_bytes())
}

// ============================================================================
// 凭据
// ============================================================================

fn read_credentials(paths: &DshPaths) -> Result<serde_yaml::Mapping, AppError> {
    let path = paths.credentials_path();
    let text = read_text_or_empty(&path)?;
    if text.trim().is_empty() {
        let mut document = serde_yaml::Mapping::new();
        document.insert(
            serde_yaml::Value::String("version".into()),
            serde_yaml::Value::Number(1u64.into()),
        );
        document.insert(
            serde_yaml::Value::String("refs".into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        return Ok(document);
    }
    let value: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|error| {
        AppError::localized(
            "dsh.credentials.unreadable",
            format!("DSH 凭据文件无法解析：{error}"),
            format!("The DSH credentials file cannot be parsed: {error}"),
        )
    })?;
    value.as_mapping().cloned().ok_or_else(|| {
        AppError::localized(
            "dsh.credentials.unreadable",
            "DSH 凭据文件的顶层不是映射",
            "The DSH credentials file is not a mapping",
        )
    })
}

fn write_credentials(paths: &DshPaths, document: &serde_yaml::Mapping) -> Result<(), AppError> {
    let text =
        serde_yaml::to_string(&serde_yaml::Value::Mapping(document.clone())).map_err(|error| {
            invalid(
                format!("DSH 凭据无法序列化：{error}"),
                format!("DSH credentials cannot be serialized: {error}"),
            )
        })?;
    let path = paths.credentials_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    crate::config::write_text_file_private(&path, &text)
}

fn refs_mapping(document: &mut serde_yaml::Mapping) -> &mut serde_yaml::Mapping {
    let key = serde_yaml::Value::String("refs".into());
    if !matches!(document.get(&key), Some(serde_yaml::Value::Mapping(_))) {
        document.insert(
            key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    document
        .get_mut(&key)
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("refs mapping was just ensured")
}

fn set_credential(document: &mut serde_yaml::Mapping, name: &str, value: &str) {
    refs_mapping(document).insert(
        serde_yaml::Value::String(name.to_string()),
        serde_yaml::Value::String(value.to_string()),
    );
}

fn remove_credential(document: &mut serde_yaml::Mapping, name: &str) {
    refs_mapping(document).remove(serde_yaml::Value::String(name.to_string()));
}

fn read_credential(document: &serde_yaml::Mapping, name: &str) -> Option<String> {
    document
        .get(serde_yaml::Value::String("refs".into()))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|refs| refs.get(serde_yaml::Value::String(name.to_string())))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
}

// ============================================================================
// 写入
// ============================================================================

fn snapshot(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::io(path, error)),
    }
}

fn restore(path: &Path, bytes: Option<&Vec<u8>>) -> Result<(), AppError> {
    match bytes {
        Some(bytes) => crate::config::atomic_write_preserving_mode(path, bytes),
        None => {
            if path.exists() {
                std::fs::remove_file(path).map_err(|error| AppError::io(path, error))?;
            }
            Ok(())
        }
    }
}

fn write_provider(
    paths: &DshPaths,
    provider: &Provider,
    set_default: bool,
) -> Result<DshWriteOutcome, AppError> {
    let route = route_from_provider(provider)?;
    let _guard = write_lock();

    let patch_path = paths.patch_path();
    let credentials_path = paths.credentials_path();
    let patch_snapshot = snapshot(&patch_path)?;
    let credentials_snapshot = snapshot(&credentials_path)?;

    let result = (|| -> Result<DshWriteOutcome, AppError> {
        let mut text = read_text_or_empty(&patch_path)?;
        let mut credentials = read_credentials(paths)?;

        match route.kind {
            RouteKind::Official => {
                text = upsert_row(
                    &text,
                    DEEPSEEK_ROW,
                    DEEPSEEK_PLUGIN,
                    route_profile_yaml(&route)?
                        .as_mapping()
                        .cloned()
                        .unwrap_or_default(),
                )?;
            }
            RouteKind::Custom => {
                let mut row = row_config(&text, PI_AI_ROW)?.unwrap_or_default();
                let providers_key = serde_yaml::Value::String("providers".into());
                let mut providers = row
                    .get(&providers_key)
                    .and_then(serde_yaml::Value::as_mapping)
                    .cloned()
                    .unwrap_or_default();
                providers.insert(
                    serde_yaml::Value::String(route.route.clone()),
                    route_profile_yaml(&route)?,
                );
                row.insert(providers_key, serde_yaml::Value::Mapping(providers));
                text = upsert_row(&text, PI_AI_ROW, PI_AI_PLUGIN, row)?;
            }
        }

        // 默认模型指针：切换时换成新路由的第一个模型；编辑同一条路由时，如果指针还指着
        // 它但模型已被删掉，顺手修到第一个还在的模型，避免 DSH 报模型不存在。
        let pointer = row_config(&text, DEFAULT_MODEL_ROW)?;
        let pointer_provider = pointer
            .as_ref()
            .and_then(|config| config.get(serde_yaml::Value::String("provider".into())))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);
        let pointer_model = pointer
            .as_ref()
            .and_then(|config| config.get(serde_yaml::Value::String("model".into())))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);
        let points_here = pointer_provider.as_deref() == Some(route.route.as_str());
        if set_default || points_here {
            let first_model = route
                .models
                .first()
                .map(|model| model.id.clone())
                .ok_or_else(|| {
                    AppError::localized(
                        "dsh.provider.models_empty",
                        "DSH 供应商至少需要一个模型才能设为默认",
                        "A DSH provider needs at least one model to become the default",
                    )
                })?;
            let keep_model = pointer_model
                .filter(|model| route.models.iter().any(|item| item.id == *model))
                .unwrap_or_else(|| first_model.clone());
            let model = if set_default {
                first_model.clone()
            } else {
                keep_model
            };
            let mut next = serde_yaml::Mapping::new();
            next.insert(
                serde_yaml::Value::String("provider".into()),
                serde_yaml::Value::String(route.route.clone()),
            );
            next.insert(
                serde_yaml::Value::String("model".into()),
                serde_yaml::Value::String(model.clone()),
            );
            if let Some(effort) = route.reasoning_effort.as_deref() {
                next.insert(
                    serde_yaml::Value::String("reasoningEffort".into()),
                    serde_yaml::Value::String(effort.to_string()),
                );
            }
            text = upsert_row(&text, DEFAULT_MODEL_ROW, DEFAULT_MODEL_PLUGIN, next)?;
        }

        // 只写不删：保存时 Key 留空可能只是「这次没改」，而删供应商时已经单独清理过
        // 引用；这里删掉会把用户已存的 key 一起抹掉。
        if !route.api_key.is_empty() {
            set_credential(&mut credentials, &route.api_key_env, &route.api_key);
        }

        write_patch(paths, &text)?;
        write_credentials(paths, &credentials)?;
        Ok(DshWriteOutcome {
            route: route.route.clone(),
            model: route
                .models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default(),
            warnings: Vec::new(),
        })
    })();

    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, patch_snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
        if let Err(restore_error) = restore(&credentials_path, credentials_snapshot.as_ref()) {
            log::error!("Failed to roll back DSH credentials: {restore_error}");
        }
    }
    result
}

/// 写一条路由，不改默认模型指针（编辑供应商时用）。
pub fn set_provider_route(
    paths: &DshPaths,
    provider: &Provider,
) -> Result<DshWriteOutcome, AppError> {
    write_provider(paths, provider, false)
}

/// 写一条路由，并把默认模型切到它的第一个模型（点卡片切换时用）。
pub fn switch_to(paths: &DshPaths, provider: &Provider) -> Result<DshWriteOutcome, AppError> {
    write_provider(paths, provider, true)
}

/// 从 profile 里删掉一条路由；如果它正被默认指针引用，则回退到其它路由。
pub fn remove_provider_route(
    paths: &DshPaths,
    provider: &Provider,
) -> Result<Vec<String>, AppError> {
    let (kind, route) = route_identity(provider);
    let _guard = write_lock();

    let patch_path = paths.patch_path();
    let credentials_path = paths.credentials_path();
    let patch_snapshot = snapshot(&patch_path)?;
    let credentials_snapshot = snapshot(&credentials_path)?;

    let result = (|| -> Result<Vec<String>, AppError> {
        let mut warnings = Vec::new();
        let mut text = read_text_or_empty(&patch_path)?;
        let mut credentials = read_credentials(paths)?;

        // 先记下这条路由用的是哪个凭据引用：用户可能手改过 apiKeyEnv，不能只按 route 推算。
        let mut key_ref = match kind {
            RouteKind::Official => DEFAULT_DEEPSEEK_KEY_REF.to_string(),
            RouteKind::Custom => derive_key_ref(&route),
        };
        if kind == RouteKind::Custom {
            if let Some(row) = row_config(&text, PI_AI_ROW)? {
                if let Some(profile) = row
                    .get(serde_yaml::Value::String("providers".into()))
                    .and_then(serde_yaml::Value::as_mapping)
                    .and_then(|providers| providers.get(serde_yaml::Value::String(route.clone())))
                    .and_then(serde_yaml::Value::as_mapping)
                {
                    if let Some(env) = profile
                        .get(serde_yaml::Value::String("apiKeyEnv".into()))
                        .and_then(serde_yaml::Value::as_str)
                    {
                        key_ref = env.to_string();
                    }
                }
            }
        } else if let Some(row) = row_config(&text, DEEPSEEK_ROW)? {
            if let Some(env) = row
                .get(serde_yaml::Value::String("apiKeyEnv".into()))
                .and_then(serde_yaml::Value::as_str)
            {
                key_ref = env.to_string();
            }
        }

        match kind {
            RouteKind::Official => {
                text = remove_row(&text, DEEPSEEK_ROW);
            }
            RouteKind::Custom => {
                if let Some(mut row) = row_config(&text, PI_AI_ROW)? {
                    let providers_key = serde_yaml::Value::String("providers".into());
                    if let Some(providers) = row
                        .get_mut(&providers_key)
                        .and_then(serde_yaml::Value::as_mapping_mut)
                    {
                        providers.remove(serde_yaml::Value::String(route.clone()));
                    }
                    let providers_empty = row
                        .get(&providers_key)
                        .and_then(serde_yaml::Value::as_mapping)
                        .is_none_or(|providers| providers.is_empty());
                    if providers_empty && row.len() == 1 {
                        // 这一行只剩空的 providers：它就是 CC Switch 建的，删掉整行让
                        // bundle 里的默认行生效，profile 保持干净。
                        text = remove_row(&text, PI_AI_ROW);
                    } else {
                        text = upsert_row(&text, PI_AI_ROW, PI_AI_PLUGIN, row)?;
                    }
                }
            }
        }

        // 默认指针还指着被删的那家：换到剩下第一条路由，换不了就保持原样并告警。
        let pointer_provider = row_config(&text, DEFAULT_MODEL_ROW)?.and_then(|config| {
            config
                .get(serde_yaml::Value::String("provider".into()))
                .and_then(serde_yaml::Value::as_str)
                .map(str::to_string)
        });
        if pointer_provider.as_deref() == Some(route.as_str()) {
            match first_available_route(&text)? {
                Some((next_route, next_model, effort)) => {
                    let mut next = serde_yaml::Mapping::new();
                    next.insert(
                        serde_yaml::Value::String("provider".into()),
                        serde_yaml::Value::String(next_route.clone()),
                    );
                    next.insert(
                        serde_yaml::Value::String("model".into()),
                        serde_yaml::Value::String(next_model),
                    );
                    if let Some(effort) = effort {
                        next.insert(
                            serde_yaml::Value::String("reasoningEffort".into()),
                            serde_yaml::Value::String(effort),
                        );
                    }
                    text = upsert_row(&text, DEFAULT_MODEL_ROW, DEFAULT_MODEL_PLUGIN, next)?;
                    warnings.push(format!("dsh.default_model_reassigned:{next_route}"));
                }
                None => {
                    // 没有任何可指向的路由了：删掉用户覆盖层，让 DSH 回落到它自带的
                    // `agent-default-model` 默认值（官方路由 + 出厂模型），而不是留下
                    // 一个指向已删路由的悬空指针。
                    text = remove_row(&text, DEFAULT_MODEL_ROW);
                    warnings.push("dsh.default_model_reset_to_default".to_string());
                }
            }
        }

        // 没有别处再引用这个凭据引用时才删，避免误删官方行共用的 key。
        if !text.contains(&key_ref) {
            remove_credential(&mut credentials, &key_ref);
        }

        write_patch(paths, &text)?;
        write_credentials(paths, &credentials)?;
        Ok(warnings)
    })();

    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, patch_snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
        if let Err(restore_error) = restore(&credentials_path, credentials_snapshot.as_ref()) {
            log::error!("Failed to roll back DSH credentials: {restore_error}");
        }
    }
    result
}

fn first_available_route(text: &str) -> Result<Option<(String, String, Option<String>)>, AppError> {
    if let Some(row) = row_config(text, PI_AI_ROW)? {
        if let Some(providers) = row
            .get(serde_yaml::Value::String("providers".into()))
            .and_then(serde_yaml::Value::as_mapping)
        {
            for (key, value) in providers {
                let Some(route) = key.as_str() else {
                    continue;
                };
                let Some(profile) = value.as_mapping() else {
                    continue;
                };
                let Some(model) = profile
                    .get(serde_yaml::Value::String("models".into()))
                    .and_then(serde_yaml::Value::as_sequence)
                    .and_then(|models| models.first())
                    .and_then(serde_yaml::Value::as_mapping)
                    .and_then(|model| model.get(serde_yaml::Value::String("id".into())))
                    .and_then(serde_yaml::Value::as_str)
                else {
                    continue;
                };
                return Ok(Some((route.to_string(), model.to_string(), None)));
            }
        }
    }
    if let Some(official) = row_config(text, DEEPSEEK_ROW)? {
        let model = official
            .get(serde_yaml::Value::String("models".into()))
            .and_then(serde_yaml::Value::as_sequence)
            .and_then(|models| models.first())
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|model| model.get(serde_yaml::Value::String("id".into())))
            .and_then(serde_yaml::Value::as_str);
        if let Some(model) = model {
            return Ok(Some((OFFICIAL_ROUTE.to_string(), model.to_string(), None)));
        }
    }
    Ok(None)
}

// ============================================================================
// 导入
// ============================================================================

fn model_from_yaml(value: &serde_yaml::Value) -> Option<DshModelConfig> {
    let map = value.as_mapping()?;
    let id = map
        .get(serde_yaml::Value::String("id".into()))
        .and_then(serde_yaml::Value::as_str)?
        .trim()
        .to_string();
    if id.is_empty() {
        return None;
    }
    let name = map
        .get(serde_yaml::Value::String("name".into()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string);
    let context_window = map
        .get(serde_yaml::Value::String("contextWindow".into()))
        .and_then(serde_yaml::Value::as_u64);
    let max_tokens = map
        .get(serde_yaml::Value::String("maxTokens".into()))
        .and_then(serde_yaml::Value::as_u64);
    let input = map
        .get(serde_yaml::Value::String("input".into()))
        .and_then(serde_yaml::Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|input| !input.is_empty());
    Some(DshModelConfig {
        id,
        name,
        context_window,
        max_tokens,
        input,
    })
}

fn models_from_yaml(value: Option<&serde_yaml::Value>) -> Vec<DshModelConfig> {
    value
        .and_then(serde_yaml::Value::as_sequence)
        .map(|models| models.iter().filter_map(model_from_yaml).collect())
        .unwrap_or_default()
}

/// 读取本机 profile 里的全部路由，并标出默认模型指向的那条。
pub fn read_live_providers(paths: &DshPaths) -> Result<Vec<DshLiveProvider>, AppError> {
    let text = read_text_or_empty(&paths.patch_path())?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let credentials = read_credentials(paths).unwrap_or_default();
    let pointer = row_config(&text, DEFAULT_MODEL_ROW)?.and_then(|config| {
        config
            .get(serde_yaml::Value::String("provider".into()))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string)
    });

    let mut providers = Vec::new();

    if let Some(row) = row_config(&text, PI_AI_ROW)? {
        if let Some(routes) = row
            .get(serde_yaml::Value::String("providers".into()))
            .and_then(serde_yaml::Value::as_mapping)
        {
            for (key, value) in routes {
                let Some(route) = key.as_str() else {
                    continue;
                };
                let Some(profile) = value.as_mapping() else {
                    continue;
                };
                let display_name = profile
                    .get(serde_yaml::Value::String("displayName".into()))
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or(route)
                    .to_string();
                let api = profile
                    .get(serde_yaml::Value::String("api".into()))
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or("anthropic-messages")
                    .to_string();
                let base_url = profile
                    .get(serde_yaml::Value::String("baseURL".into()))
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let api_key_env = profile
                    .get(serde_yaml::Value::String("apiKeyEnv".into()))
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| derive_key_ref(route));
                let models =
                    models_from_yaml(profile.get(serde_yaml::Value::String("models".into())));
                let config = DshProviderConfig {
                    kind: Some("custom".into()),
                    route: Some(route.to_string()),
                    display_name: Some(display_name.clone()),
                    api: Some(api),
                    base_url: Some(base_url),
                    api_key: read_credential(&credentials, &api_key_env),
                    api_key_env: Some(api_key_env),
                    models,
                    reasoning_effort: None,
                };
                let mut provider = Provider::with_id(
                    route.to_string(),
                    display_name,
                    serde_json::to_value(&config).map_err(|error| {
                        invalid(
                            format!("DSH 路由「{route}」无法转换：{error}"),
                            format!("DSH route '{route}' cannot be converted: {error}"),
                        )
                    })?,
                    None,
                );
                provider.meta = Some(ProviderMeta {
                    live_config_managed: Some(true),
                    ..Default::default()
                });
                providers.push(DshLiveProvider {
                    is_current: pointer.as_deref() == Some(route),
                    provider,
                });
            }
        }
    }

    if let Some(row) = row_config(&text, DEEPSEEK_ROW)? {
        let base_url = row
            .get(serde_yaml::Value::String("baseURL".into()))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());
        let models = models_from_yaml(row.get(serde_yaml::Value::String("models".into())));
        let api_key_env = row
            .get(serde_yaml::Value::String("apiKeyEnv".into()))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_KEY_REF.to_string());
        if !models.is_empty() {
            let config = DshProviderConfig {
                kind: Some("deepseek-official".into()),
                route: Some(OFFICIAL_ROUTE.to_string()),
                display_name: Some("DeepSeek".into()),
                api: Some("anthropic-messages".into()),
                base_url: Some(base_url),
                api_key: read_credential(&credentials, &api_key_env),
                api_key_env: Some(api_key_env),
                models,
                reasoning_effort: None,
            };
            let mut provider = Provider::with_id(
                OFFICIAL_ROUTE.to_string(),
                "DeepSeek".to_string(),
                serde_json::to_value(&config).map_err(|error| {
                    invalid(
                        format!("DSH 官方路由无法转换：{error}"),
                        format!("The DSH official route cannot be converted: {error}"),
                    )
                })?,
                None,
            );
            provider.category = Some("official".to_string());
            provider.meta = Some(ProviderMeta {
                live_config_managed: Some(true),
                ..Default::default()
            });
            providers.push(DshLiveProvider {
                is_current: pointer.as_deref() == Some(OFFICIAL_ROUTE),
                provider,
            });
        }
    }

    Ok(providers)
}

// ============================================================================
// MCP 服务器（每个服务器一条 `dsh-mcp-client` 行）
// ============================================================================

/// DSH 的 MCP 客户端插件名：`serverName` 唯一，一台服务器一条行。
const MCP_PLUGIN: &str = "@deepseek-ai/dsh-mcp-client";
/// CC Switch 建的 MCP 行 id 前缀，用于把自己的行和用户手写的区分开。
const MCP_ROW_PREFIX: &str = "mcp-ccs-";
/// MCP 行的连接字段：更新时先删掉这些，再把新的并进去，其它字段原样保留。
const MCP_TRANSPORT_FIELDS: [&str; 8] = [
    "transport",
    "serverName",
    "command",
    "args",
    "env",
    "cwd",
    "url",
    "headers",
];

/// profile 里的一条 MCP 行。
#[derive(Debug, Clone)]
pub struct DshMcpEntry {
    pub row_id: String,
    pub server_name: Option<String>,
    pub config: Value,
    pub disabled: bool,
    /// 行里有 `!!js` 表达式：可以读、可以禁用/删除，但不重写配置（会丢表达式）。
    pub has_js: bool,
}

fn row_key(block: &str, key: &str) -> Option<String> {
    for (index, line) in block.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if index > 0 && indent != 2 {
            continue;
        }
        let body = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let Some(rest) = body.strip_prefix(&format!("{key}:")) else {
            continue;
        };
        let value = rest.trim().trim_matches(|c| c == '\'' || c == '"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn row_disabled(block: &str) -> bool {
    block.lines().any(|line| {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        indent == 2
            && trimmed.starts_with("disabled:")
            && matches!(trimmed["disabled:".len()..].trim(), "true" | "yes" | "True")
    })
}

fn has_js_expression(block: &str) -> bool {
    block.contains("!!js")
}

/// 把 `!!js` 值换成字符串占位，好让只读解析能拿到其它字段（serverName、transport）。
fn sanitize_js_tags(block: &str) -> String {
    block
        .lines()
        .map(|line| match line.find("!!js") {
            Some(index) => format!("{}\"__ccs_js__\"", &line[..index]),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_row_mapping_relaxed(block: &str, id: &str) -> Result<serde_yaml::Mapping, AppError> {
    match parse_row_mapping(block, id) {
        Ok(mapping) => Ok(mapping),
        Err(strict_error) => {
            let sanitized = sanitize_js_tags(block);
            parse_row_mapping(&sanitized, id).map_err(|_| strict_error)
        }
    }
}

fn mcp_entries_from(text: &str) -> Result<Vec<DshMcpEntry>, AppError> {
    let mut entries = Vec::new();
    for span in scan_rows(text) {
        let block = row_block(&span, text);
        if row_key(block, "name").as_deref() != Some(MCP_PLUGIN) {
            continue;
        }
        let Some(row_id) = row_key(block, "id") else {
            continue;
        };
        let mapping = parse_row_mapping_relaxed(block, &row_id)?;
        let config = mapping
            .get(serde_yaml::Value::String("config".into()))
            .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        let server_name = config
            .get("serverName")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| row_key(block, "serverName"));
        entries.push(DshMcpEntry {
            row_id,
            server_name,
            config,
            disabled: row_disabled(block),
            has_js: has_js_expression(block),
        });
    }
    Ok(entries)
}

/// 读 profile 里的全部 MCP 行。
pub fn read_mcp_servers(paths: &DshPaths) -> Result<Vec<DshMcpEntry>, AppError> {
    let text = read_text_or_empty(&paths.patch_path())?;
    mcp_entries_from(&text)
}

fn slug_server_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "server".to_string()
    } else {
        out
    }
}

fn unique_server_name(base: &str, entries: &[DshMcpEntry], skip_row: Option<&str>) -> String {
    let taken: Vec<&str> = entries
        .iter()
        .filter(|entry| Some(entry.row_id.as_str()) != skip_row)
        .filter_map(|entry| entry.server_name.as_deref())
        .collect();
    if !taken.contains(&base) {
        return base.to_string();
    }
    for index in 2..1000 {
        let suffix = format!("-{index}");
        let mut candidate = base.to_string();
        let keep = 32usize.saturating_sub(suffix.len());
        candidate.truncate(keep);
        while candidate.ends_with('-') {
            candidate.pop();
        }
        candidate.push_str(&suffix);
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    format!("server-{}", uuid_like())
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

fn mcp_row_id(server_id: &str) -> String {
    let mut out = String::from(MCP_ROW_PREFIX);
    for c in server_id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.len() == MCP_ROW_PREFIX.len() {
        out.push_str("server");
    }
    out
}

fn find_mcp_entry<'a>(
    entries: &'a [DshMcpEntry],
    row_id: &str,
    name_hint: &str,
) -> Option<&'a DshMcpEntry> {
    entries
        .iter()
        .find(|entry| entry.row_id == row_id)
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.server_name.as_deref() == Some(name_hint))
        })
}

/// 文本级改一行的 `disabled:`，不动配置内容（`!!js` 表达式也不会被重写）。
fn set_row_disabled(text: &str, id: &str, disabled: bool) -> String {
    let Some(span) = find_row(text, id) else {
        return text.to_string();
    };
    let block = row_block(&span, text);
    let mut lines: Vec<String> = block.lines().map(str::to_string).collect();
    let disabled_at = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        line.len() - trimmed.len() == 2 && trimmed.starts_with("disabled:")
    });
    match (disabled, disabled_at) {
        (true, Some(index)) => lines[index] = "  disabled: true".to_string(),
        (true, None) => {
            let insert_at = lines
                .iter()
                .position(|line| line.trim_start().starts_with("name:"))
                .map(|index| index + 1)
                .unwrap_or(1);
            lines.insert(insert_at, "  disabled: true".to_string());
        }
        (false, Some(index)) => {
            lines.remove(index);
        }
        (false, None) => {}
    }
    let mut rebuilt = lines.join("\n");
    if block.ends_with('\n') {
        rebuilt.push('\n');
    }
    let mut out = String::with_capacity(text.len() + rebuilt.len());
    out.push_str(&text[..span.start]);
    out.push_str(&rebuilt);
    out.push_str(&text[span.end..]);
    out
}

fn write_row_config(
    text: &str,
    row_id: &str,
    config: serde_yaml::Mapping,
) -> Result<String, AppError> {
    upsert_row(text, row_id, MCP_PLUGIN, config)
}

/// 写入（或更新）一条 MCP 行并启用它。
///
/// `native` 是已经去过 CC Switch 外壳的 DSH 原生配置（`transport` + 连接字段）；
/// `server_name_hint` 由调用方按 cc-switch 的服务器 id 生成，这里负责去重。
pub fn upsert_mcp_server(
    paths: &DshPaths,
    server_id: &str,
    server_name_hint: &str,
    native: &Value,
) -> Result<DshMcpEntry, AppError> {
    let _guard = write_lock();
    let patch_path = paths.patch_path();
    let snapshot = snapshot(&patch_path)?;
    let result = (|| -> Result<DshMcpEntry, AppError> {
        let text = read_text_or_empty(&patch_path)?;
        let entries = mcp_entries_from(&text)?;
        let row_id = mcp_row_id(server_id);
        let hint = slug_server_name(server_name_hint);
        let target = find_mcp_entry(&entries, &row_id, &hint).cloned();

        if let Some(entry) = target.as_ref() {
            if entry.has_js {
                return Err(AppError::localized(
                    "dsh.mcp.js_expression",
                    format!(
                        "DSH 里的 MCP 行 `{}` 含有 !!js 表达式，CC Switch 不改写它。请在 DSH 设置里调整这条服务器。",
                        entry.row_id
                    ),
                    format!(
                        "MCP row `{}` in DSH contains a !!js expression; CC Switch will not rewrite it. Edit that server inside DSH.",
                        entry.row_id
                    ),
                ));
            }
        }

        let target_row_id = target
            .as_ref()
            .map(|entry| entry.row_id.clone())
            .unwrap_or(row_id);
        if target.is_none() && find_row(&text, &target_row_id).is_some() {
            return Err(AppError::localized(
                "dsh.mcp.row_conflict",
                format!("DSH 配置里已有 id 为 `{target_row_id}` 的其它行，换一个服务器 id 再试"),
                format!(
                    "The DSH profile already has a row with id `{target_row_id}`; choose another server id"
                ),
            ));
        }
        let server_name = target
            .as_ref()
            .and_then(|entry| entry.server_name.clone())
            .unwrap_or_else(|| unique_server_name(&hint, &entries, Some(&target_row_id)));

        let mut merged = target
            .as_ref()
            .and_then(|entry| entry.config.as_object().cloned())
            .unwrap_or_default();
        for field in MCP_TRANSPORT_FIELDS {
            merged.remove(field);
        }
        let Some(native_object) = native.as_object() else {
            return Err(invalid(
                "DSH MCP 配置必须是 JSON 对象".to_string(),
                "DSH MCP configuration must be a JSON object".to_string(),
            ));
        };
        for (key, value) in native_object {
            merged.insert(key.clone(), value.clone());
        }
        merged.insert("serverName".into(), Value::String(server_name.clone()));

        let config = json_to_yaml(&Value::Object(merged.clone()))?
            .as_mapping()
            .cloned()
            .unwrap_or_default();
        let mut next = write_row_config(&text, &target_row_id, config)?;
        next = set_row_disabled(&next, &target_row_id, false);
        write_patch(paths, &next)?;

        Ok(DshMcpEntry {
            row_id: target_row_id,
            server_name: Some(server_name),
            config: Value::Object(merged),
            disabled: false,
            has_js: false,
        })
    })();
    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
    }
    result
}

/// 取消勾选：只把这一行标成 `disabled: true`，保留配置（含 `!!js`）。
pub fn disable_mcp_server(
    paths: &DshPaths,
    server_id: &str,
    server_name_hint: &str,
) -> Result<(), AppError> {
    let _guard = write_lock();
    let patch_path = paths.patch_path();
    let snapshot = snapshot(&patch_path)?;
    let result = (|| -> Result<(), AppError> {
        let text = read_text_or_empty(&patch_path)?;
        let entries = mcp_entries_from(&text)?;
        let row_id = mcp_row_id(server_id);
        let hint = slug_server_name(server_name_hint);
        let Some(target) = find_mcp_entry(&entries, &row_id, &hint) else {
            return Ok(());
        };
        let next = set_row_disabled(&text, &target.row_id, true);
        write_patch(paths, &next)
    })();
    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
    }
    result
}

/// 删除服务器时只清理 CC Switch 自己建的行（`mcp-ccs-*`）。
///
/// 取消勾选之后 `apps.dsh` 已经是 false，但行还留在 profile 里；删除服务器时要顺手
/// 清掉它。只认自己的行 id：用户手写的同名服务器（别的行 id）不动。
pub fn remove_owned_mcp_server(paths: &DshPaths, server_id: &str) -> Result<(), AppError> {
    let _guard = write_lock();
    let patch_path = paths.patch_path();
    let snapshot = snapshot(&patch_path)?;
    let result = (|| -> Result<(), AppError> {
        let text = read_text_or_empty(&patch_path)?;
        let row_id = mcp_row_id(server_id);
        if find_row(&text, &row_id).is_none() {
            return Ok(());
        }
        let next = remove_row(&text, &row_id);
        write_patch(paths, &next)
    })();
    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
    }
    result
}

/// 删除服务器：删掉对应行。
pub fn remove_mcp_server(
    paths: &DshPaths,
    server_id: &str,
    server_name_hint: &str,
) -> Result<(), AppError> {
    let _guard = write_lock();
    let patch_path = paths.patch_path();
    let snapshot = snapshot(&patch_path)?;
    let result = (|| -> Result<(), AppError> {
        let text = read_text_or_empty(&patch_path)?;
        let entries = mcp_entries_from(&text)?;
        let row_id = mcp_row_id(server_id);
        let hint = slug_server_name(server_name_hint);
        let Some(target) = find_mcp_entry(&entries, &row_id, &hint) else {
            return Ok(());
        };
        let next = remove_row(&text, &target.row_id);
        write_patch(paths, &next)
    })();
    if result.is_err() {
        if let Err(restore_error) = restore(&patch_path, snapshot.as_ref()) {
            log::error!("Failed to roll back DSH profile patch: {restore_error}");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXISTING_PATCH: &str = "\
# user patch layer
- id: agent-default-model
  name: '@deepseek-ai/dsh-agent-default-model'
  config:
    provider: deepseek-account
    model: deepseek-flash
    reasoningEffort: max

- id: plugin-manager
  name: '@deepseek-ai/dsh-plugin-manager'
  disabled: !!js \"!ctx.get('profileContext')\"

- id: permission
  name: '@deepseek-ai/dsh-permission-presets'
  config:
    defaultPreset: danger-full-access
";

    fn test_paths() -> (tempfile::TempDir, DshPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = DshPaths::at(dir.path());
        std::fs::create_dir_all(paths.profile_dir()).expect("profile dir");
        (dir, paths)
    }

    fn seed_patch(paths: &DshPaths, text: &str) {
        std::fs::write(paths.patch_path(), text).expect("seed patch");
    }

    fn custom_provider(route: &str, name: &str, key: &str) -> Provider {
        Provider::with_id(
            format!("prov-{route}"),
            name.to_string(),
            json!({
                "kind": "custom",
                "route": route,
                "displayName": name,
                "api": "anthropic-messages",
                "baseURL": "http://127.0.0.1:8787/",
                "apiKey": key,
                "models": [
                    {"id": "mock-sonnet", "name": "Mock Sonnet", "contextWindow": 200000, "maxTokens": 8192, "input": ["text"]}
                ],
                "reasoningEffort": "high"
            }),
            None,
        )
    }

    fn official_provider() -> Provider {
        Provider::with_id(
            "deepseek-official".to_string(),
            "DeepSeek".to_string(),
            json!({
                "kind": "deepseek-official",
                "baseURL": "https://api.deepseek.com/anthropic",
                "apiKey": "sk-official",
                "models": [{"id": "deepseek-chat", "name": "DeepSeek Chat"}]
            }),
            None,
        )
    }

    fn read(paths: &DshPaths) -> String {
        std::fs::read_to_string(paths.patch_path()).unwrap_or_default()
    }

    #[test]
    fn normalize_route_and_key_ref_match_dsh_rules() {
        assert_eq!(normalize_route("My Gateway!"), "my-gateway");
        assert_eq!(normalize_route("9router"), "dsh-9router");
        assert_eq!(normalize_route("a__b"), "a-b");
        assert_eq!(derive_key_ref("my-gateway"), "MY_GATEWAY_API_KEY");
        assert!(is_valid_route("my-gateway-2"));
        assert!(!is_valid_route("My-Gateway"));
        assert!(!is_valid_route("my--gateway"));
        assert!(!is_valid_route("9router"));
    }

    #[test]
    fn switch_writes_route_credentials_and_pointer_without_touching_other_rows() {
        let (_dir, paths) = test_paths();
        seed_patch(&paths, EXISTING_PATCH);

        let outcome = switch_to(
            &paths,
            &custom_provider("local-mock", "Local Mock", "sk-test"),
        )
        .expect("switch");
        assert_eq!(outcome.route, "local-mock");
        assert_eq!(outcome.model, "mock-sonnet");

        let text = read(&paths);
        assert!(text.contains("# user patch layer"));
        assert!(text.contains("!!js \"!ctx.get('profileContext')\""));
        assert!(text.contains("- id: permission"));
        assert_eq!(text.matches("- id: llm-pi-ai").count(), 1);
        assert_eq!(text.matches("- id: agent-default-model").count(), 1);
        assert!(text.contains("local-mock:"));
        assert!(text.contains("baseURL: http://127.0.0.1:8787"));
        assert!(text.contains("apiKeyEnv: LOCAL_MOCK_API_KEY"));

        let pointer = row_config(&text, DEFAULT_MODEL_ROW).unwrap().unwrap();
        assert_eq!(
            pointer
                .get(serde_yaml::Value::String("provider".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("local-mock")
        );
        assert_eq!(
            pointer
                .get(serde_yaml::Value::String("model".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("mock-sonnet")
        );
        assert_eq!(
            pointer
                .get(serde_yaml::Value::String("reasoningEffort".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("high")
        );

        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(
            read_credential(&credentials, "LOCAL_MOCK_API_KEY").as_deref(),
            Some("sk-test")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.credentials_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "credentials must stay private");
        }
    }

    #[test]
    fn switching_twice_keeps_existing_routes() {
        let (_dir, paths) = test_paths();
        switch_to(&paths, &custom_provider("route-a", "A", "sk-a")).unwrap();
        switch_to(&paths, &custom_provider("route-b", "B", "sk-b")).unwrap();

        let text = read(&paths);
        assert!(text.contains("route-a:"));
        assert!(text.contains("route-b:"));
        assert_eq!(text.matches("- id: llm-pi-ai").count(), 1);

        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(
            read_credential(&credentials, "ROUTE_A_API_KEY").as_deref(),
            Some("sk-a")
        );
        assert_eq!(
            read_credential(&credentials, "ROUTE_B_API_KEY").as_deref(),
            Some("sk-b")
        );
    }

    #[test]
    fn official_switch_writes_the_deepseek_row() {
        let (_dir, paths) = test_paths();
        seed_patch(&paths, EXISTING_PATCH);
        switch_to(&paths, &official_provider()).unwrap();

        let text = read(&paths);
        assert_eq!(text.matches("- id: llm-deepseek").count(), 1);
        assert!(text.contains("baseURL: https://api.deepseek.com/anthropic"));
        assert!(text.contains("apiKeyEnv: DEEPSEEK_API_KEY"));
        assert!(!text.contains("- id: llm-pi-ai"));

        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(
            read_credential(&credentials, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-official")
        );

        let live = read_live_providers(&paths).unwrap();
        assert_eq!(live.len(), 1);
        assert!(live[0].is_current);
        assert_eq!(live[0].provider.id, OFFICIAL_ROUTE);
        assert_eq!(live[0].provider.category.as_deref(), Some("official"));
    }

    #[test]
    fn removing_the_current_route_moves_the_pointer_to_a_remaining_one() {
        let (_dir, paths) = test_paths();
        let a = custom_provider("route-a", "A", "sk-a");
        let b = custom_provider("route-b", "B", "sk-b");
        switch_to(&paths, &a).unwrap();
        switch_to(&paths, &b).unwrap();

        let warnings = remove_provider_route(&paths, &b).unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.starts_with("dsh.default_model_reassigned")));

        let text = read(&paths);
        assert!(!text.contains("route-b:"));
        assert!(text.contains("route-a:"));
        let pointer = row_config(&text, DEFAULT_MODEL_ROW).unwrap().unwrap();
        assert_eq!(
            pointer
                .get(serde_yaml::Value::String("provider".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("route-a")
        );

        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(read_credential(&credentials, "ROUTE_B_API_KEY"), None);
        assert_eq!(
            read_credential(&credentials, "ROUTE_A_API_KEY").as_deref(),
            Some("sk-a")
        );
    }

    #[test]
    fn saving_with_an_empty_key_keeps_the_stored_credential() {
        let (_dir, paths) = test_paths();
        switch_to(&paths, &custom_provider("route-a", "A", "sk-a")).unwrap();

        let mut without_key = custom_provider("route-a", "A", "");
        without_key.settings_config["apiKey"] = serde_json::Value::Null;
        set_provider_route(&paths, &without_key).unwrap();

        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(
            read_credential(&credentials, "ROUTE_A_API_KEY").as_deref(),
            Some("sk-a")
        );
    }

    #[test]
    fn removing_the_last_managed_route_cleans_up_the_rows() {
        let (_dir, paths) = test_paths();
        let provider = custom_provider("route-a", "A", "sk-a");
        switch_to(&paths, &provider).unwrap();
        remove_provider_route(&paths, &provider).unwrap();

        let text = read(&paths);
        assert!(!text.contains("- id: llm-pi-ai"), "{text}");
        assert!(!text.contains("- id: agent-default-model"), "{text}");
        let credentials = read_credentials(&paths).unwrap();
        assert_eq!(read_credential(&credentials, "ROUTE_A_API_KEY"), None);
    }

    #[test]
    fn removal_of_an_unreferenced_route_keeps_the_pointer() {
        let (_dir, paths) = test_paths();
        let a = custom_provider("route-a", "A", "sk-a");
        switch_to(&paths, &a).unwrap();
        let b = custom_provider("route-b", "B", "sk-b");
        set_provider_route(&paths, &b).unwrap();

        remove_provider_route(&paths, &b).unwrap();
        let text = read(&paths);
        let pointer = row_config(&text, DEFAULT_MODEL_ROW).unwrap().unwrap();
        assert_eq!(
            pointer
                .get(serde_yaml::Value::String("provider".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("route-a")
        );
    }

    #[test]
    fn read_live_providers_roundtrips_what_was_written() {
        let (_dir, paths) = test_paths();
        switch_to(
            &paths,
            &custom_provider("local-mock", "Local Mock", "sk-test"),
        )
        .unwrap();
        set_provider_route(&paths, &official_provider()).unwrap();
        // 官方行是后写的，指针还停在 local-mock 上。
        switch_to(
            &paths,
            &custom_provider("local-mock", "Local Mock", "sk-test"),
        )
        .unwrap();

        let live = read_live_providers(&paths).unwrap();
        assert_eq!(live.len(), 2);
        let mock = live
            .iter()
            .find(|item| item.provider.id == "local-mock")
            .expect("mock route");
        assert!(mock.is_current);
        let config: DshProviderConfig =
            serde_json::from_value(mock.provider.settings_config.clone()).unwrap();
        assert_eq!(config.base_url.as_deref(), Some("http://127.0.0.1:8787"));
        assert_eq!(config.api_key.as_deref(), Some("sk-test"));
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.models[0].context_window, Some(200000));

        let official = live
            .iter()
            .find(|item| item.provider.id == OFFICIAL_ROUTE)
            .expect("official route");
        assert!(!official.is_current);
        assert_eq!(official.provider.category.as_deref(), Some("official"));
    }

    #[test]
    fn invalid_configuration_is_rejected_before_writing() {
        let (_dir, paths) = test_paths();
        let mut provider = custom_provider("local-mock", "Local Mock", "sk-test");
        provider.settings_config["models"] = json!([]);
        let error = switch_to(&paths, &provider).unwrap_err().to_string();
        assert!(error.contains("至少需要一个模型"), "{error}");
        assert!(!paths.patch_path().exists());

        let mut provider = custom_provider("local-mock", "Local Mock", "sk-test");
        provider.settings_config["baseURL"] = json!("ftp://example.com");
        let error = switch_to(&paths, &provider).unwrap_err().to_string();
        assert!(error.contains("http://"), "{error}");

        let mut provider = custom_provider("local-mock", "Local Mock", "sk-test");
        provider.settings_config["api"] = json!("gemini-native");
        let error = switch_to(&paths, &provider).unwrap_err().to_string();
        assert!(error.contains("协议"), "{error}");
    }

    #[test]
    fn a_broken_target_row_is_reported_instead_of_overwritten() {
        let (_dir, paths) = test_paths();
        seed_patch(
            &paths,
            "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config: [unclosed\n",
        );
        let error = switch_to(&paths, &custom_provider("route-a", "A", "sk-a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("llm-pi-ai"), "{error}");
        assert_eq!(
            std::fs::read_to_string(paths.patch_path()).unwrap(),
            "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config: [unclosed\n"
        );
    }

    fn stdio_mcp_config() -> Value {
        json!({
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": {"GITHUB_TOKEN": "x"}
        })
    }

    #[test]
    fn mcp_rows_roundtrip_disable_and_remove() {
        let (_dir, paths) = test_paths();
        seed_patch(&paths, EXISTING_PATCH);

        let entry =
            upsert_mcp_server(&paths, "github", "github", &stdio_mcp_config()).expect("upsert mcp");
        assert_eq!(entry.row_id, "mcp-ccs-github");
        assert_eq!(entry.server_name.as_deref(), Some("github"));
        assert!(!entry.disabled);

        let text = read(&paths);
        assert!(text.contains("name: '@deepseek-ai/dsh-mcp-client'"));
        assert!(text.contains("transport: stdio"));
        assert!(text.contains("# user patch layer"));

        disable_mcp_server(&paths, "github", "github").expect("disable");
        let disabled = read_mcp_servers(&paths).expect("read mcp");
        assert_eq!(disabled.len(), 1);
        assert!(disabled[0].disabled);
        // 禁用只动 disabled，README 里的连接字段原样保留
        assert!(read(&paths).contains("command: npx"));

        upsert_mcp_server(&paths, "github", "github", &stdio_mcp_config()).expect("re-enable");
        let enabled = read_mcp_servers(&paths).expect("read mcp");
        assert!(!enabled[0].disabled);

        remove_mcp_server(&paths, "github", "github").expect("remove");
        assert!(read_mcp_servers(&paths).unwrap().is_empty());
        assert!(!read(&paths).contains("dsh-mcp-client"));
    }

    #[test]
    fn mcp_updates_replace_transport_fields_only() {
        let (_dir, paths) = test_paths();
        upsert_mcp_server(&paths, "web", "web", &stdio_mcp_config()).unwrap();
        upsert_mcp_server(
            &paths,
            "web",
            "web",
            &json!({
                "transport": "streamable-http",
                "url": "https://example.test/mcp",
                "headers": {"Authorization": "Bearer x"}
            }),
        )
        .expect("switch transport");

        let entries = read_mcp_servers(&paths).unwrap();
        assert_eq!(entries.len(), 1);
        let config = &entries[0].config;
        assert_eq!(config["transport"], "streamable-http");
        assert_eq!(config["url"], "https://example.test/mcp");
        assert!(config.get("command").is_none());
    }

    #[test]
    fn mcp_adopts_an_existing_row_with_the_same_server_name() {
        let (_dir, paths) = test_paths();
        seed_patch(
            &paths,
            "- id: mcp-github\n  name: '@deepseek-ai/dsh-mcp-client'\n  config:\n    transport: stdio\n    serverName: github\n    command: old-cmd\n",
        );

        upsert_mcp_server(&paths, "github", "github", &stdio_mcp_config())
            .expect("adopt existing row");

        let text = read(&paths);
        assert_eq!(text.matches("dsh-mcp-client").count(), 1);
        assert!(text.contains("- id: mcp-github"));
        assert!(!text.contains("mcp-ccs-github"));
        assert!(text.contains("command: npx"));
    }

    #[test]
    fn mcp_rejects_rewriting_js_rows_but_can_disable_them() {
        let (_dir, paths) = test_paths();
        seed_patch(
            &paths,
            "- id: mcp-github\n  name: '@deepseek-ai/dsh-mcp-client'\n  config:\n    transport: stdio\n    serverName: github\n    command: npx\n    env:\n      GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN\n",
        );

        let entries = read_mcp_servers(&paths).expect("read js row");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].has_js);
        assert_eq!(entries[0].server_name.as_deref(), Some("github"));

        let error = upsert_mcp_server(&paths, "github", "github", &stdio_mcp_config())
            .expect_err("js rows are not rewritten");
        assert!(error.to_string().contains("!!js"), "{error}");

        disable_mcp_server(&paths, "github", "github").expect("disable js row");
        let text = read(&paths);
        assert!(text.contains("disabled: true"));
        assert!(text.contains("!!js process.env.GITHUB_TOKEN"));
    }

    #[test]
    fn remove_owned_mcp_server_only_touches_ccs_rows() {
        let (_dir, paths) = test_paths();
        seed_patch(
            &paths,
            "- id: mcp-github\n  name: '@deepseek-ai/dsh-mcp-client'\n  config:\n    transport: stdio\n    serverName: github\n    command: user-cmd\n",
        );
        // 用户手写的行 id 不是 mcp-ccs-*，按 id 清理时不能动它
        remove_owned_mcp_server(&paths, "github").expect("cleanup");
        assert!(read(&paths).contains("user-cmd"));

        upsert_mcp_server(&paths, "gitlab", "gitlab", &stdio_mcp_config()).unwrap();
        remove_owned_mcp_server(&paths, "gitlab").expect("cleanup own row");
        assert!(!read(&paths).contains("mcp-ccs-gitlab"));
    }

    #[test]
    fn mcp_servers_with_different_ids_get_their_own_rows() {
        let (_dir, paths) = test_paths();
        upsert_mcp_server(&paths, "alpha", "alpha", &stdio_mcp_config()).unwrap();
        upsert_mcp_server(&paths, "beta", "beta", &stdio_mcp_config()).unwrap();

        let entries = read_mcp_servers(&paths).unwrap();
        assert_eq!(entries.len(), 2);
        let names: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.server_name.as_deref())
            .collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn dsh_paths_follow_dsh_home() {
        let resolved = DshPaths::at("/tmp/dsh-home");
        assert_eq!(
            resolved.patch_path(),
            PathBuf::from("/tmp/dsh-home/profiles/desktop/cordis.patch.yml")
        );
        assert_eq!(
            resolved.credentials_path(),
            PathBuf::from("/tmp/dsh-home/.credentials.yaml")
        );
        assert_eq!(
            resolved.agents_path(),
            PathBuf::from("/tmp/dsh-home/AGENTS.md")
        );
    }
}
