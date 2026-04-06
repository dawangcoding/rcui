# AGENTS.md — RCUI 项目开发与系统指南

## 项目概述

RCUI 是 CloudCLI UI (Node.js/Express) 的 Rust 后端重写版本。后端使用 Axum 0.8 + SQLx + SQLite，前端保持原有 TypeScript/React 不变。项目为 Claude Code、Cursor、Codex、Gemini 四个 AI 编程助手提供统一的 Web UI 管理界面。

原始项目位于 `~/Code/claudecodeui`，本项目从中提取前端并重写后端。

## 构建与运行

```bash
# 检查编译
cargo check

# 开发构建
cargo build

# 发布构建（推荐）
cargo build --release

# 运行 server
JWT_SECRET=your-secret SERVER_PORT=3001 cargo run -p rcui-server

# 运行 CLI
./target/release/rcui start --port 3001

# 前端开发模式（需要另开终端）
cd frontend && npm install && npm run dev

# 完整构建（前端 + 后端）
bash build.sh

# 生产模式（带静态文件服务）
STATIC_DIR=frontend/dist ./target/release/rcui-server
```

## 代码质量命令

```bash
# 类型/编译检查 — 修改代码后必须运行
cargo check

# 格式化
cargo fmt

# Lint
cargo clippy

# 修复未使用 import（适合批量清理）
cargo fix --allow-dirty --allow-no-vcs
```

## Workspace 结构

```
~/Code/rcui/
├── Cargo.toml                    # Workspace 根，members: rcui-server, rcui-cli
├── crates/
│   ├── rcui-server/              # 主服务端 (bin: rcui-server)
│   │   └── src/
│   │       ├── main.rs           # 入口：路由注册、中间件、静态文件服务
│   │       ├── config.rs         # AppConfig — 从环境变量加载配置
│   │       ├── state.rs          # AppState — Arc 共享状态（db, broadcast, sessions）
│   │       ├── error.rs          # AppError — 统一错误类型，实现 IntoResponse
│   │       ├── auth/             # 认证模块
│   │       │   ├── jwt.rs        # JWT 签发/验证/自动刷新
│   │       │   ├── middleware.rs # AuthUser / ApiKeyAuth 提取器
│   │       │   └── password.rs   # bcrypt 密码哈希
│   │       ├── db/               # 数据库层（SQLx + SQLite）
│   │       │   ├── mod.rs        # init_pool, run_migrations
│   │       │   ├── users.rs      # 用户 CRUD
│   │       │   ├── api_keys.rs   # API Key 管理
│   │       │   ├── credentials.rs
│   │       │   ├── session_names.rs
│   │       │   ├── app_config.rs # KV 存储（JWT secret 自动生成）
│   │       │   ├── notifications.rs
│   │       │   └── push_subscriptions.rs
│   │       ├── providers/        # 四大 AI 助手适配器
│   │       │   ├── mod.rs        # ProviderAdapter trait + ProviderRegistry
│   │       │   ├── types.rs      # SessionProvider, NormalizedMessage, MessageKind
│   │       │   ├── utils.rs      # 通用工具函数
│   │       │   ├── claude/       # Claude 适配器（读 ~/.claude/projects/）
│   │       │   ├── cursor/       # Cursor 适配器（读 ~/.cursor/chats/ SQLite）
│   │       │   ├── codex/        # Codex 适配器（读 ~/.codex/sessions/）
│   │       │   └── gemini/       # Gemini 适配器（读 ~/.gemini/tmp/）
│   │       ├── routes/           # HTTP 路由 handler
│   │       │   ├── auth.rs       # 注册/登录/用户信息
│   │       │   ├── projects.rs   # 项目列表/添加/重命名/删除
│   │       │   ├── sessions.rs   # Session 列表/消息/删除/命名
│   │       │   ├── settings.rs   # API Key/凭证/通知偏好/推送
│   │       │   ├── user.rs       # Git 配置/Onboarding
│   │       │   ├── git.rs        # 20 个 Git 操作端点
│   │       │   ├── mcp.rs        # MCP Server 管理（Claude + Cursor）
│   │       │   ├── commands.rs   # 命令列表/加载/执行
│   │       │   ├── ws.rs         # WebSocket 升级与消息处理
│   │       │   └── health.rs     # 健康检查
│   │       └── services/         # 后台服务
│   │           ├── chat.rs       # ChatCommand 分发 + CLI 进程 spawn
│   │           ├── file_watcher.rs  # 文件变更监听 + 广播
│   │           └── project_scanner.rs # 项目目录扫描 + 缓存
│   └── rcui-cli/                 # CLI 工具 (bin: rcui)
│       └── src/main.rs           # clap: start/open/status/stop
├── migrations/
│   └── 001_initial.sql           # 数据库 schema（embedded via include_str!）
├── frontend/                     # TypeScript/React 前端（从原项目复制）
│   ├── src/                      # React 源码
│   ├── public/                   # 静态资源
│   ├── package.json              # 前端依赖
│   ├── vite.config.js            # Vite 配置（代理到 Rust 后端）
│   ├── tailwind.config.js
│   └── tsconfig.json
├── shared/                       # 前后端共享类型定义
└── build.sh                      # 一键构建脚本
```

