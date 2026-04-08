# 终端 (Shell) 功能架构文档

## 整体架构

终端功能采用**前后端分离 + WebSocket 实时通信 + PTY 伪终端**的架构：

```
┌─────────────────────────────────────────────────────┐
│  前端 (React + xterm.js)                            │
│  Shell.tsx → useShellRuntime → useShellTerminal     │
│                               → useShellConnection  │
└────────────────────┬────────────────────────────────┘
                     │ WebSocket (/shell?token=JWT)
                     │ 双向 JSON 消息
┌────────────────────┴────────────────────────────────┐
│  后端 (Axum + portable_pty)                         │
│  routes/shell.rs → services/pty_manager.rs          │
│  PTY 进程管理、会话复用、输出缓冲                      │
└─────────────────────────────────────────────────────┘
```

---

## 前端组件层次

```
Shell.tsx (主组件，CLI prompt 检测)
├── useShellRuntime (编排 hook)
│   ├── useShellTerminal (xterm.js 生命周期、addon 加载)
│   └── useShellConnection (WebSocket 生命周期、消息处理)
├── ShellHeader (状态栏 + 断开/重启按钮)
├── ShellConnectionOverlay (加载/连接/待连接覆盖层)
├── ShellMinimalView (精简模式，auth URL 面板)
├── ShellEmptyState (未选择项目)
└── TerminalShortcutsPanel (移动端虚拟键盘)
```

### 文件清单

| 文件 | 说明 |
|------|------|
| `frontend/src/components/shell/types/types.ts` | 消息类型与接口定义 |
| `frontend/src/components/shell/constants/constants.ts` | xterm 配置、终端参数、prompt 检测参数 |
| `frontend/src/components/shell/utils/socket.ts` | WebSocket URL 构建、JSON 解析 |
| `frontend/src/components/shell/utils/auth.ts` | Auth URL 解析（Codex 特殊处理） |
| `frontend/src/components/shell/utils/terminalStyles.ts` | xterm 焦点样式注入 |
| `frontend/src/components/shell/hooks/useShellTerminal.ts` | 终端生命周期、xterm 初始化、addon 加载 |
| `frontend/src/components/shell/hooks/useShellConnection.ts` | WebSocket 生命周期、消息处理、重连 |
| `frontend/src/components/shell/hooks/useShellRuntime.ts` | Hook 编排、状态管理 |
| `frontend/src/components/shell/view/Shell.tsx` | 主组件、prompt 检测、UI 布局 |
| `frontend/src/components/shell/view/subcomponents/ShellHeader.tsx` | 连接状态、断开/重启按钮 |
| `frontend/src/components/shell/view/subcomponents/ShellConnectionOverlay.tsx` | 加载/连接覆盖层 |
| `frontend/src/components/shell/view/subcomponents/ShellMinimalView.tsx` | 精简模式 auth URL 面板 |
| `frontend/src/components/shell/view/subcomponents/ShellEmptyState.tsx` | 未选择项目状态 |
| `frontend/src/components/shell/view/subcomponents/TerminalShortcutsPanel.tsx` | 移动端虚拟键盘 |

---

## WebSocket 消息协议

### 前端 → 后端（3 种消息）

#### 1. 初始化消息 (`init`)

