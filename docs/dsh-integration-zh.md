# DeepSeek Harness（DSH）接入 CC Switch — 调研与适配方案

> 基线：cc-switch `4.0.18`（本仓库 HEAD `cb90a244`）；DeepSeek Harness `0.2.1-rc.2`（`/Applications/DeepSeek Harness.app/Contents/Resources/app.asar` 内 `dsh` 运行时）。
> 本文分三块：① 现行支持应用盘点 → ② 各应用适配机制与代码 → ③ DSH 源码结论与落地方案。

---

## 0. 结论摘要

1. **cc-switch 现行支持 11 个应用**：`claude`、`claude-desktop`、`codex`、`gemini`、`grokbuild`、`opencode`、`openclaw`、`hermes`、`pi`、`mcode`、`agy`。其中 5 个走「切换式整文件覆盖」（Claude 系 / Codex / Gemini / Grok Build），5 个走「累加式条目共存 + 启用标记」，`agy` 是特例（私有目录 + 终端注入环境变量）。
2. **DSH 的适配形态最接近 `hermes`**：路由（provider）是配置文件里的条目，可以多条共存；「当前用哪个」由另一个指针行（`agent-default-model`）决定。所以 DSH 应实现为**累加式应用 + 切换时重写默认模型指针**，而不是整文件覆盖。
3. **DSH 侧只需写三个位置**：
   - `~/.dsh/profiles/desktop/cordis.patch.yml` → `llm-pi-ai` 行的 `config.providers.<route>`（第三方端点），或 `llm-deepseek` 行（官方 DeepSeek 端点）；
   - `~/.dsh/.credentials.yaml` → `refs.<ROUTE>_API_KEY: sk-...`（0600 权限，与 DSH 设置页同一份存储）；
   - 同一份 patch 的 `agent-default-model` 行 → `{provider, model, reasoningEffort}`，这就是「当前供应商」。
4. **patch 的语义是「整行 config 替换，不深度合并」**，且 DSH 自己写入时用 `<file>.lock` + 原子 rename。因此写入器必须做**读-改-写**并保留非托管路由，不能只写自己那一段。
5. **MVP 不改数据库**：`providers` 表按 `(id, app_type)` 存，新应用不需要迁移；只有做 MCP / Skills 的独立开关时才需要 `SCHEMA_VERSION 21 → 22` 增加 `enabled_dsh` 列。
6. **风险集中在三点**：YAML 保真（用户 patch 里有注释，bundle 层有 `!!js` 标签）、双写者冲突（DSH 运行中用户也在设置页改）、协议/模型元数据猜错。方案里各有对应处理。

---

## 1. 现行 cc-switch 支持的应用

### 1.1 注册表（前端就是权威清单）

- 前端 `AppId`：`src/lib/api/types.ts:2`
- 前端应用清单：`src/config/appConfig.tsx:19`（`APP_IDS`）、`:33`（`DEFAULT_VISIBLE_APPS`）、`:48`（`SKILLS_APP_IDS`）、`:66`（`PROXY_APP_IDS`）
- 可见性开关类型：`src/types.ts:307`（`VisibleApps`）
- 后端枚举：`src-tauri/src/app_config.rs:418`（`AppType`）、`:437`（`as_str`）、`:493`（`all()`）、`:515`（`FromStr`）

### 1.2 逐应用适配方式

| App | 显示名 | 模式 | 客户端写入目标 | 关键代码 |
| --- | --- | --- | --- | --- |
| `claude` | Claude Code | 切换式 | `~/.claude/settings.json`（`env.ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN`） | `services/provider/claude_direct.rs`、`claude_editor.rs` |
| `claude-desktop` | Claude Desktop | 切换式（仅直连/路由） | Claude Desktop 配置库 JSON | `claude_desktop_config.rs:135` |
| `codex` | Codex | 切换式 | `~/.codex/auth.json` + `~/.codex/config.toml` | `codex_config.rs:270/279/832`、`codex_direct.rs` |
| `gemini` | Gemini CLI | 切换式 | `~/.gemini/settings.json` + `.env` | `gemini_config.rs:9/18/329`、`gemini_direct.rs` |
| `grokbuild` | Grok Build | 切换式 | `~/.grok/config.toml` | `grok_config.rs:25/30`、`grok_direct.rs` |
| `opencode` | OpenCode | 累加式 | `~/.config/opencode/opencode.json(c)`（`provider.<id>.options.baseURL/apiKey`） | `opencode_config.rs:49/59/108` |
| `openclaw` | OpenClaw | 累加式 | `~/.openclaw/openclaw.json`（顶层 `baseUrl/apiKey`） | `openclaw_config.rs:35/46` |
| `hermes` | Hermes | 累加式 + 指针 | `<hermes home>/config.yaml`（`custom_providers[]` + 顶层 `model.provider`） | `hermes_config.rs:53/101` |
| `pi` | Pi | 累加式 | `~/.pi/agent/models.json` + `settings.json` | `pi_config/mod.rs:34/75/79` |
| `mcode` | MiniMax Code | 累加式 | MiniMax Code 数据目录 | `mcode_config.rs:12/59` |
| `agy` | Antigravity CLI | 特例 | `~/.gemini/antigravity-cli/settings.json`（只写模型）+ `~/.cc-switch/agy/<id>/` 私有目录 + 启动时注入 `GEMINI_API_KEY` | `agy_config.rs:31/36/40/51` |

配套能力矩阵（决定了新应用默认能拿到什么）：

- **本地代理 / 故障转移**：`claude` / `codex` / `gemini` / `grokbuild`（`appConfig.tsx:66`、`app_config.rs` 的 `supports_local_proxy`）。
- **直连 / 路由模式切换**：上述 4 个 + `claude-desktop`（`has_route_mode`）。
- **Skills 面板**：9 个应用（`SKILLS_APP_IDS`，不含 `claude-desktop` / `openclaw`）。
- **托盘**：只有 5 个切换式应用（`tray.rs:41` `TRAY_APPS`），累加式应用不进托盘。
- **会话浏览**：`session_manager/providers/` 下已有 10 个 provider 模块（`mod.rs:1`）。
- **MCP / Skills 独立开关**：数据库列 `enabled_<app>`（`database/schema.rs:71`、`:103`），当前 `SCHEMA_VERSION = 21`（`database/mod.rs:54`）。

---