## 核心技术栈与版本

| 组件 | 版本 | 说明 |
|------|------|------|
| Axum | 0.8 | 路由参数使用 `{param}` 语法（非 `:param`） |
| SQLx | 0.8 | 异步 SQLite，WAL 模式 |
| tokio | 1 | full features |
| tower-http | 0.6 | CORS, Trace, ServeDir/ServeFile, gzip |
| jsonwebtoken | 9 | HS256, 7 天过期, 50% 自动刷新 |
| bcrypt | 0.17 | 密码哈希 |
| notify | 7 + notify-debouncer-mini 0.5 | 文件变更监听，300ms 防抖 |
| clap | 4 | CLI derive 模式 |
| serde/serde_json | 1 | JSON 序列化 |
| md-5 | 0.10 | RustCrypto MD5（Cursor 项目路径哈希） |
| dashmap | 6 | 并发安全 HashMap（活跃 session 管理） |

## 关键架构模式

### 1. 共享状态：`Arc<AppState>`

所有 handler 通过 `State(state): State<Arc<AppState>>` 提取共享状态。AppState 包含：

- `db: SqlitePool` — 数据库连接池
- `jwt_secret: String` — JWT 签名密钥
- `config: AppConfig` — 应用配置
- `broadcast_tx: broadcast::Sender<BroadcastMessage>` — WebSocket 广播
- `active_sessions: DashMap<String, Arc<Mutex<ActiveSession>>>` — 活跃 CLI 进程
- `project_cache: Arc<RwLock<Option<Vec<Value>>>>` — 项目列表缓存

### 2. 认证提取器

受保护路由使用 `AuthUser` 提取器（实现 `FromRequestParts`）：

```rust
pub async fn list_projects(
    _auth: AuthUser,                     // 自动验证 JWT
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> { ... }
```

- `AuthUser` 从 `Authorization: Bearer <token>` 或 `?token=` 查询参数提取
- Platform 模式 (`VITE_IS_PLATFORM=true`) 自动使用第一个数据库用户
- Token 过期时间 7 天，过半自动刷新
- `verify_token(secret, token)` — 参数顺序: 密钥在前，token 在后
- 返回 `TokenData<Claims>`，通过 `.claims.user_id` 访问

### 3. 错误处理

`AppError` 实现 `IntoResponse`，所有 handler 返回 `Result<T, AppError>`：

- `Unauthorized(String)` → 401
- `BadRequest(String)` → 400
- `NotFound(String)` → 404
- `Conflict(String)` → 409
- `Database(sqlx::Error)` → 500（日志记录，不暴露详情）
- `Io / Internal` → 500