```json
{
  "type": "init",
  "projectPath": "/Users/user/projects/myapp",
  "sessionId": "session-123",
  "hasSession": true,
  "provider": "claude",
  "cols": 100,
  "rows": 30,
  "initialCommand": null,
  "isPlainShell": false
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `projectPath` | `string` | 项目绝对路径 |
| `sessionId` | `string \| null` | 会话 ID，plain-shell 时为 null |
| `hasSession` | `boolean` | 是否有已存在的会话 |
| `provider` | `string` | `claude` / `cursor` / `codex` / `gemini` / `plain-shell` |
| `cols` / `rows` | `number` | 终端列数/行数 |
| `initialCommand` | `string \| null` | 初始命令（plain-shell 模式使用） |
| `isPlainShell` | `boolean` | 是否为普通 shell 模式 |

#### 2. 输入消息 (`input`)

```json
{
  "type": "input",
  "data": "ls -la\n"
}
```

原始字符数据，包含控制序列（如 `\x1b` 为 Esc，`\x03` 为 Ctrl+C）。

#### 3. 窗口大小变化 (`resize`)

```json
{
  "type": "resize",
  "cols": 120,
  "rows": 40
}
```

### 后端 → 前端（2 种消息）

#### 1. 进程输出 (`output`)

```json
{
  "type": "output",
  "data": "total 48\ndrwxr-xr-x..."
}
```

包含 ANSI 转义序列的原始 PTY 输出。

#### 2. 认证 URL (`auth_url`)

```json
{
  "type": "auth_url",
  "url": "https://auth.openai.com/codex/device?code=ABC123"
}
```

后端检测到 CLI 输出中包含认证 URL 时发送。

---

## 后端 PTY 管理

### 进程创建流程

文件：`crates/rcui-server/src/routes/shell.rs`、`crates/rcui-server/src/services/pty_manager.rs`

1. 收到 `init` 消息后，**验证路径安全性**（防遍历、防空字节、防符号链接）
2. **验证 Session ID**（正则白名单 `[a-zA-Z0-9_.:\-]+`）
3. 根据 provider **构建 shell 命令**：

| Provider | 命令模板 |
|----------|---------|
| Claude | `claude --resume "{sid}" \|\| claude` |
| Cursor | `cursor-agent --resume="{sid}"` |
| Codex | `codex resume "{sid}" \|\| codex` |
| Gemini | `gemini --resume "{sid}"` |
| Plain Shell | 直接运行 `initialCommand` 或交互式 `bash` |

4. 使用 `portable_pty` 创建 **PTY master/slave 对**
5. 设置环境变量：`TERM=xterm-256color`、`COLORTERM=truecolor`、`FORCE_COLOR=3`
6. 在项目目录下 spawn `bash -c "{command}"`

### Session Key 计算

```
格式: {project_path}_{session_id|default}[_cmd_{base64_prefix}]
示例: /Users/me/proj_default_cmd_Y2xhdWRlIC1w
```

### 会话复用与重连

```
断开连接
  → ws_tx 置为 None（PTY 进程继续运行）
  → 输出写入环形缓冲区（容量 5000 条）
  → 启动 30 分钟清理定时器

重连（30 分钟内）
  → 匹配 session_key 找到已有会话
  → 取消清理定时器
  → 回放环形缓冲区（补齐断开期间的输出）
  → 重新绑定 ws_tx
  → 调整 PTY 终端大小

超时（30 分钟后未重连）
  → kill 子进程
  → 中止 reader task
  → 移除会话记录
```

### 输出读取（Background Reader Task）

- 通过 `tokio::task::spawn_blocking()` 在阻塞线程中读取 PTY 输出
- 4KB chunk 循环读取
- 每个 chunk：
  1. UTF-8 lossy 转换
  2. 去 ANSI 检测 auth URL（正则匹配 + 触发短语检测）
  3. 发送 `output` 消息到 WebSocket
  4. 写入环形缓冲区
  5. 直到 EOF（进程退出）

### Auth URL 检测

- 维护滚动 32KB 干净文本缓冲区
- 正则提取 URL：`https?://[^\s<>"...]`
- 检测触发短语：`browser didn't open`、`open this url`、`device code`、`authenticate` 等
- 去重：HashSet 记录已公告的 URL
- 返回 `(url, should_auto_open)` 元组

---

## 前端 xterm.js 配置

### 终端选项