## 2. 适配机制与代码解剖

### 2.1 一次「切换」的完整链路

```
前端点卡片
  → invoke("switch_provider", { app, id })            commands/provider.rs:166
  → switch_provider_internal                           commands/provider.rs:148
  → ProviderService::switch                            services/provider/mod.rs:6560
  → switch_inner                                       services/provider/mod.rs:6588
       ├─ Pi  → pi::enable()（累加式专用启用）
       ├─ Agy → 直接拒绝（卡只在「打开终端」里生效）
       ├─ 路由模式（proxy）→ 只换路由，必要时先改写客户端
       └─ switch_normal                              services/provider/mod.rs:6665
            ├─ Claude / Codex / Gemini / Grok / Desktop → 各自 `*_direct.rs` 写客户端
            ├─ OpenCode OMO / OMO-Slim → 互斥写 + 删对方文件
            ├─ 累加式 → set_current_provider + write_live_for_state（live.rs:388）
            └─ Hermes 额外 apply_switch_defaults（把 config.yaml 的 model.provider 指向新条目）
```

要点：

- **切换式应用**的写入只在 `*_direct.rs` 里发生，`live.rs` 只做分发；写入走统一的原子写 + 备份 + 失败回滚。
- **累加式应用**切换时只保证条目存在于原生文件（`write_live_for_state`），并把 `live_config_managed` 标记翻成 `true`；失败会回滚刚写入的原生条目。
- **Hermes 是本次 DSH 的现成模板**：条目累加 + 切换时改一个「默认模型」指针。DSH 完全同构。

### 2.2 每个应用的「配置对象」形状

`Provider.settingsConfig` 是 `Record<string, any>`（`src/types.ts`），不同应用形状完全不同，由各自的直写/累加写函数解释：

- Claude：`{ env: { ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN, ... } }`
- Codex：`{ auth: {...}, config: "<TOML 文本>" }`
- Gemini / Agy：`{ env: { GEMINI_API_KEY, GOOGLE_GEMINI_BASE_URL }, config: { model: { name } } }`
- OpenCode / Mcode：`{ options: { baseURL, apiKey } }`
- OpenClaw：`{ baseUrl, apiKey }`（顶层驼峰）
- Hermes：`{ base_url, api_key }`（顶层蛇形）
- Pi：Pi 原生 `models.json.providers.<key>` 字段名（`pi_config::provider_base_url`）

凭证提取有统一入口 `Provider::resolve_usage_credentials`（`src-tauri/src/provider.rs:150` 起），**新增应用必须在这个 match 里显式加分支**——它是 `AppType` 的穷尽匹配，新枚举变体会直接编译失败，这是仓库刻意留的护栏（注释里写明了）。

### 2.3 新增一个应用要动的地方（以 `agy` 为样本的 checklist）

| 层 | 位置 | 必做 |
| --- | --- | --- |
| 类型 | `src/lib/api/types.ts`、`src/config/appConfig.tsx`、`src/types.ts` | 是 |
| 后端枚举 | `app_config.rs`（`AppType` / `as_str` / `FromStr` / `all()` / `MultiAppConfig::default`） | 是 |
| 写入器 | 新 `*_config.rs` + `services/provider/*_direct.rs`（或累加写函数） | 是 |
| 凭证提取 | `provider.rs:150` 的 match | 是（编译器强制） |
| 默认模型指针 | 若应用有「当前模型」概念，参考 `hermes_config::apply_switch_defaults` | 视应用 |
| 路径覆盖 | `settings.rs` 的 `get_*_override_dir` | 建议 |
| 托盘 | `tray.rs`（`TRAY_APPS` / `app_display_name`） | 累加式可不进 |
| 提示词文件 | `prompt_files.rs:21/42/52` | 视应用 |
| 会话 | `session_manager/providers/<app>.rs` + `mod.rs` | 二期 |
| 数据库 | `enabled_<app>` 列 + `SCHEMA_VERSION` 迁移 | 仅 MCP/Skills |
| 代理 | `proxy/providers/mod.rs`（当前累加式都 `return None`） | 不建议首期 |
| 前端表单/预设/文案 | `components/providers/forms/*`、`config/*ProviderPresets.ts`、`i18n/locales/*` | 是 |
| 文档 | `docs/<app>-integration-zh.md` | 是（本文件） |

`docs/agy-integration-zh.md` 是一份高质量的既有范式：范围、写入目标、凭据传递、失败回滚、验证命令、边界说明都写清楚了。DSH 文档按同样口径写。

---

## 3. DSH 侧研究（源码结论）

### 3.1 形态与版本

- 桌面端是 Electron 壳 + **Cordis 插件树**；业务能力全在 `@deepseek-ai/dsh-*` 包里（本机 307 个包，版本 `0.2.1-rc.2`，见 `dsh/package.json`）。
- 开源仓库：`github.com/deepseek-ai/deepseek-harness`（各包 `package.json.repository` 指向）。
- 桌面 profile 由 Electron **独占**：`dsh/lib/bin.js:41` 的 `rejectElectronProfile` 明确禁止 CLI 启动 `desktop` profile。→ cc-switch 只能改文件，不能靠 `dsh` 命令拉起/重启桌面端。

### 3.2 目录与分层

```
~/.dsh/                                  （可用 DSH_HOME 覆盖，dsh-home-paths/lib/index.js）
├── .credentials.yaml                    （0600，凭据；dsh-credentials-local/lib/index.js:49）
├── profiles/desktop/
│   ├── package.json                     （dsh.profile.bundles = [dsh-base, dsh-web-app, ...]）
│   ├── cordis.yml                       （profile 根：一个空数组）
│   ├── cordis.patch.yml                 （★ 用户 patch 层，CC Switch 的写入目标）
│   └── node_modules/                    （profile 局部插件）
├── sessions/                            （会话 JSONL，二期数据源）
├── storages/                            （workspace.json 等）
└── plugins/                             （本地插件）
```

层级合成顺序（`dsh/lib/profile-boot-*.js`）：`package.json` 里的每个 bundle patch → 用户 `cordis.patch.yml` → `--patch` 覆盖层。**后写覆盖先写，按行 id 定位，整行 `config` 替换**。

### 3.3 配置系统：patch 行 + settings.mutate