### 4. Provider 适配器

四个 provider 通过 `ProviderAdapter` trait 统一：

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn fetch_history(&self, session_id: &str, opts: FetchHistoryOptions) -> Result<FetchHistoryResult, AppError>;
    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage>;
}
```

Session 数据来源：
- **Claude**: `~/.claude/projects/{encoded_path}/` 目录下的 JSONL 文件
- **Cursor**: `~/.cursor/chats/` 下的 SQLite 数据库 (`store.db`)，项目路径经 MD5 哈希
- **Codex**: `~/.codex/sessions/` 下的 JSON 文件
- **Gemini**: `~/.gemini/tmp/` 下的会话文件

路由 handler 中通过 `match` 直接分发到对应 provider，不使用 ProviderRegistry。

### 5. WebSocket + Chat

- `/ws?token=<jwt>` — WebSocket 升级端点
- 客户端发送 `ChatCommand`（serde tagged enum, `#[serde(tag = "type", rename_all = "kebab-case")]`）
- 服务端通过 `execute_command()` 分发到 `spawn_claude/cursor/codex/gemini()`
- 各 spawn 函数启动对应 CLI 进程（`claude`, `cursor-agent`, `codex`, `gemini`），解析流式 JSON 输出
- 使用 `mpsc::unbounded_channel` 将 `ChatResponse` 转发到 WebSocket
- `futures_util::{SinkExt, StreamExt}` 用于 WebSocket 读写分离

### 6. 文件监听

`services::file_watcher` 使用 `notify-debouncer-mini`（300ms 防抖）监听四个 provider 目录：
- 变更时清除 `project_cache` 并通过 `broadcast_tx` 广播 `ProjectsUpdated`
- WebSocket 客户端收到广播后刷新项目列表

### 7. 静态文件服务

通过 `STATIC_DIR` 环境变量启用，使用 `tower_http::services::{ServeDir, ServeFile}` 实现 SPA fallback：