| 配置项 | 值 | 说明 |
|--------|-----|------|
| `fontFamily` | `Menlo, Monaco, 'Courier New', monospace` | 字体 |
| `fontSize` | `14` | 字号 |
| `cursorBlink` | `true` | 光标闪烁 |
| `scrollback` | `10000` | 滚动缓冲行数 |
| `tabStopWidth` | `4` | Tab 宽度 |
| `macOptionIsMeta` | `true` | Mac Option 键作为 Meta |
| `theme` | VS Code Dark | gray-900 背景，gray-100 前景 |

### 加载的 Addon

| Addon | 说明 | 备注 |
|-------|------|------|
| **FitAddon** | 终端自适应容器大小 | 必需，resize 时调用 `fitAddon.fit()` |
| **WebLinksAddon** | 链接可点击 | 精简模式下禁用（防止换行链接干扰登录流程） |
| **WebglAddon** | GPU 加速渲染 | 可选，不可用时 Canvas 降级 |

### 键盘处理

通过 `terminal.attachCustomKeyEventHandler()` 自定义：

| 快捷键 | 行为 |
|--------|------|
| `Cmd/Ctrl+C`（有选中文本） | 复制选中内容到剪贴板 |
| `Cmd/Ctrl+V` | 从剪贴板读取文本，通过 WebSocket 发送到 PTY |
| 精简模式下 `c` 键 | 复制 auth URL 到剪贴板 |
| 其他按键 | 默认终端行为（通过 `terminal.onData` 发送到 PTY） |

### 移动端虚拟键盘 (TerminalShortcutsPanel)

桌面端隐藏（`md:hidden`），移动端显示：

- 粘贴、Esc、Tab、Shift+Tab 按钮
- CTRL / ALT 修饰键切换
- 方向键（上/下/左/右）
- Scroll Down（滚动到底部）

Ctrl 组合键实现：字符 `a` → `\x01`（Ctrl+A），即 `charCode - 96`。
Alt 组合键实现：前缀 `\x1b`（如 Alt+X → `\x1b` + `x`）。

---

## CLI Prompt 检测

Shell.tsx 中实现了对 CLI 选择提示（如 Claude 的权限模式选择）的自动检测。

### 检测算法

1. **触发**：终端输出后 **500ms 防抖**触发扫描
2. **定位 footer**：从光标位置向下扫描，寻找 `esc to cancel` 或 `enter to select` 文本
3. **提取选项**：从 footer 向上扫描 15 行，匹配 `/^\s*[❯›>]?\s*(\d+)\.\s+(.+)/` 格式
4. **验证**：选项编号 1-N 必须连续无间断
5. **显示**：满足 2-5 个有效选项时，渲染底部浮层按钮

### UI 渲染

```
┌──────────────────────────────────────────┐
│  终端输出区域                             │
│  ❯ 1. allowedAutonomousMode             │
│    2. planMode                           │
│    3. defaultMode                        │
│                                          │
├──────────────────────────────────────────┤
│ [1. allowedAutonomousMode] [2. planMode] │
│ [3. defaultMode] [Esc]                   │
└──────────────────────────────────────────┘
```

- 数字按钮：发送对应数字到 PTY
- Esc 按钮：发送 `\x1b`（ESC 转义序列）
- 点击后立即清除选项浮层

### 相关常量

| 常量 | 值 | 说明 |
|------|-----|------|
| `PROMPT_DEBOUNCE_MS` | `500` | 输出后等待时间 |
| `PROMPT_BUFFER_SCAN_LINES` | `20` | 向上扫描行数 |
| `PROMPT_OPTION_SCAN_LINES` | `15` | 选项扫描行数 |
| `PROMPT_MIN_OPTIONS` | `2` | 最少选项数 |
| `PROMPT_MAX_OPTIONS` | `5` | 最多选项数 |

---

## 连接生命周期

### 连接流程