- DSH 设置页写配置的真实路径：`settings.mutate(ns, ops, expectedRevision)` → `configEditor.edit(entry, ...)` → 写 `profile.patchPath`（`dsh-settings/lib/index.js:488/502/508`、`dsh-config-editor/lib/index.js:24`）。
- 写文件用 `dsh-atomic-write`：`<file>.lock` 独占 + 原子 rename；凭据文件同样走锁（`dsh-atomic-write/lib/index.js`）。
- 热更新：`hmr` 插件默认开（`dsh-base/cordis.patch.yml` 的 `hmr` 行），profile 配置改动会在运行中重载对应行；**改了模型/供应商不需要重启应用**。

关键语义（决定写入器设计）：

> 「A patch replaces the targeted row's whole `config` rather than merging into it.」
> —— `dsh-base/cordis.patch.yml` 顶部注释
所以写 `llm-pi-ai` 行时必须带上**完整的 `providers` 字典**，否则会把用户其他路由一起抹掉。

### 3.4 供应商模型

`dsh-base/cordis.patch.yml` 里与本任务相关的行：

| 行 id | 插件包 | 作用 |
| --- | --- | --- |
| `llm` | `dsh-llm` | 供应商无关的 LLM 服务接口（`registerAdapter` / `registerConfigurableProviders`） |
| `llm-deepseek` | `dsh-llm-deepseek-api-key` | **官方 DeepSeek 路由**，路由名固定 `deepseek-official` |
| `llm-deepseek-account` | `dsh-llm-deepseek-account` | DeepSeek 平台账号登录路由（`deepseek-account`） |
| `llm-pi-ai` | `dsh-llm-pi-ai` | **多供应商路由容器**（默认 dormant，零路由） |
| `agent-default-model` | `dsh-agent-default-model` | 默认模型 = `{provider, model, reasoningEffort}` |
| `credentials` | `dsh-credentials-local` | 凭据服务 |

#### A. 官方 DeepSeek 行（`llm-deepseek`）

- 配置字段：`dsh-llm-deepseek/lib/index.js:318` 的 `deepSeekConfigFields` —— `baseURL`、`models[]`、`maxTokens`、`defaultContextWindow`、`thinking`、`reasoningEffort`、`retryPolicy` 等。
- 端点解析：`baseURL ?? env DEEPSEEK_BASE_URL ?? https://api.deepseek.com/anthropic`（`:347`、`:430`），必须是**无 query / 无凭据的 HTTP(S) 根**，否则直接报错。
- 鉴权：`x-api-key` 头，key 来自 credential ref，默认 `DEEPSEEK_API_KEY`（`dsh-llm-deepseek-api-key/lib/index.js`）。
- 结论：**只适合 Anthropic Messages 兼容端点**，且一个实例只有一条路由。

#### B. 多供应商行（`llm-pi-ai`）

- schema：`providers: z.dict(profile)`（`dsh-llm-pi-ai/lib/index.js:1083`），`profile` 字段见 `:1049`：
  `apiKeyEnv`、`displayName`、`api`、`baseURL`、`models[]`、`modelOverrides`、`compat`、`headers`、`reasoningEfforts`、`transport`、`timeoutMs` 等。
- 协议只有三种（`:834` 的 `PROTOCOLS`）：`openai-completions`、`openai-responses`、`anthropic-messages`。
- `models[]` 每项最小只需 `id`，可选 `name` / `contextWindow` / `maxTokens` / `input` / `reasoningEfforts` / `compat`。
- 路由命名（设置页同款规则，`dsh-client-ui-settings-models/lib/client.js`）：
  `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`，且派生 key ref：
  `deriveKeyRef(route) = route.toUpperCase().replace(/[^A-Z0-9]+/g, "_") + "_API_KEY"`（`client.js:1230`）。
- 设置页新建自定义供应商时写的就是这两步（`client.js:1535` `NS = "llm-pi-ai"`、`:1607` `settings.mutate` 的 `providers.<route>`、`:1616` `credentials.set`）。

→ **cc-switch 的第三方供应商应落在这条行里**，与 DSH 设置页完全同构，UI 里能看见、能改、能删。

### 3.5 凭据存储

`~/.dsh/.credentials.yaml`（`version: 1`，0600，路径可配）：

```yaml
version: 1
refs:                       # 环境变量式引用 → 明文 secret（cc-switch 写这里最省事）
  MY_GATEWAY_API_KEY: sk-...
records:                    # 结构化记录，key 形如 <scope>/<id>
  deepseek-account-platform/default:
    kind: grant
    payload: { version: 1, token: ..., issuer: ... }
  some-provider/route-x:
    kind: api-key
    key: sk-...
```

- 解析顺序：`refs` → 进程环境 → 项目/用户 `.env`（`dsh-credentials-local` 的 `resolve`）。
- 文件权限校验严格：group/other 任何位都会拒绝加载（`GROUP_OTHER_BITS = 63`）。cc-switch 写入必须 `0600`、目录 `0700`。

### 3.6 默认模型 / 「当前供应商」

- `agent-default-model` 行：`provider` + `model` + 可选 `reasoningEffort`（`dsh-agent-default-model/lib/index.js:21`）。
- 本机现状：该行被写成 `provider: deepseek-account`、`model: deepseek-flash`、`reasoningEffort: max`（`~/.dsh/profiles/desktop/cordis.patch.yml`）。
- 会话内还有模型选择器，会按路由列出候选；因此**新路由一旦写进 patch，模型选择器里立刻可选**，不依赖 cc-switch 再做什么。

### 3.7 运行时接口（可选增强路径）

- 桌面端跑一个本地 web 服务（本机 `http://127.0.0.1:56710`），对外暴露 gateway：`settings.describe/read/write`、`credentials.describe/set/unset` 等（`dsh-api-settings-controller/lib/index.js`）。
- 鉴权基于 `client-connection/browser-session` 的 grant（同样在 `.credentials.yaml` 里）。
- **首期不必用**：文件写入 + HMR 已足够；网关路径可作为「消除双写者冲突」的二期增强。

### 3.8 本机基线快照（用于回归）

```
~/.dsh/profiles/desktop/cordis.patch.yml  42 行，5 个行：agent-default-model / ui-settings-account / ui-chat / ui-settings / permission
~/.dsh/.credentials.yaml                  3 条记录：client-connection/browser-session、deepseek-account-platform/device、deepseek-account-platform/default
~/.dsh/profiles/desktop/package.json      bundles = [@deepseek-ai/dsh-base, @deepseek-ai/dsh-web-app, dsh-purge]
```