```rust
app.fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index_file)))
```

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SERVER_PORT` / `PORT` | `3001` | 服务端口 |
| `HOST` | `0.0.0.0` | 绑定地址 |
| `JWT_SECRET` | 自动生成并存入 DB | JWT 签名密钥 |
| `DATABASE_PATH` | `~/.rcui/auth.db` | SQLite 数据库路径 |
| `STATIC_DIR` | (无) | 前端 dist 目录路径 |
| `WORKSPACES_ROOT` | `$HOME` | 工作区根目录 |
| `CONTEXT_WINDOW` | `160000` | 上下文窗口大小 |
| `VITE_IS_PLATFORM` | `false` | Platform 模式（跳过 JWT 认证） |
| `API_KEY` | (无) | 全局 API Key |
| `RUST_LOG` | `info` | 日志级别（tracing EnvFilter） |

## 数据库

- SQLite + WAL 模式
- Schema 通过 `include_str!("../../../../migrations/001_initial.sql")` 嵌入二进制
- 启动时自动创建表（`CREATE TABLE IF NOT EXISTS`）
- 主要表: `users`, `app_config`, `session_names`, `api_keys`, `user_credentials`, `push_subscriptions`, `vapid_keys`, `user_notification_preferences`
- 单用户模式：只允许注册一个用户

## API 路由总览

### 公开路由
- `GET /health` — 健康检查
- `GET /ws` — WebSocket（通过 query param 认证）
- `GET /api/auth/status` — 认证状态
- `POST /api/auth/register` — 注册（限一个用户）
- `POST /api/auth/login` — 登录

### 受保护路由（需要 JWT）
- `GET/POST /api/auth/user, /api/auth/logout`
- `GET/POST/PUT/DELETE /api/projects/**`
- `GET/DELETE/POST /api/sessions/**`
- `GET/POST/PATCH/DELETE /api/settings/**`
- `GET/POST /api/user/**`
- `GET/POST /api/git/**` — 20 个 Git 操作
- `GET/POST/DELETE /api/mcp/**` — Claude + Cursor MCP 管理
- `GET/DELETE /api/cursor/mcp/**` — Cursor 专用 MCP
- `GET /api/mcp-utils/all-servers`
- `POST /api/commands/**` — 命令管理

## 开发注意事项

### Axum 0.8 路由语法
路由参数必须使用 `{param}` 而非 `:param`：
```rust
.route("/api/projects/{projectName}", delete(handler))
```

### 添加新路由的步骤
1. 在 `routes/` 下对应文件添加 handler 函数
2. 在 `main.rs` 的 Router 链中注册路由
3. 需要认证的路由，handler 参数加 `_auth: AuthUser`

### 添加新数据库表
1. 在 `migrations/001_initial.sql` 中添加 `CREATE TABLE IF NOT EXISTS`
2. 在 `db/` 下创建对应模块文件
3. 在 `db/mod.rs` 中 `pub mod` 导出

### Git 路由安全
`routes/git.rs` 实现了完整的安全防护：
- 路径遍历检测（`..`, 空字节）
- Shell 元字符拦截
- Git ref 名称验证
- diff 输出超 500KB 截断

### 已知警告
当前有 27 个 `dead_code` 警告，均为已声明但尚未被路由 handler 直接调用的字段/函数。这些是预留给前端完整集成时使用的，不影响功能。

### Linter/Formatter 注意
本项目存在外部 linter hook，可能在文件保存时自动格式化或重写内容。如果编辑被意外覆盖：
- 使用 `cargo fmt` 的代码风格
- 避免手动格式化，让工具自动处理
- 需要绕过时，可用 Python 脚本或 bash 直接操作文件

## 参考文档

### Claude Agent SDK 架构参考
实现或修改 Claude 相关功能（`providers/claude/`、WebSocket chat、session 管理等）时，**必须参考**：

- **文档路径**: `docs/claude-agent-sdk-architecture.md`
- **基于版本**: `@anthropic-ai/claude-agent-sdk` v0.2.92
- **核心内容**:
  - SDK 系统架构（进程 spawn + IPC 通信模式）
  - SDKMessage 消息协议（20+ 种消息类型的完整定义）
  - JSONL 会话文件格式（`~/.claude/projects/` 下的存储结构）
  - V1 `query()` / V2 `SDKSession` API 接口
  - canUseTool 权限回调机制
  - Hook 系统（27 种事件）
  - MCP 集成（4 种服务器类型）
  - Bridge 远程会话（CCR 连接）

**适用场景**:
- 解析 Claude 会话 JSONL 文件时，参考 SDKMessage 类型体系
- 实现 WebSocket chat 命令分发时，参考 query 数据流和控制请求通道
- 添加新的 session 管理功能时，参考会话持久化 API（listSessions, getSessionMessages 等）
- 对接 Claude Agent SDK 编程接口时，参考 V1/V2 API 和 Transport 抽象

### Claude CLI 对接规范（必读）

RCUI 的 `spawn_claude()` 直接 spawn `claude` CLI 子进程并通过 stdin/stdout JSON 行进行双向通信，本质上是手动复刻了 `@anthropic-ai/claude-agent-sdk` 内部 `query()` 的行为。**任何涉及 Claude CLI 对接的开发，都必须以 SDK 源码为唯一参考标准**，确保 RCUI 的行为与 SDK 完全一致。

- **SDK 源码位置**: `~/Code/claudecodeui/node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs`
- **原版上层调用**: `~/Code/claudecodeui/server/claude-sdk.js` 中的 `queryClaudeSDK()` 函数

#### 核心原则

> SDK 内部就是 spawn CLI 子进程 + stdin/stdout JSON 行通信。RCUI 做的是同样的事情。
> 既然本质相同，就不应该有任何行为差异。所有实现细节必须逐项对标 SDK 源码。

**开发流程要求**：
1. 下方检查清单覆盖的是已知关键项，开发时先按清单逐项确认。
2. **遇到任何不确定的行为、格式、时序问题，必须回溯 SDK 源码确认**，不要凭猜测实现。
3. SDK 源码是混淆过的单文件 JS，关键类/函数定位方式：
   - `cX` — ProcessTransport（CLI 进程 spawn、stdin/stdout 读写、close/endInput）
   - `pX` — Query 核心（readMessages、handleControlRequest、processControlRequest、streamInput）
   - `gz` — SDKSession（send、stream、close，多轮会话管理）
   - `KL` — query() 内部写用户消息到 stdin 的函数
   - `q$` — JSON.stringify 包装
   - `s$` — SDK 内部 debug 日志
   - 搜索 `endInput`、`can_use_tool`、`control_response`、`isSingleUserTurn` 等关键词可快速定位
4. 如果 SDK 升级了版本，以新版源码为准，同步更新本规范和 RCUI 实现。

#### 1. CLI 启动参数

SDK 内部 `cX.initialize()` 构建的参数列表（RCUI 使用 `claude` 命令需额外加 `--print`）：

```
claude --print \                          # RCUI 必须加，SDK 用 node cli.js 不需要
       --output-format stream-json \      # 必须，stdout 输出 JSON 行
       --input-format stream-json \       # 必须，stdin 接收 JSON 行
       --permission-prompt-tool stdio \   # 有 canUseTool 回调时必须加
       --verbose \                        # SDK 默认加
       [--resume <sessionId>] \           # 恢复会话
       [--model <model>] \               # 指定模型
       [--allowedTools <csv>] \           # 预授权工具列表
       [--disallowedTools <csv>] \        # 禁用工具列表
       [--dangerously-skip-permissions] \ # bypassPermissions 模式
       [--permission-mode <mode>]         # 权限模式
```

#### 2. 环境变量

| 变量 | 值 | 来源 | 说明 |
|------|-----|------|------|
| `CLAUDE_CODE_ENTRYPOINT` | `sdk-ts` | SDK 内部设置 | 标识 SDK 模式，启用双向控制协议 |
| `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS` | `1` | 原版 claudecodeui | 禁用实验性功能 |
| `CLAUDE_CODE_STREAM_CLOSE_TIMEOUT` | `300000` | 原版 claudecodeui | SDK 默认 5 秒太短，原版覆盖为 5 分钟。工具执行（ping、npm install、编译）可能耗时较长，必须设置 |

#### 3. stdin 消息协议

所有消息为单行 JSON + `\n` 换行。启动后按顺序发送：

**消息 1 — 初始化请求（SDK `initialize()` 方法）：**
```json
{
  "type": "control_request",
  "request_id": "<uuid>",
  "request": {
    "subtype": "initialize",
    "hooks": {},
    "sdkMcpServers": [],
    "jsonSchema": null,
    "systemPrompt": null,
    "appendSystemPrompt": null,
    "agents": {},
    "promptSuggestions": false,
    "agentProgressSummaries": false
  }
}
```

**消息 2 — 用户消息（SDK `KL()` 函数 / `SDKSession.send()` 方法）：**
```json
{
  "type": "user",
  "session_id": "",
  "message": {
    "role": "user",
    "content": [{"type": "text", "text": "<用户输入>"}]
  },
  "parent_tool_use_id": null
}
```

注意：
- `session_id` 必须包含，值为空字符串 `""`
- 不包含 `isSynthetic` 字段（SDK 不发此字段）
- 图片作为 `{"type": "image", "source": {...}}` content block 追加

#### 4. stdout 事件流

CLI stdout 输出单行 JSON 事件，主要类型：

| type | 含义 | 处理方式 |
|------|------|----------|
| `system` (subtype: `init`) | CLI 初始化完成，包含 `session_id` | 捕获 session_id，发送 session_created |
| `stream_event` | 包裹的流式 token 事件 | 解析内部 content_block_start/delta/stop |
| `assistant` | 完整助手消息（含 content 数组） | 提取 text / tool_use 内容块 |
| `control_request` (subtype: `can_use_tool`) | 权限请求 | 发送 permission_request 到前端，等待用户决定 |
| `control_response` | 对我们发送的 control_request 的响应 | init 的 ack 等 |
| `result` | **回合结束** | 关闭 stdin，等待 CLI 退出 |
| `keep_alive` | 心跳 | 忽略 |

#### 5. 权限请求/响应协议（can_use_tool）

**CLI → RCUI（stdout）：**
```json
{
  "type": "control_request",
  "request_id": "<uuid>",
  "request": {
    "subtype": "can_use_tool",
    "tool_name": "Bash",
    "input": {"command": "ping -c 4 example.com"},
    "tool_use_id": "<tool_use_id>",
    ...
  }
}
```

**RCUI → CLI（stdin），SDK `handleControlRequest()` 方法：**
```json
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "<匹配的 request_id>",
    "response": {
      "behavior": "allow",
      "toolUseID": "<tool_use_id>",
      "updatedInput": { ... }
    }
  }
}
```

或拒绝：
```json
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "<匹配的 request_id>",
    "response": {
      "behavior": "deny",
      "toolUseID": "<tool_use_id>",
      "message": "User denied tool use"
    }
  }
}
```

关键实现点：
- `request_id` 必须从请求中原样返回
- `toolUseID` 必须从请求的 `tool_use_id` 字段提取并包含在响应中
- 使用 `PENDING_TOOL_USE_IDS` DashMap 存储 `request_id → tool_use_id` 映射

#### 6. 回合结束与进程生命周期

SDK 内部 `readMessages()` 方法的处理逻辑：

```
收到 result 事件
  → isSingleUserTurn ? transport.endInput() : 继续等待
  → endInput() 关闭 stdin
  → CLI 收到 EOF 后退出
  → stdout 到达 EOF
  → readMessages 循环结束
  → 调用 cleanup()
```

RCUI 必须复刻相同行为：

1. 收到 `result` 事件后，设置 `ActiveSession.stdin_tx = None`（等价于 SDK 的 `endInput()`）
2. **不要 break** — 继续读取 stdout 直到 CLI 自然退出（EOF）
3. 5 秒超时兜底（如果 CLI 未退出，超时后 break → DashMap remove → `kill_on_drop`）
4. 移除 DashMap 条目，发送 `complete` 到前端

**绝对不要**在收到 result 后直接 break + SIGKILL，这与 SDK 行为不一致。

#### 7. 进程清理（SDK `close()` 方法）

SDK 的 `ProcessTransport.close()` 实现了分级终止：

```
stdin.end()          → 关闭 stdin
等待 2 秒 (LM=2000)  → SIGTERM
再等 5 秒            → SIGKILL
```

RCUI 中 `kill_on_drop(true)` 会在 Child drop 时发送 SIGKILL，作为最后兜底。正常流程应该是 stdin 关闭 → CLI 自然退出。

#### 8. 对照检查清单

开发或修改 Claude CLI 对接功能时，逐项检查：

- [ ] CLI 参数是否与 SDK `cX.initialize()` 一致？
- [ ] 环境变量是否包含 `CLAUDE_CODE_ENTRYPOINT=sdk-ts`、`CLAUDE_CODE_STREAM_CLOSE_TIMEOUT=300000`？
- [ ] stdin 初始化消息格式是否与 SDK `initialize()` 一致？
- [ ] 用户消息格式是否包含 `session_id: ""`、不包含 `isSynthetic`？
- [ ] control_response 是否包含 `toolUseID`、`request_id`，subtype 是否为 `success`？
- [ ] result 事件后是否关闭 stdin 而非直接 break？
- [ ] 进程终止是否遵循 stdin 关闭 → 等待退出 → 超时兜底？
- [ ] 超时时间是否合理？（result 后 5 秒，正常 120 秒）