```
用户点击 "在 Shell 中继续"
  → connectToShell()
  → 创建 WebSocket(/shell?token=JWT)
  → ws.onopen()
    → isConnected = true
    → fitAddon.fit()
    → 发送 init 消息（项目路径、provider、终端尺寸等）
  → 后端创建/复用 PTY
  → 输出通过 WebSocket 写入 xterm.js
```

### 断开流程

```
disconnectFromShell()
  → closeSocket()（关闭 WebSocket）
  → clearTerminalScreen()（清屏 + ANSI reset）
  → 重置状态：isConnected/isConnecting = false
  → 清除 authUrl
  → 后端：ws_tx 置 None，启动 30 分钟清理定时器
```

### 重启流程

```
用户点击 "重启" 按钮
  → handleRestartShell()
    → pendingRestartConnectRef = true
    → isRestarting = true
  → useShellRuntime effect 触发
    → disconnectFromShell()（关闭 WebSocket）
    → disposeTerminal()（销毁 xterm.js 实例）
  → 200ms 后 isRestarting = false
  → useShellTerminal effect 重新创建 xterm.js 实例
    → isInitialized = true
  → auto-connect effect 检测到 pending 标记
    → connectToShell()（建立新 WebSocket + 发送 init）
```

### 会话切换

```
selectedSession.id 变化
  → disconnectFromShell()（断开旧连接）
  → 用户手动点击连接（或 autoConnect 自动触发）
  → 新 init 消息携带新的 sessionId
```

### 自动连接

当 `autoConnect=true` 时，终端初始化完成后自动调用 `connectToShell()`：

```typescript
useEffect(() => {
  if (!autoConnect || !isInitialized || isConnecting || isConnected) return;
  connectToShell();
}, [autoConnect, connectToShell, isConnected, isConnecting, isInitialized]);
```

---

## 认证流程

### WebSocket 认证

```typescript
// 前端：构建带 JWT token 的 WebSocket URL
const token = localStorage.getItem('auth-token');
const url = `ws://${host}/shell?token=${encodeURIComponent(token)}`;
```

```rust
// 后端：验证 JWT
let user_id = auth::jwt::verify_token(&state.jwt_secret, &token)
    .map(|data| data.claims.user_id)
    .ok();
// 无效 token → 发送 "Authentication required" → 关闭连接
```

Platform 模式（`VITE_IS_PLATFORM=true`）跳过 token 校验。

### CLI Auth URL 处理

1. 后端 reader task 检测到 PTY 输出中的认证 URL
2. 发送 `auth_url` 消息到前端
3. 前端更新 `authUrl` 状态
4. **精简模式**（`minimal=true`）：ShellMinimalView 显示 URL 输入框 + "打开" / "复制" 按钮
5. **Codex 登录特殊处理**：`codex login` 命令使用固定的 `CODEX_DEVICE_AUTH_URL`

---

## 安全措施

### 路径验证

- 禁止空字节（`\0`）
- 禁止路径遍历（`..`）
- 禁止符号链接（canonicalize 后验证）
- 验证目标是目录

### Session ID 验证

- 正则白名单：`[a-zA-Z0-9_.:\-]+`
- 拒绝任何 shell 元字符

### 并发限制

- 单 WebSocket 连接最多 10 个 PTY 会话
- 超出限制返回 BadRequest

### 进程清理

- 断开连接后 30 分钟超时 kill
- `kill_on_drop(true)` 作为最终兜底
- 环形缓冲区有容量上限（5000 条）

---

## 状态管理

### 前端状态

| 状态 | 类型 | 说明 |
|------|------|------|
| `isConnected` | `boolean` | WebSocket 已连接 |
| `isInitialized` | `boolean` | xterm.js 已初始化 |
| `isConnecting` | `boolean` | 连接进行中 |
| `isRestarting` | `boolean` | 重启进行中 |
| `authUrl` | `string` | 当前 auth URL |
| `authUrlVersion` | `number` | auth URL 变更计数（强制 UI 刷新） |
| `cliPromptOptions` | `CliPromptOption[] \| null` | 检测到的 CLI 选项 |

### 前端 Ref（可变引用，跨渲染保持）

| Ref | 说明 |
|-----|------|
| `wsRef` | 当前 WebSocket 实例 |
| `terminalRef` | xterm.js Terminal 实例 |
| `fitAddonRef` | FitAddon 实例 |
| `authUrlRef` | 最新 auth URL（原始字符串） |
| `selectedProjectRef` | 当前项目（供 WebSocket handler 读取最新值） |
| `selectedSessionRef` | 当前会话 |
| `pendingRestartConnectRef` | 重启后待自动重连标记 |

### 后端状态

| 状态 | 存储 | 说明 |
|------|------|------|
| `pty_sessions` | `Arc<Mutex<HashMap<String, PtySession>>>` | 所有活跃 PTY 会话 |
| `current_session_key` | 连接级局部变量 | 当前连接绑定的会话 |

`PtySession` 包含：PTY master、writer、child 进程、环形缓冲区、ws_tx 通道、reader task handle、cleanup task handle。

---

## 端到端示例：用户在项目中运行 Claude

### 1. 连接阶段

```
用户点击 "在 Shell 中继续"
  → 前端创建 xterm.js 实例（Menlo 14px, WebGL, 10k scrollback）
  → 建立 WebSocket ws://localhost:3001/shell?token=xxx
  → 发送 init 消息：
    { type: "init", projectPath: "/Users/user/myapp", provider: "claude",
      sessionId: null, cols: 100, rows: 30 }