### 3.9 Provider → DSH 的字段映射

| CC Switch | DSH |
| --- | --- |
| `Provider.id` | route 名（规范化到 `^[a-z][a-z0-9-]*$`） |
| `Provider.name` | `providers.<route>.displayName` |
| `settingsConfig.baseURL` | `providers.<route>.baseURL`（或 `llm-deepseek.baseURL`） |
| `settingsConfig.api` | `providers.<route>.api`（三选一） |
| `settingsConfig.models[]` | `providers.<route>.models[]` |
| API Key | `.credentials.yaml` 的 `refs.<ROUTE>_API_KEY` |
| 「当前供应商」 | `agent-default-model` 行的 `provider` + `model` |
| 思考强度 | `agent-default-model.reasoningEffort` 或模型级 `reasoningEfforts` |

---

## 4. 适配方案

### 4.1 范围

**M1–M3（首期，MVP）**

- 应用 ID `dsh`，累加式；供应商 = DSH 路由；切换 = 确保路由存在 + 改默认模型指针。
- 官方 DeepSeek 卡（写 `llm-deepseek` 行）与第三方卡（写 `llm-pi-ai` 行）两种。
- 凭据写入 `.credentials.yaml` 的 `refs`。
- 启动导入（从 patch 反推路由）、全量同步、失败回滚、备份。
- 前端表单、预设、i18n、卡片状态、模型清单编辑。

**不做（首期）**

- 本地代理 / 故障转移 / 路由模式：DSH 是 Electron 单体，接管其出口需要独立数据面适配，不能只靠复用 UI 宣称可用。
- MCP / Skills / 会话 / 用量：已在后续迭代补齐（见 §7.4–§7.6）。
- 运行时 gateway 写入：二期增强（见 4.12）。

### 4.2 数据模型（`settingsConfig`）

DSH 原生形状 + 少量 cc-switch 元数据，避免二次翻译：

```json
{
  "kind": "pi-ai",
  "route": "my-gateway",
  "displayName": "My Gateway",
  "api": "anthropic-messages",
  "baseURL": "https://api.example.com",
  "apiKeyEnv": "MY_GATEWAY_API_KEY",
  "models": [
    {
      "id": "claude-sonnet-4-5",
      "name": "Sonnet 4.5",
      "contextWindow": 200000,
      "maxTokens": 32000,
      "input": ["text", "image"]
    }
  ],
  "reasoningEffort": "high"
}
```

官方卡：

```json
{
  "kind": "deepseek-official",
  "baseURL": "https://api.deepseek.com/anthropic",
  "apiKeyEnv": "DEEPSEEK_API_KEY",
  "models": [{ "id": "deepseek-chat", "name": "DeepSeek Chat", "contextWindow": 128000, "maxTokens": 8192 }]
}
```

规则：

- `kind == "deepseek-official"` 时只允许一条（DSH 侧路由名固定），cc-switch 里也只允许一个此类供应商。
- `route` / `apiKeyEnv` 由 cc-switch 规范化生成，用户可见可改（校验规则同 DSH）。
- `models` 至少 1 条；`contextWindow` / `maxTokens` 缺省给保守值，允许手改。

### 4.3 写入器：`src-tauri/src/dsh_config.rs`

职责：

1. **路径解析**：`DSH_HOME` → `~/.dsh`；`profiles/<profile>/cordis.patch.yml`（默认 `desktop`，可配置）；`.credentials.yaml`。新增 `settings.rs::get_dsh_override_dir`。
2. **读取**：解析 patch 为「行 id → config」；只认自己关心的三行（`llm-pi-ai`、`llm-deepseek`、`agent-default-model`），其余行**原文保留**。
3. **读-改-写**：
   - 读 `providers` 全量字典 → upsert/remove 目标 route → 写回整行 config；
   - 更新 `agent-default-model`；
   - 写 `refs`（新增/更新 key）。
4. **保真策略**（关键）：
   - 引入 `yaml_edit`（保留注释与格式）或等价方案；**禁止**用 `serde_yaml::to_string` 整文件重写（会丢注释、丢 `!!js` 标签，直接弄坏用户 patch）。
   - 遇到无法安全编辑的结构（自定义标签、行不是 mapping）→ 返回 localized 错误，提示「请在 DSH 设置页修改」，不做破坏性写入。
5. **并发与事务**：
   - 与 DSH 相同：先取 `<file>.lock`（`O_EXCL`，记录 PID/时间），再写临时文件 + 原子 rename；
   - 写前记 mtime + 内容 hash；写完校验回读；失败恢复备份；
   - 备份目录：`~/.cc-switch/backups/dsh/<ts>/`，带 `cordis.patch.yml`、`.credentials.yaml`（凭据备份按 0600、只保留 N 份）。
6. **回滚**：provider 行与 credentials 任一失败 → 两份文件都回滚到写前快照，返回明确的 localized 错误。

伪代码：

```rust
pub fn switch_to(provider: &Provider) -> Result<SwitchOutcome, AppError> {
    let _lock = FileLock::acquire(&patch_path)?;
    let snapshot = Snapshot::take(&[&patch_path, &credentials_path])?;
    let outcome = (|| {
        let mut doc = PatchDoc::load(&patch_path)?;          // yaml_edit，保留注释
        match kind_of(provider)? {
            Kind::PiAi(profile) => {
                let mut providers = doc.row_config("llm-pi-ai")
                    .get("providers").cloned().unwrap_or_default(); // 全量保留
                providers.upsert(profile.route.clone(), profile.to_yaml());
                doc.set_row_config("llm-pi-ai", json!({ "providers": providers }))?;
            }
            Kind::DeepSeekOfficial(p) => doc.set_row_config("llm-deepseek", p.to_yaml())?,
        }
        doc.set_row_config("agent-default-model", json!({
            "provider": provider.route(),
            "model": provider.first_model_id(),
            "reasoningEffort": provider.reasoning_effort(),
        }))?;
        doc.commit_atomic(&patch_path)?;
        Credentials::upsert_ref(&credentials_path, provider.key_ref(), provider.api_key())?;
        Ok(())
    })();
    if outcome.is_err() { snapshot.restore()?; }
    outcome
}
```

### 4.4 凭据写入

