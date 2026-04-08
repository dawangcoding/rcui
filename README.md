# RCUI

RCUI 是 [CloudCLI UI (claudecodeui)](https://github.com/siteboon/claudecodeui) 项目的 Rust 后端重写版本。原项目基于 Node.js/Express，RCUI 将后端完全替换为 Axum 0.8 + SQLx + SQLite 实现，前端保持原有 TypeScript/React 代码不变。为 **Claude Code**、**Cursor**、**Codex**、**Gemini** 四大 AI 编程助手提供统一的会话管理、项目浏览、Git 操作和 MCP 配置等功能。

> **说明：** 本项目前端代码直接来源于 claudecodeui，未做修改；所有后端逻辑（API 路由、数据库、WebSocket、Provider 适配等）均使用 Rust 从零实现，API 接口与原项目保持兼容。

## 功能特性

- **多 AI 助手支持** — 统一管理 Claude Code、Cursor、Codex、Gemini 的会话和消息
- **实时 WebSocket 通信** — 流式对话、权限请求/响应、文件变更广播
- **完整的项目管理** — 项目列表、文件浏览/编辑/上传、工作区创建
- **内置 Git 操作** — 状态查看、提交、分支管理、远程推拉等 20+ 个 Git 端点
- **MCP Server 管理** — Claude CLI 和 Cursor 的 MCP 配置管理
- **Web 终端** — 内嵌 xterm.js 终端，通过 PTY 与后端交互
- **安全认证** — JWT 认证 + bcrypt 密码哈希，支持 API Key 访问
- **推送通知** — Web Push (VAPID) 支持
- **国际化** — i18next 多语言支持
- **SPA 静态文件服务** — 生产模式下直接由 Rust 后端提供前端静态文件

## 技术栈

### 后端

| 组件 | 版本 | 说明 |
|------|------|------|
| Axum | 0.8 | Web 框架，支持 WebSocket 和 multipart |
| SQLx | 0.8 | 异步 SQLite（WAL 模式） |
| Tokio | 1 | 异步运行时 |
| tower-http | 0.6 | CORS / Trace / 静态文件 / gzip 压缩 |
| jsonwebtoken | 9 | JWT (HS256) |
| bcrypt | 0.17 | 密码哈希 |
| notify | 7 | 文件变更监听（300ms 防抖） |
| clap | 4 | CLI 参数解析 |
| dashmap | 6 | 并发安全 HashMap |
| portable-pty | 0.9 | 伪终端支持 |

### 前端

| 组件 | 说明 |
|------|------|
| React 18 | UI 框架 |
| Vite 7 | 构建工具 |
| TypeScript | 类型系统 |
| Tailwind CSS 3 | 样式方案 |
| CodeMirror 6 | 代码编辑器 |
| xterm.js 5 | Web 终端 |
| react-router-dom | 路由管理 |
| react-markdown | Markdown 渲染（支持 KaTeX 数学公式） |
| i18next | 国际化 |

## 项目结构

```
rcui/
├── Cargo.toml                    # Workspace 根配置
├── crates/
│   ├── rcui-server/              # 主服务端 (bin: rcui-server)
│   │   └── src/
│   │       ├── main.rs           # 入口：路由注册、中间件、静态文件服务
│   │       ├── config.rs         # 配置管理（环境变量）
│   │       ├── state.rs          # 共享状态（db, broadcast, sessions）
│   │       ├── error.rs          # 统一错误类型
│   │       ├── auth/             # JWT / 中间件 / 密码哈希
│   │       ├── db/               # 数据库层（用户、API Key、凭证等）
│   │       ├── providers/        # AI 助手适配器
│   │       │   ├── claude/       # Claude（~/.claude/projects/ JSONL）
│   │       │   ├── cursor/       # Cursor（~/.cursor/chats/ SQLite）
│   │       │   ├── codex/        # Codex（~/.codex/sessions/ JSON）
│   │       │   └── gemini/       # Gemini（~/.gemini/tmp/）
│   │       ├── routes/           # HTTP 路由 handler
│   │       └── services/         # 后台服务（chat、文件监听、项目扫描）
│   └── rcui-cli/                 # CLI 工具 (bin: rcui)
│       └── src/main.rs           # start / open / status / stop
├── migrations/                   # SQLite schema
├── frontend/                     # React 前端
│   ├── src/
│   ├── public/
│   ├── package.json
│   └── vite.config.js
└── build.sh                      # 一键构建脚本
```

## 快速开始

### 前置要求

- Rust 1.85+ (Edition 2024)
- Node.js 18+
- npm 或 pnpm

### 开发模式

**终端 1 — 启动后端：**

```bash
cargo run -p rcui-server
```

**终端 2 — 启动前端：**

```bash
cd frontend
npm install
npm run dev
```

前端开发服务器运行在 `http://localhost:5173`，API 和 WebSocket 请求会自动代理到后端 `localhost:3001`。

### 生产构建

```bash
# 一键构建（前端 + 后端）
bash build.sh

# 启动生产服务
STATIC_DIR=frontend/dist ./target/release/rcui-server
```

### 使用 CLI

```bash
# 构建 CLI
cargo build --release -p rcui-cli

# 启动服务
./target/release/rcui start --port 3001

# 后台运行
./target/release/rcui start --port 3001 --daemon

# 指定前端目录
./target/release/rcui start --static-dir frontend/dist

# 在浏览器中打开
./target/release/rcui open

# 查看状态
./target/release/rcui status

# 停止服务
./target/release/rcui stop
```

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SERVER_PORT` / `PORT` | `3001` | 服务端口 |
| `HOST` | `0.0.0.0` | 绑定地址 |
| `JWT_SECRET` | 自动生成 | JWT 签名密钥（未设置时自动生成并存入数据库） |
| `DATABASE_PATH` | `~/.rcui/auth.db` | SQLite 数据库路径 |
| `STATIC_DIR` | — | 前端 dist 目录路径（设置后启用静态文件服务） |
| `WORKSPACES_ROOT` | `$HOME` | 工作区扫描根目录 |
| `CONTEXT_WINDOW` | `160000` | 上下文窗口大小 |
| `VITE_IS_PLATFORM` | `false` | Platform 模式（跳过 JWT 认证） |
| `API_KEY` | — | 全局 API Key |
| `RUST_LOG` | `info` | 日志级别 |

## API 概览

### 公开端点

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/health` | 健康检查 |
| GET | `/ws` | WebSocket 连接（query param 认证） |
| GET | `/shell` | Web 终端 WebSocket |
| POST | `/api/auth/register` | 用户注册（仅限一个用户） |
| POST | `/api/auth/login` | 用户登录 |
| GET | `/api/auth/status` | 认证状态 |

### 受保护端点（需要 JWT）

| 模块 | 路径前缀 | 说明 |
|------|----------|------|
| 项目管理 | `/api/projects` | 项目列表 / 添加 / 重命名 / 删除 / 文件操作 |
| 会话管理 | `/api/sessions` | 会话列表 / 消息 / 删除 / 命名 |
| Git 操作 | `/api/git` | 状态 / 提交 / 分支 / 远程 / diff 等 20+ 端点 |
| MCP 管理 | `/api/mcp` | Claude CLI MCP 配置 |
| Cursor MCP | `/api/cursor/mcp` | Cursor MCP 配置 |
| 设置 | `/api/settings` | API Key / 凭证 / 通知偏好 / 推送订阅 |
| 用户 | `/api/user` | Git 配置 / Onboarding 状态 |
| 命令 | `/api/commands` | 命令列表 / 加载 / 执行 |

## AI 助手会话数据来源

| Provider | 数据目录 | 格式 |
|----------|----------|------|
| Claude | `~/.claude/projects/{encoded_path}/` | JSONL 文件 |
| Cursor | `~/.cursor/chats/` | SQLite 数据库（路径 MD5 哈希） |
| Codex | `~/.codex/sessions/` | JSON 文件 |
| Gemini | `~/.gemini/tmp/` | 会话文件 |

## 数据库

使用 SQLite 存储应用数据，WAL 模式保证并发读写性能。Schema 嵌入二进制文件，启动时自动创建表。

主要数据表：

- `users` — 用户信息（单用户模式）
- `app_config` — KV 配置存储
- `session_names` — 自定义会话名称
- `api_keys` — API Key 管理
- `user_credentials` — 用户凭证（如 GitHub Token）
- `push_subscriptions` — 推送通知订阅
- `vapid_keys` — Web Push VAPID 密钥
- `user_notification_preferences` — 通知偏好

## 代码质量

```bash
# 编译检查
cargo check

# 格式化
cargo fmt

# Lint
cargo clippy

# 前端类型检查
cd frontend && npm run typecheck
```

## License

MIT