```

### 2. 后端处理

```
收到 init
  → 路径验证通过
  → session_key = "/Users/user/myapp_default"
  → 无已有会话，构建命令: bash -c "claude"
  → spawn PTY 进程（TERM=xterm-256color）
  → 启动 reader task
  → 注册到 pty_sessions
```

### 3. 终端交互

```
Claude CLI 输出: "Welcome to Claude! Syncing..."
  → reader task 读取 4KB chunk
  → 发送 { type: "output", data: "Welcome..." }
  → 前端 terminal.write("Welcome...")
  → xterm.js 渲染

用户输入: "帮我修复 bug"
  → terminal.onData("帮我修复 bug")
  → 发送 { type: "input", data: "帮我修复 bug" }
  → 后端写入 PTY writer
  → Claude CLI 处理并输出结果
```

### 4. 认证场景

```
Claude CLI 输出: "Open this URL: https://auth.anthropic.com/..."
  → reader task 检测到 auth URL
  → 发送 { type: "auth_url", url: "https://auth.anthropic.com/..." }
  → 前端 authUrl 状态更新
  → ShellMinimalView 显示 URL + "打开" / "复制" 按钮
```

### 5. 断开与重连

```
用户断开连接
  → 前端关闭 WebSocket + 清屏
  → 后端 ws_tx = None，输出继续写入缓冲区
  → 30 分钟清理定时器启动

用户重新连接（10 分钟后）
  → 新 WebSocket + 新 init 消息
  → 后端匹配到已有 session_key
  → 取消清理定时器
  → 回放缓冲区（补齐 10 分钟内的输出）
  → 继续正常交互
```

---

## 错误处理

### 前端

| 场景 | 处理 |
|------|------|
| WebSocket 连接错误 | `isConnected = false`，可手动重连 |
| WebSocket 关闭 | 清屏 + 重置状态 |
| 消息解析失败 | `console.error`，继续处理 |
| 剪贴板操作失败 | 静默失败（`.catch(() => {})`） |

### 后端

| 场景 | 处理 |
|------|------|
| 路径验证失败 | 400 BadRequest |
| Session ID 无效 | 400 BadRequest |
| PTY 会话数超限 | 400 BadRequest |
| PTY spawn 失败 | 500 Internal |
| PTY 读取失败 | 中断 reader loop，触发清理 |
| PTY 写入失败 | warn 日志，继续 |