- 只写 `refs`（明文），与 DSH 设置页写入的形态一致；`records` 区不碰。
- `refs` 的 key 用 `deriveKeyRef(route)` 生成，保证 DSH 设置页能把「已配置」点亮。
- 权限：文件 `0600`、父目录 `0700`；Windows 不设 POSIX 位但要确保在用户目录下。
- 卸载/删除供应商时：删 `refs` 条目 → 删路由 → 若它是当前默认模型，则回退到官方卡或第一条路由。

### 4.5 「当前供应商」与切换语义

- `ProviderService::switch_inner` 对 `AppType::Dsh` 直接进 `dsh_config::switch_to`（不走 `write_live_for_state` 的通用路径，因为要同时改指针）。
- 切换后 `SwitchResult` 里带 `warnings`：
  - `dsh.models_empty`（路由无模型，DSH 模型选择器里不可用）；
  - `dsh.restart_required`（只有一种情况：profile 目录不在 HMR 监听范围内时，提示用户重启 DSH）。
  - 删除当前路由且没有其它路由可指时给 `dsh.default_model_reset_to_default`：删掉用户覆盖层的
    `agent-default-model`，让 DSH 回落到出厂默认，避免留下指向已删路由的悬空指针。
- 删除当前供应商 → 默认模型指针回退（优先官方卡，否则第一条），并写 `warnings`。

### 4.6 导入与同步

- `import_default_config`：DSH 目录存在时，读 patch 反推：
  - `llm-pi-ai.providers.*` → 第三方卡；
  - `llm-deepseek`（且 baseURL 存在）→ 官方/自建 Messages 卡；
  - 当前默认模型指向的路由 → `current`。
- **只导入有实质内容的行**（有 baseURL 或 providers 非空），不生成虚构 `default`（沿用 agy 文档里的既有约定）。
- 全量同步（`sync_all_providers_to_live`）对 DSH：重写所有托管路由 + 指针；未托管路由（用户手写在 patch 里、cc-switch 无对应行）保持不动。
- 托管判定：cc-switch 写入的 route 记在 provider 的 `meta.owned = true`（或 `route` 前缀白名单），避免误删用户手写条目。

### 4.7 校验与连通性检查

保存前：

1. `route` 匹配 `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`，且不与现有路由冲突（除自身）。
2. `baseURL` 是 http(s)、无 query/hash/内嵌凭据（与 `llm-deepseek` 的校验口径一致）。
3. `apiKeyEnv` 匹配 `^[A-Za-z_][A-Za-z0-9_]*$`。
4. `api` ∈ {`anthropic-messages`, `openai-completions`, `openai-responses`}。
5. `models` 非空，`id` 非空且唯一。
6. YAML 可解析、目标行可定位。

连通性检查（不阻塞保存）：

- `anthropic-messages` → `GET {baseURL}/v1/models`（带 `x-api-key`/`anthropic-version`）；
- `openai-*` → `GET {baseURL}/models`（`Authorization: Bearer`）；
- 失败只作为卡片上的状态提示，不写失败。

### 4.8 前端

- `src/lib/api/types.ts`：`AppId` 增加 `"dsh"`。
- `src/config/appConfig.tsx`：`APP_IDS`、`DEFAULT_VISIBLE_APPS`、`SKILLS_APP_IDS`（二期再加）、图标。
- `src/types.ts`：`VisibleApps` 增加 `dsh`。
- 表单：新增 `components/providers/forms/DshProviderForm.tsx`，`ProviderForm.tsx` 里按 `app` 分发；字段 = kind / route / displayName / api / baseURL / apiKey / models 列表（可粘贴 JSON）。
- 预设：`src/config/dshProviderPresets.ts`（官方 DeepSeek、常见 Anthropic 兼容聚合、OpenAI 兼容自建），只预填 `baseURL/api/models`，不预填 key。
- 文案：`src/i18n/locales/*` 增加 `dsh.*` 一组 key（四语：zh/en/ja/zh-TW，与仓库现有语言集一致）。
- 卡片：显示 route、协议、模型数、key 是否已配置（由 cc-switch 侧判断 `refs` 是否存在）。

### 4.9 数据库

- **供应商不需要迁移**：`providers` 按 `(id, app_type)` 存，`app = "dsh"` 即可用。
- **MCP 已接入**：`mcp_servers` 增加 `enabled_dsh BOOLEAN NOT NULL DEFAULT 0`，`SCHEMA_VERSION 21 → 22`；新建库的 DDL 与 `21 =>` 升级迁移都覆盖（`schema.rs`）。Skills 仍未接，`skills` 表不加列。

### 4.10 二期：会话 / MCP / Skills / 用量

> MCP、会话、Skills、用量均已在本次接入完成，落地细节见 §7.4–§7.6；本节只剩运行时 gateway 写入等后续项。

- **会话**：`~/.dsh/sessions/*.jsonl`（`session-persistence-jsonl`，root = `dshHomePath('sessions')`）。新增 `session_manager/providers/dsh.rs`，只读解析 + 去重，恢复命令交给 DSH 自身（不伪造）。
- **MCP**：已完成（`mcp::dsh`）。一台服务器一条 `dsh-mcp-client` 行；stdio / Streamable HTTP；`!!js` 行只读。
- **Skills**：DSH 有 `dsh-skill-filesystem`，目录布局需实测后再接。
- **用量**：DSH 会话里带 token 元数据，可复用 `session_usage` 管道；先做只读聚合，不做计费推断。

### 4.11 测试计划

Rust 单测（`src-tauri/src/dsh_config.rs` 内联 + `tests/`）：

1. 空 patch / 无 `llm-pi-ai` 行 → 新建行，其它行字节级不变（golden 对比）。
2. 已有两条路由 → 新增第三条不丢前两条（整行替换语义的回归）。
3. 用户 patch 含注释与 `!!js` 标签 → 编辑后注释保留、标签原样。
4. 凭据：新建 ref、覆盖 ref、删除 ref、权限 0600 断言。
5. 失败注入：写 patch 成功但写凭据失败 → 两份文件回滚。
6. `agent-default-model` 指针：切到新路由、删除当前路由后的回退。
7. 导入：从 golden patch 反推 3 个 provider + current。
8. 并发：两个写入者（模拟 DSH 的 `.lock`）→ 第二个等待或明确失败，不产生半写文件。

真机联调（手工，记录在 PR 描述里）：

1. 用临时 `DSH_HOME` 指向 fixture 目录，启动 DSH 桌面端（`DSH_HOME=/tmp/dsh-fixture open -a "DeepSeek Harness"`，或应用内切换 profile）→ 设置页 Models 里应看到 cc-switch 写的路由与「已配置」。
2. 写一个本地 mock 的 Anthropic Messages 端点（`127.0.0.1`），发一条消息验证真能跑通。
3. 在 DSH 设置页删掉路由 → cc-switch 重新同步不会「复活」已删条目（除非用户显式开启同步）。

命令（与仓库现有 CI 口径一致）：

```sh
pnpm typecheck
pnpm test:unit
cargo test --manifest-path src-tauri/Cargo.toml --lib dsh -- --test-threads=1
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
pnpm build:renderer
```

### 4.12 风险与边界

| 风险 | 影响 | 处理 |
| --- | --- | --- |
| YAML 保真 | 用户 patch 被重写后丢注释/坏标签 | 用保注释编辑器；只改目标子树；解析失败即拒绝写入 |
| 整行 config 替换 | 抹掉用户手写路由 | 写入前读全量 `providers` 并合并；单测覆盖 |
| 双写者 | DSH 运行中用户同时改设置页 → 覆盖 | 写前 mtime/hash 校验 + `.lock`；冲突返回明确错误；二期走 gateway |
| 协议猜错 | 请求 404/400 | 表单默认 `anthropic-messages`（cc-switch 主力场景），连通性检查给提示，不自动改写 |
| 模型元数据不准 | 上下文窗口/最大输出与实际不符 | 允许手改，缺省保守值，卡片标注「估算」 |
| 版本漂移 | DSH 改 schema/行 id | 行 id 与 schema 从源码常量取，写死会漂；加版本探测（`dsh/package.json` 的 version），不匹配时降级为只读 |
| `desktop` profile 独占 | 不能 CLI 重启 | 只改文件；提示用户在 DSH 内重开或等 HMR |
| Windows | 路径/权限差异 | `%USERPROFILE%\.dsh`；不写 POSIX 位；换行统一 LF |
| 官方卡多实例 | DSH 路由名固定 `deepseek-official` | cc-switch 侧限制官方卡唯一 |

### 4.13 里程碑

| 阶段 | 内容 | 预估 |
| --- | --- | --- |
| M1 | `AppType::Dsh` + `dsh_config` 只读（路径、解析、导入）+ 单测 | 0.5–1 人日 |
| M2 | 写入器（patch 三行 + credentials + 回滚 + 锁）+ `switch_inner` 接线 | 1–1.5 人日 |
| M3 | 前端表单/预设/i18n/卡片状态 + 与 DSH 真机联调 | 1–1.5 人日 |
| M4 | MCP / Skills / 会话（各自独立 PR） | 2–3 人日 |
| M5 | gateway 写入路径（消除双写者冲突） | 1–2 人日 |

---

## 5. 可复查的关键位置

**cc-switch**

- `src/config/appConfig.tsx:19/33/48/66`
- `src/lib/api/types.ts:2`、`src/types.ts:307`
- `src-tauri/src/app_config.rs:418/437/459/493/515`
- `src-tauri/src/commands/provider.rs:148/166`
- `src-tauri/src/services/provider/mod.rs:6560/6588/6665`
- `src-tauri/src/services/provider/live.rs:388`
- `src-tauri/src/provider.rs:150`
- `src-tauri/src/tray.rs:41/51`、`prompt_files.rs:21`
- `src-tauri/src/database/mod.rs:54`、`database/schema.rs:71/103`
- `docs/agy-integration-zh.md`（新增应用的文档范式）

**DeepSeek Harness**（`/Applications/DeepSeek Harness.app/Contents/Resources/app.asar` → `dsh/`）

- `dsh/package.json`（版本 `0.2.1-rc.2`）、`dsh/lib/bin.js:35`（desktop profile 独占）
- `@deepseek-ai/dsh-home-paths/lib/index.js`（`~/.dsh`、`DSH_HOME`）
- `@deepseek-ai/dsh-base/cordis.patch.yml:90`（`agent-default-model`）、`:135`（`llm-pi-ai`）、`:525`（`llm-deepseek`）
- `@deepseek-ai/dsh-llm-deepseek/lib/index.js:318/347/430`
- `@deepseek-ai/dsh-llm-deepseek-api-key/lib/index.js`（route `deepseek-official`、`DEEPSEEK_API_KEY`）
- `@deepseek-ai/dsh-llm-pi-ai/lib/index.js:834/1049/1083`
- `@deepseek-ai/dsh-client-ui-settings-models/lib/client.js:1230/1535/1607/1616`
- `@deepseek-ai/dsh-settings/lib/index.js:488/502/508`
- `@deepseek-ai/dsh-config-editor/lib/index.js:24`（`documentPath = patchPath`）
- `@deepseek-ai/dsh-credentials-local/lib/index.js:49/195/608`
- `@deepseek-ai/dsh-atomic-write/lib/index.js`（`<file>.lock` + 原子 rename）

**本机现状**

- `~/.dsh/profiles/desktop/cordis.patch.yml`（42 行、5 个行）
- `~/.dsh/profiles/desktop/package.json`（`dsh.profile.bundles`）
- `~/.dsh/.credentials.yaml`（`version: 1` + `refs` + `records`）

---

## 6. 附录：最小可验证样例（手工写入临时 DSH_HOME）

> 用于在隔离目录里验证格式，不碰真实 `~/.dsh`。DSH 支持 `DSH_HOME` 覆盖根目录。

`$DSH_HOME/profiles/desktop/cordis.patch.yml`（在已有行之外追加/替换这三行）：

```yaml
- id: llm-pi-ai
  name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
      local-mock:
        displayName: Local Mock
        api: anthropic-messages
        baseURL: http://127.0.0.1:8787
        apiKeyEnv: LOCAL_MOCK_API_KEY
        models:
          - id: mock-sonnet
            name: Mock Sonnet
            contextWindow: 200000
            maxTokens: 8192
            input: [text]

- id: agent-default-model
  name: '@deepseek-ai/dsh-agent-default-model'
  config:
    provider: local-mock
    model: mock-sonnet
    reasoningEffort: low
```

`$DSH_HOME/.credentials.yaml`（`chmod 600`）：

```yaml
version: 1
refs:
  LOCAL_MOCK_API_KEY: sk-local-test
```

验证点：

1. DSH 设置页 Models 里出现 `Local Mock` 路由与模型，key 状态为「已配置」。
2. 新建会话发一条消息，mock 端点收到 `POST /v1/messages`，`x-api-key: sk-local-test`。
3. 把 `refs` 里的 key 删掉再发请求 → 报 `MISSING_CREDENTIAL`，说明凭据确实走 `refs`。
4. 删掉 `providers.local-mock` 但保留 `agent-default-model` → DSH 侧应报路由不存在，cc-switch 的写入器要避免产生这种半状态。

上述 4 条建议直接固化成 `tests/fixtures/dsh/` 的 golden 用例，对应 §4.11 的第 1、2、4、6 项。

---

## 7. 实现状态（本次落地）

M1–M4 已按本方案实现（供应商、MCP、会话、Skills），只剩 M5 的运行时 gateway 写入（见 §4.12）。

### 7.1 已实现

| 能力 | 落地位置 |
| --- | --- |
| 应用注册（`dsh`） | `app_config.rs` 的 `AppType` / `as_str` / `FromStr` / `all` / `is_additive_mode`；`src/lib/api/types.ts`、`src/config/appConfig.tsx`、`src/types.ts` |
| profile patch 行读写 | `src-tauri/src/dsh_config.rs`：按行 id 定位、整行 `config` 替换、只解析目标行（其它行的注释与 `!!js` 原样保留） |
| 第三方路由 | `llm-pi-ai.providers.<route>`：`displayName` / `api` / `baseURL` / `apiKeyEnv` / `models` |
| 官方路由 | `llm-deepseek`：`baseURL` / `models` / `apiKeyEnv`（默认 `DEEPSEEK_API_KEY`） |
| 凭据 | `$DSH_HOME/.credentials.yaml` 的 `refs.<ROUTE>_API_KEY`（0600，保留 `records` 等其它区段） |
| 默认模型指针 | `agent-default-model`：切换时写 `{provider, model, reasoningEffort}`；编辑同一条路由时若指针模型被删会自动修到第一个仍在的模型 |
| 切换 | `ProviderService::switch_normal` 里对 `dsh` 同时写路由行与指针；`SwitchResult.warnings` 透传回退告警 |
| 删除供应商 | `remove_from_live_config` → `dsh_config::remove_provider_route`：删路由、必要时重指默认模型、无引用时删 `refs` |
| 导入 | 启动时 `import_dsh_providers_from_live` 幂等同步；供应商页「导入」按钮走 `import_dsh_providers_from_live` 命令 |
| 提示词 | `$DSH_HOME/AGENTS.md`（`prompt_files.rs`） |
| 校验 | 后端 `route_from_provider` 在入库前校验 route / baseURL / 协议 / 模型 / 凭据引用；前端表单同规则即时提示 |
| 回滚 | 写 patch 或凭据任一步失败，两份文件都恢复到写前快照 |
| 清理 | 删掉最后一条托管路由后，只剩空 `providers` 的自建行与用户覆盖层的默认指针一并移除，profile 回到 bundle 默认状态 |
| 前端表单 | `DshProviderForm.tsx`：路由类型、路由名、协议、端点、Key、模型列表、思考强度；预设 `dshProviderPresets.ts`；五语文案 |
| 设置可见性 | Rust `VisibleApps.dsh` + 前端 `VisibleApps` / `APP_ICON_MAP` / `AppGlyph` / 端点测速超时 / 用量短名 |
| MCP | `src-tauri/src/mcp/dsh.rs` + `dsh_config` 的行读写：一台服务器一条 `dsh-mcp-client` 行；stdio / Streamable HTTP；开关、删除、导入；`mcp_servers.enabled_dsh`；前端 `MCP_APP_IDS` 与导入来源展示 |
| 会话 | `session_manager/providers/dsh.rs`：列表读 `storages/session_projcache`（标题、目录、用量、最后活动），正文流式解压 `sessions/**/session.v4.jsonl.zstd`，还原 user / assistant / tool 消息；删除交给 DSH（后端拒绝、前端隐藏） |
| Skills | `skills.enabled_dsh` + `SkillService` 路径 `$DSH_HOME/skills`（目录 bundle `<name>/SKILL.md` 或平铺 `<name>.md`）；前端 `SKILLS_APP_IDS` 已包含 DSH |
| 用量 | `services/session_usage_dsh.rs`：从每个 `assistant/message` 的 `usage` 导入 token 与费用，按 `会话 + turn + step` 去重；输入按 fresh input 记，输出含思考 token |
| Deep link | `ccswitch://…resource=provider&app=dsh`：从链接的 name/endpoint/apiKey/model/apiFormat 生成第三方路由行；缺模型或缺端点直接拒绝 |

### 7.2 明确的未实现

- 本地代理 / 故障转移 / 聚合：`get_adapter` 对 `dsh` 返回 `None`，`supports_local_proxy` 不含它。
- 运行时 gateway 写入（§4.12 的二期）：仍走文件 + HMR。

### 7.3 验证

```sh
cargo check --manifest-path src-tauri/Cargo.toml --lib      # 0 error / 0 warning
cargo test --manifest-path src-tauri/Cargo.toml --lib dsh_config -- --test-threads=1
# 18 passed：行定位/保真、合并、凭据 0600、指针回退与清理、空 Key 不误删、导入回读、非法配置拒绝、坏行不覆盖、MCP 行读写与清理
cargo test --manifest-path src-tauri/Cargo.toml --lib mcp::dsh -- --test-threads=1
# 4 passed：stdio / HTTP 投影、url-only 归一、SSE 拒绝、反向导入
cargo test --manifest-path src-tauri/Cargo.toml --lib session_manager::providers::dsh -- --test-threads=1
# 2 passed：压缩事件日志还原、投影缓存扫描与空白会话过滤
cargo test --manifest-path src-tauri/Cargo.toml --lib session_usage_dsh -- --test-threads=1
# 2 passed：fresh input 语义、turn+step 去重与保留期墓碑
cargo test --manifest-path src-tauri/Cargo.toml --lib deeplink::tests -- --test-threads=1
# 80 passed：含 DSH 深链接的协议/端点/模型校验

# 可选：拿一份真实 DSH_HOME 的副本做冒烟（不碰真实账号）
cp -R ~/.dsh /tmp/dsh-smoke
CCS_DSH_SMOKE_HOME=/tmp/dsh-smoke \
  cargo test --manifest-path src-tauri/Cargo.toml --lib dsh_smoke -- --ignored --test-threads=1
# 副本里带 sessions/ 与 storages/ 时，同时验证会话解析与用量导入

# 受影响的集成测试（MCP 开关 / 重同步 / 导入导出）：52 passed
cargo test --manifest-path src-tauri/Cargo.toml \
  --test mcp_commands --test provider_commands --test provider_service --test import_export_sync

cargo test --manifest-path src-tauri/Cargo.toml --lib -- --test-threads=1
# 3961 passed / 0 failed / 12 ignored（供应商、MCP、会话、Skills、用量、深链接、迁移与全部既有用例）
pnpm typecheck                                              # 通过
pnpm build:renderer                                         # 通过
npx vitest run                                              # 3162 passed；仅剩基线已有的 3 个失败（UsageDashboard x2、rememberedChoice）
```

手工验收（建议在隔离的 `DSH_HOME` 里做，别碰真实账号）：

1. `DSH_HOME=/tmp/dsh-fixture` 启动 DSH，先让它在设置页写出一份 profile。
2. CC Switch 里给 DSH 添加一个第三方供应商（本地 mock 的 Anthropic Messages 端点），点「启用」。
3. 打开 DSH 设置页 Models：应看到该路由、模型，Key 状态为「已配置」；新建会话发一条消息，mock 端点应收到 `POST /v1/messages` 且带 `x-api-key`。
4. 回到 CC Switch 删除该供应商：DSH 里路由消失，默认模型指针回退到剩余路由（或保留并给出 `dsh.default_model_unassigned` 警告）。

### 7.4 MCP 适配细节

| 项 | 行为 |
| --- | --- |
| 配置面 | 一台服务器一条 profile 行：`id: mcp-ccs-<服务器 id>`、`name: '@deepseek-ai/dsh-mcp-client'`、`config.{transport,serverName,…}` |
| 传输 | `stdio` → `transport: stdio` + `command/args/env/cwd`；`http` → `transport: streamable-http` + `url/headers`；**SSE 直接拒绝**（DSH 只支持前两种） |
| serverName | 优先沿用同 id 行的值；新行按服务器 id 生成，遵守 `[A-Za-z0-9_-]{1,32}`；已存在同名行时**复用它**而不是新建（避免 DSH 的 serverName 冲突） |
| 启用 / 更新 | 写入连接字段、保留行里其它字段（`toolCallTimeoutMs`、`reconnect` 等），并去掉 `disabled` |
| 取消勾选 | 先写 profile（只写 `disabled: true`，配置原样保留），成功后再翻数据库；重新勾选时恢复 |
| 删除 | 启用过的删整行；只取消过勾选的，按 `mcp-ccs-*` 行 id 清理自己建的行 |
| `!!js` 行 | 只读、可禁用/删除；拒绝改写配置（避免丢表达式），返回明确错误 |
| 导入 | 以 `serverName` 作为供应商 id；`disabled` 行导入为未启用；含 `!!js` 的行跳过并记日志 |
| 数据库 | `SCHEMA_VERSION 22` 引入 `mcp_servers.enabled_dsh`，v23 再加 `skills.enabled_dsh`；MCP 页面开关与「从应用导入」自动包含 DSH |

### 7.5 会话适配细节

| 项 | 行为 |
| --- | --- |
| 列表 | 只读 `$DSH_HOME/storages/session_projcache/sessions/session-<id>.json`：标题、`identity.cwd`、创建时间、`lastPromptAt`、`blank` 标记；空白会话不展示 |
| 正文 | 在 `$DSH_HOME/sessions/<编码工作区>/session-<id>/` 下找最高版本 `session.v*.jsonl.zstd`，用 zstd 流式解压逐行还原：`user/message`、`assistant/message`（text / reasoning / tool-call）、`tool/result`；事件 `time` 作为时间戳，`turn/start` 与 `data.turn` 提供轮次 |
| 阅读页 | 大工具输出只给预览（压缩日志没有可直接引用的行偏移，暂不提供 `full` 引用）；思考内容按通用预览截断 |
| 删除 | 后端拒绝、前端隐藏：日志、投影缓存与会话锁要由 DSH 一起维护 |
| 恢复命令 | 不提供：桌面 profile 由 Electron 独占，硬编码 CLI 恢复命令会误导 |
| 入口 | 前端 `SESSION_APP_IDS` 含 `dsh`，来源说明写 `~/.dsh/sessions` 与投影缓存路径 |

### 7.6 Skills 适配细节

| 项 | 行为 |
| --- | --- |
| 目录 | `$DSH_HOME/skills`（DSH 只扫一层：`<name>/SKILL.md` 目录 bundle 或 `<name>.md` 平铺文件） |
| 启用开关 | `skills.enabled_dsh`（`SCHEMA_VERSION 23`）；前端 `SKILLS_APP_IDS` 已包含 DSH |
| 同步 | 复用 CC Switch 既有的 SSOT → 各应用目录的安装/链接机制，没有 DSH 专属格式转换 |
| 边界 | 项目级 `.dsh/skills` 不归 CC Switch 管；DSH 的 frontmatter 要求 `name` 为 kebab-case，沿用现有 skill 校验 |

### 7.7 已知边界

- 同一 route 名如果用户之前手写过，cc-switch 会整段替换该 route 的 profile（DSH 的 patch 语义就是整行替换），但不会动同行的其它 route。
- 官方卡在 DSH 里只有一个路由名 `deepseek-official`；cc-switch 不阻止创建第二张官方卡，后写的会覆盖先写的配置（UI 预设只有一张）。
- `.credentials.yaml` 被 cc-switch 重写时按 YAML 数据往返，`records` 的注释会丢；键值与结构不变。
- 保存供应商时 Key 留空表示「这次不改」，不会删除已存的 ref；删除供应商且没有其它路由引用同一
  ref 时才清理。
- 与 DSH 同时写配置时的冲突靠「写前快照 + 原子替换」兜底；跨进程锁（`.lock`）是 M5 的网关路径要补的。
- 用量只统计带 `usage` 的助手消息；模型不在定价表里时费用记 0（不猜价），历史消息不会因为换供应商而改归属。
