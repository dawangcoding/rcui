# @anthropic-ai/claude-agent-sdk 系统架构分析

> 基于 v0.2.92 (2026-04-04) 源码分析，供 RCUI 项目实现 Claude 相关功能时参考。

## 一、整体定位

Claude Agent SDK 是 Claude Code CLI 的**编程接口包装层（SDK wrapper）**。它不直接调用 Anthropic API，而是通过 **spawn 一个 Claude Code CLI 子进程** 来工作，然后通过 stdio/WebSocket 进行通信。本质上是一个"进程间通信(IPC)框架 + 消息协议 + 会话管理"系统。

```
你的应用 (Node.js/Bun)
    |
    v
+----------------------+
|  claude-agent-sdk    |  <- SDK 层 (sdk.mjs)
|  (npm 包)            |
+------+---------------+
       | spawn 子进程 / WebSocket
       v
+----------------------+
|  Claude Code CLI     |  <- cli.js (13MB, 内嵌在包中)
|  (子进程)            |
+------+---------------+
       | Anthropic API (HTTP)
       v
+----------------------+
|  Claude Model API    |  <- Anthropic 后端
+----------------------+
```

## 二、包结构 — 5 个导出入口

| 入口 | 文件 | 用途 |
|------|------|------|
| `@anthropic-ai/claude-agent-sdk` | sdk.mjs / sdk.d.ts | **主入口** — `query()`, 会话管理, V2 API |
| `.../browser` | browser-sdk.js | **浏览器端** — WebSocket transport |
| `.../bridge` | bridge.mjs | **远程桥接** — 连接 claude.ai CCR 后端 |
| `.../embed` | embed.js | CLI 路径导出 — `default cliPath` |
| `.../sdk-tools` | sdk-tools.d.ts | 工具 I/O 类型定义（纯类型，无运行时） |

依赖仅两个：
- `@anthropic-ai/sdk` ^0.80.0 — 类型复用 (BetaMessage 等)
- `@modelcontextprotocol/sdk` ^1.27.1 — MCP 协议支持

## 三、核心交互架构

### 1. V1 API — `query()` 函数（主力接口）

```typescript
function query(params: {
    prompt: string | AsyncIterable<SDKUserMessage>;
    options?: Options;
}): Query;
```

`Query` 接口实现了 `AsyncGenerator<SDKMessage, void>`，数据流是**单向流式**的：

```
调用方                              SDK                           CLI 子进程
  |                                  |                               |
  |  query({ prompt, options })      |                               |
  | -------------------------------->|  spawn cli.js                |
  |                                  |------------------------------>|
  |                                  |  stdin: SDKUserMessage (JSON) |
  |                                  |------------------------------>|
  |                                  |                               |
  |                                  |  stdout: SDKMessage (JSON行)  |
  |  for await (const msg of query)  |<------------------------------|
  | <--------------------------------|                               |
  |                                  |                               |
  |  query.interrupt()               |  control_request              |
  | -------------------------------->|------------------------------>|
  |                                  |  control_response             |
  |                                  |<------------------------------|
```

**Query 控制方法**：
- `interrupt()` — 中断当前执行
- `setModel()` / `setPermissionMode()` — 动态调整配置
- `setMcpServers()` — 运行时增删 MCP 服务器
- `rewindFiles()` — 文件回滚到某个消息点
- `streamInput()` — 多轮对话时注入新消息
- `close()` — 强制终止
- `initializationResult()` — 获取初始化信息（commands, models, account）
- `supportedCommands()` / `supportedModels()` / `supportedAgents()` — 查询能力
- `mcpServerStatus()` — MCP 服务器状态
- `getContextUsage()` — 上下文窗口使用量
- `accountInfo()` — 账户信息
- `applyFlagSettings()` — 运行时合并设置
- `seedReadState()` — 种子读取状态缓存
- `stopTask()` — 停止子任务
- `reconnectMcpServer()` / `toggleMcpServer()` — MCP 管理

### 2. V2 API — Session 模式（不稳定/Alpha）

```typescript
function unstable_v2_createSession(options: SDKSessionOptions): SDKSession;
function unstable_v2_resumeSession(sessionId: string, options: SDKSessionOptions): SDKSession;
function unstable_v2_prompt(message: string, options: SDKSessionOptions): Promise<SDKResultMessage>;
```

V2 将 `query()` 的单次调用拆分为持久化会话：

```typescript
interface SDKSession {
    readonly sessionId: string;
    send(message: string | SDKUserMessage): Promise<void>;  // 发送
    stream(): AsyncGenerator<SDKMessage, void>;             // 接收
    close(): void;
}
```

区别于 V1 的 `prompt` 一次性传入，V2 支持 `send()` + `stream()` 分离调用。

### 3. Browser 入口 — WebSocket Transport

```typescript
// 从 '@anthropic-ai/claude-agent-sdk/browser' 导入
function query(options: BrowserQueryOptions): Query;

type BrowserQueryOptions = {
    prompt: AsyncIterable<SDKUserMessage>;
    websocket: {
        url: string;                    // wss://...
        headers?: Record<string, string>;
        authMessage?: AuthMessage;      // OAuth token
    };
    canUseTool?: CanUseTool;
    hooks?: Partial<Record<HookEvent, HookCallbackMatcher[]>>;
    mcpServers?: Record<string, McpServerConfig>;
};
```

### 4. Bridge 入口 — 远程 CCR 会话

Bridge 用于连接 **claude.ai 的 Cloud Code Runner (CCR)** 后端：

```
本地 Worker                Bridge                    CCR (claude.ai)
    |                        |                            |
    |  fetchRemoteCredentials|  POST /v1/code/sessions/   |
    | ---------------------->|  {id}/bridge               |
    |                        |--------------------------->|
    |                        |  { worker_jwt, epoch }     |
    |                        |<---------------------------|
    |                        |                            |
    |  attachBridgeSession   |  SSE stream (持久连接)     |
    | ---------------------->| <--------------------------|
    |                        |                            |
    |  handle.write(msg)     |  POST events               |
    | ---------------------->|--------------------------->|
    |                        |                            |
    |  onInboundMessage(msg) |  SSE event                 |
    | <----------------------|<---------------------------|
```

**BridgeSessionHandle 方法**：
- `write(msg)` — 向 CCR 发送 SDKMessage
- `sendResult()` — 标记 turn 结束
- `sendControlRequest/Response` — 权限请求转发
- `reportState('idle'|'running'|'requires_action')` — 状态上报
- `reportMetadata()` — 上报分支/目录等元数据
- `reportDelivery()` — 上报事件处理状态
- `reconnectTransport()` — JWT 过期后重连
- `getSequenceNum()` — SSE 高水位标记，断线续传
- `flush()` / `close()` — 清理

**关键函数**：
- `createCodeSession(baseUrl, accessToken, title, timeoutMs, tags?)` — 创建 CCR 会话
- `fetchRemoteCredentials(sessionId, baseUrl, accessToken, ...)` — 获取 Worker JWT
- `attachBridgeSession(opts)` — 挂载到会话

## 四、消息协议 — SDKMessage 类型体系

SDKMessage 是整个系统的核心数据载体，所有交互都围绕这 20+ 种消息类型：

```
SDKMessage (联合类型)
+-- SDKAssistantMessage      — Claude 的回复 (含 BetaMessage)
+-- SDKUserMessage           — 用户消息
+-- SDKUserMessageReplay     — 恢复会话时的历史回放
+-- SDKResultMessage         — 执行结果 (success / error)
|   +-- SDKResultSuccess     — 成功，含 cost/usage/structured_output
|   +-- SDKResultError       — 失败，含 error 类型
+-- SDKSystemMessage         — 系统初始化信息 (init)
+-- SDKPartialAssistantMessage — 流式 token (stream_event)
+-- SDKStatusMessage         — 状态变更 (compacting 等)
+-- SDKCompactBoundaryMessage — 上下文压缩边界标记
+-- SDKAPIRetryMessage       — API 重试通知
+-- SDKToolProgressMessage   — 工具执行进度
+-- SDKToolUseSummaryMessage — 工具使用摘要
+-- SDKAuthStatusMessage     — 认证状态
+-- SDKTaskStartedMessage    — 子任务启动
+-- SDKTaskProgressMessage   — 子任务进度 (含 summary)
+-- SDKTaskNotificationMessage — 子任务完成通知
+-- SDKSessionStateChangedMessage — 会话状态变更
+-- SDKRateLimitEvent        — 速率限制事件
+-- SDKHookStarted/Progress/ResponseMessage — Hook 生命周期
+-- SDKLocalCommandOutputMessage — 本地命令输出
+-- SDKPromptSuggestionMessage — 下一轮提示建议
+-- SDKElicitationCompleteMessage — MCP 表单完成
+-- SDKFilesPersistedEvent   — 文件持久化事件
```

### 关键消息类型详解

#### SDKAssistantMessage
```typescript
type SDKAssistantMessage = {
    type: 'assistant';
    message: BetaMessage;              // Anthropic API 原始响应
    parent_tool_use_id: string | null; // 工具调用上下文
    error?: SDKAssistantMessageError;
    uuid: UUID;
    session_id: string;
};
```

#### SDKResultSuccess
```typescript
type SDKResultSuccess = {
    type: 'result';
    subtype: 'success';
    duration_ms: number;
    duration_api_ms: number;
    num_turns: number;
    result: string;
    total_cost_usd: number;
    usage: NonNullableUsage;
    modelUsage: Record<string, ModelUsage>;
    permission_denials: SDKPermissionDenial[];
    structured_output?: unknown;
    deferred_tool_use?: SDKDeferredToolUse;
    terminal_reason?: TerminalReason;
};
```

#### SDKSystemMessage (init)
```typescript
type SDKSystemMessage = {
    type: 'system';
    subtype: 'init';
    agents?: string[];
    apiKeySource: ApiKeySource;
    claude_code_version: string;
    cwd: string;
    tools: string[];
    mcp_servers: { name: string; status: string; }[];
    model: string;
    permissionMode: PermissionMode;
    slash_commands: string[];
    skills: string[];
    plugins: { name: string; path: string; }[];
};
```

#### SDKUserMessage
```typescript
type SDKUserMessage = {
    type: 'user';
    message: MessageParam;             // Anthropic SDK MessageParam
    parent_tool_use_id: string | null;
    isSynthetic?: boolean;
    tool_use_result?: unknown;
    priority?: 'now' | 'next' | 'later';
    timestamp?: string;
    uuid?: UUID;
    session_id?: string;
};
```

## 五、控制请求双向通道

SDK 和 CLI 子进程间有一个 **请求-响应** 控制通道：

```
SDKControlRequest (type: 'control_request')
+-- initialize          — 初始化会话（hooks, MCP, agents, systemPrompt）
+-- interrupt           — 中断执行
+-- can_use_tool        — 权限请求（最重要！）
+-- set_model           — 切换模型
+-- set_permission_mode — 切换权限模式
+-- set_max_thinking_tokens
+-- mcp_status          — 查询 MCP 服务器状态
+-- mcp_set_servers     — 动态设置 MCP 服务器
+-- mcp_reconnect       — 重连 MCP 服务器
+-- mcp_toggle          — 启用/禁用 MCP 服务器
+-- mcp_message         — 向 MCP 发送 JSON-RPC
+-- get_context_usage   — 查询上下文占用
+-- rewind_files        — 文件回滚
+-- seed_read_state     — 种子读取状态缓存
+-- hook_callback       — Hook 回调执行
+-- reload_plugins      — 重载插件
+-- stop_task           — 停止子任务
+-- apply_flag_settings — 运行时合并设置
+-- elicitation         — MCP 表单请求
+-- get_settings        — 获取当前设置
+-- cancel_async_message — 取消异步消息
+-- ...更多 (side_question, generate_title, remote_control 等)
```

## 六、权限系统 — canUseTool 回调

每次工具执行前，CLI 会发送 `can_use_tool` 控制请求：

```typescript
type CanUseTool = (
    toolName: string,
    input: Record<string, unknown>,
    options: {
        signal: AbortSignal;
        suggestions?: PermissionUpdate[];  // 建议的永久规则
        blockedPath?: string;
        decisionReason?: string;
        title?: string;        // "Claude wants to read foo.txt"
        displayName?: string;  // "Read file"
        description?: string;
        toolUseID: string;
        agentID?: string;      // 子 agent 调用时存在
    }
) => Promise<PermissionResult>;

type PermissionResult = 
    | { behavior: 'allow'; updatedInput?: Record<string, unknown>; updatedPermissions?: PermissionUpdate[]; }
    | { behavior: 'deny'; message: string; interrupt?: boolean; };
```

权限模式 6 种：`default` | `acceptEdits` | `bypassPermissions` | `plan` | `dontAsk` | `auto`

## 七、Hook 系统 — 27 种事件

Hook 是 CLI 执行过程中的拦截点，支持 `command`（shell）、`prompt`（LLM）、`agent`（代理验证） 三种类型：

```
PreToolUse / PostToolUse / PostToolUseFailure  — 工具生命周期
SessionStart / SessionEnd                       — 会话生命周期
UserPromptSubmit                                — 用户输入
SubagentStart / SubagentStop                    — 子 Agent 生命周期
PreCompact / PostCompact                        — 上下文压缩
PermissionRequest / PermissionDenied            — 权限事件
Notification                                    — 通知
Stop / StopFailure                              — 停止事件
TaskCreated / TaskCompleted                     — 任务事件
Elicitation / ElicitationResult                 — MCP 表单
ConfigChange                                    — 配置变更
FileChanged / CwdChanged                        — 文件/目录变更
InstructionsLoaded                              — 指令加载
WorktreeCreate / WorktreeRemove                 — Git Worktree
Setup / TeammateIdle                            — 设置/协作
```

## 八、子 Agent / 子任务架构

```typescript
type AgentDefinition = {
    description: string;          // 何时使用
    prompt: string;              // 系统提示词
    tools?: string[];            // 允许的工具
    disallowedTools?: string[];  // 禁止的工具
    model?: string;              // 模型选择
    mcpServers?: AgentMcpServerSpec[];
    maxTurns?: number;
    background?: boolean;        // 异步执行（fire-and-forget）
    memory?: 'user' | 'project' | 'local';
    effort?: EffortLevel;
    permissionMode?: PermissionMode;
    skills?: string[];
    initialPrompt?: string;
    criticalSystemReminder_EXPERIMENTAL?: string;
};
```

子 Agent 通过内部 Agent 工具调用，产出消息序列：
1. `SDKTaskStartedMessage` — 子任务启动
2. `SDKTaskProgressMessage` — 子任务进度（含 summary、usage）
3. `SDKTaskNotificationMessage` — 子任务完成/失败/停止

## 九、MCP 集成 — 4 种服务器类型

```typescript
type McpServerConfig = 
    | McpStdioServerConfig            // { command, args, env }  — 本地进程
    | McpSSEServerConfig              // { type: 'sse', url }    — SSE 远程
    | McpHttpServerConfig             // { type: 'http', url }   — HTTP 远程
    | McpSdkServerConfigWithInstance; // { type: 'sdk', instance } — 同进程
```

SDK MCP 服务器可通过 `createSdkMcpServer()` 在宿主进程内创建：

```typescript
import { createSdkMcpServer, tool, query } from '@anthropic-ai/claude-agent-sdk';
import { z } from 'zod/v4';

const server = createSdkMcpServer({
    name: 'my-tools',
    tools: [
        tool('greet', 'Say hello', { name: z.string() }, async ({ name }) => ({
            content: [{ type: 'text', text: `Hello ${name}!` }]
        }))
    ]
});

const q = query({ prompt: '...', options: { mcpServers: { 'my-tools': server } } });
```

## 十、会话持久化

会话存储在 `~/.claude/projects/{dir}/{sessionId}.jsonl`，SDK 提供完整的 CRUD：

| 函数 | 功能 |
|------|------|
| `listSessions(options?)` | 列出会话（支持分页、按目录过滤、worktree） |
| `getSessionInfo(sessionId, options?)` | 获取单个会话元数据 |
| `getSessionMessages(sessionId, options?)` | 读取会话消息历史 |
| `listSubagents(sessionId, options?)` | 列出子 Agent ID |
| `getSubagentMessages(sessionId, agentId, options?)` | 读取子 Agent 消息 |
| `renameSession(sessionId, title, options?)` | 重命名 |
| `tagSession(sessionId, tag, options?)` | 打标签（null 清除） |
| `forkSession(sessionId, options?)` | 分叉（复制+重映射 UUID） |

### SDKSessionInfo
```typescript
type SDKSessionInfo = {
    sessionId: string;
    summary: string;
    lastModified: number;
    fileSize?: number;
    customTitle?: string;
    firstPrompt?: string;
    gitBranch?: string;
    cwd?: string;
    tag?: string;
    createdAt?: number;
};
```

## 十一、Options 完整参考

`query()` 的 `options` 参数支持以下配置：

| 选项 | 类型 | 说明 |
|------|------|------|
| `abortController` | AbortController | 取消控制器 |
| `additionalDirectories` | string[] | 额外可访问目录 |
| `agent` | string | 主线程 agent 名称 |
| `agents` | Record<string, AgentDefinition> | 自定义子 agent |
| `allowedTools` | string[] | 自动允许的工具 |
| `canUseTool` | CanUseTool | 权限回调 |
| `continue` | boolean | 继续最近会话 |
| `cwd` | string | 工作目录 |
| `disallowedTools` | string[] | 禁用的工具 |
| `tools` | string[] 或 preset | 可用工具集 |
| `env` | Record<string, string> | 环境变量 |
| `executable` | 'bun'/'deno'/'node' | JS 运行时 |
| `enableFileCheckpointing` | boolean | 文件检查点（支持 rewind） |
| `hooks` | Partial<Record<HookEvent, ...>> | Hook 回调 |
| `onElicitation` | OnElicitation | MCP 表单回调 |
| `includePartialMessages` | boolean | 流式 token 事件 |
| `includeHookEvents` | boolean | Hook 生命周期事件 |
| `maxThinkingTokens` | number | 思考 token 上限（已废弃） |
| `thinking` | ThinkingConfig | 思考控制 |
| `effort` | EffortLevel | 推理努力级别 |
| `maxTurns` | number | 最大对话轮次 |
| `maxBudgetUsd` | number | 最大预算（美元） |
| `mcpServers` | Record<string, McpServerConfig> | MCP 服务器 |
| `model` | string | 模型选择 |
| `outputFormat` | OutputFormat | 结构化输出 |
| `permissionMode` | PermissionMode | 权限模式 |
| `persistSession` | boolean | 是否持久化会话（默认 true） |
| `resume` | string | 恢复指定会话 |
| `sessionId` | string | 指定会话 ID |
| `sandbox` | SandboxSettings | 沙箱隔离 |
| `settings` | string 或 Settings | 设置覆盖 |
| `settingSources` | SettingSource[] | 加载哪些文件设置 |
| `systemPrompt` | string 或 preset | 系统提示词 |
| `spawnClaudeCodeProcess` | function | 自定义进程启动 |
| `plugins` | SdkPluginConfig[] | 插件配置 |
| `promptSuggestions` | boolean | 下轮提示建议 |
| `agentProgressSummaries` | boolean | 子 agent 进度摘要 |
| `betas` | SdkBeta[] | Beta 功能 |
| `forkSession` | boolean | resume 时分叉 |
| `debug` / `debugFile` | boolean/string | 调试模式 |
| `stderr` | function | stderr 回调 |

## 十二、Transport 抽象

```typescript
interface Transport {
    write(data: string): void | Promise<void>;
    close(): void;
    isReady(): boolean;
    readMessages(): AsyncGenerator<StdoutMessage, void, unknown>;
    endInput(): void;
}
```

三种实现：
1. **ProcessTransport** — spawn 子进程，stdin/stdout 通信（默认）
2. **WebSocketTransport** — 浏览器端 WebSocket 连接
3. **SSETransport** — Bridge 模式，SSE 读 + HTTP POST 写

## 十三、内置工具列表 (sdk-tools.d.ts)

| 工具 | 输入类型 | 说明 |
|------|----------|------|
| Agent | AgentInput | 子 agent 调用 |
| Bash | BashInput | Shell 命令执行 |
| FileEdit | FileEditInput | 文件编辑 |
| FileRead | FileReadInput | 文件读取（文本/图片/PDF/Notebook） |
| FileWrite | FileWriteInput | 文件写入 |
| Glob | GlobInput | 文件模式匹配 |
| Grep | GrepInput | 内容搜索 |
| WebFetch | WebFetchInput | 网页获取 |
| WebSearch | WebSearchInput | 网页搜索 |
| TodoWrite | TodoWriteInput | 任务列表管理 |
| AskUserQuestion | AskUserQuestionInput | 用户交互 |
| NotebookEdit | NotebookEditInput | Notebook 编辑 |
| McpInput | McpInput | MCP 工具调用 |
| Config | ConfigInput | 配置管理 |
| EnterWorktree / ExitWorktree | | Git Worktree 管理 |
| ExitPlanMode | ExitPlanModeInput | 退出计划模式 |
| TaskOutput / TaskStop | | 任务输出/停止 |
| ListMcpResources / ReadMcpResource | | MCP 资源管理 |

## 十四、完整数据流（一次 query 的生命周期）

```
1. 调用 query({ prompt: "Fix the bug", options: {...} })

2. SDK spawn cli.js 子进程，通过 stdin 写入:
   +-- SDKControlRequest { subtype: 'initialize', hooks, agents, ... }
   +-- SDKUserMessage { type: 'user', message: { role: 'user', content: '...' } }

3. CLI 初始化，返回:
   +-- SDKSystemMessage { type: 'system', subtype: 'init', tools, model, ... }

4. CLI 调用 Claude API -> 流式接收:
   +-- SDKPartialAssistantMessage { type: 'stream_event', event: ... } (多条)

5. 模型决定使用工具 -> CLI 发送权限请求:
   +-- SDKControlRequest { subtype: 'can_use_tool', tool_name: 'Bash', input: {...} }

6. SDK 调用 canUseTool 回调 -> 返回:
   +-- SDKControlResponse { response: { behavior: 'allow' } }

7. 工具执行中:
   +-- SDKToolProgressMessage { tool_name: 'Bash', elapsed_time_seconds: 5 }

8. 完整助手回复:
   +-- SDKAssistantMessage { type: 'assistant', message: BetaMessage{...} }

9. 可能多轮工具调用 (重复 4-8)

10. 最终结果:
    +-- SDKResultSuccess {
          type: 'result', subtype: 'success',
          result: "...", total_cost_usd: 0.05,
          usage: { input_tokens, output_tokens, ... }
        }
```

## 十五、与 RCUI 的关联

RCUI 项目的 Claude provider (`providers/claude/`) 需要读取 `~/.claude/projects/` 目录下的 JSONL 文件来展示会话历史。这些文件的格式与 SDK 的 `SDKMessage` 类型体系完全一致。

关键对应关系：
- RCUI 的 `NormalizedMessage` <-> SDK 的 `SDKAssistantMessage` / `SDKUserMessage`
- RCUI 的 session 列表 <-> SDK 的 `listSessions()` 返回的 `SDKSessionInfo`
- RCUI 的消息历史 <-> SDK 的 `getSessionMessages()` 返回的 `SessionMessage[]`
- RCUI 的 WebSocket chat <-> SDK 的 `query()` 流式输出

如果后续需要从"读取 JSONL 文件"升级为"通过 SDK 交互"，可以参考本文档中的 V1/V2 API 和 Transport 抽象。

## 十六、设计总结

1. **进程隔离** — SDK 是薄包装，核心逻辑在 13MB 的 `cli.js` 中，通过 IPC 通信
2. **流式优先** — 所有交互基于 `AsyncGenerator<SDKMessage>` 流
3. **双向控制** — 不只是"发请求-收回复"，还有实时的控制请求通道（权限、中断、配置变更）
4. **三种 Transport** — 进程 stdio（默认）、WebSocket（浏览器）、SSE Bridge（远程 CCR）
5. **Hook + Permission 扩展** — 27 种 Hook 事件 + `canUseTool` 回调提供了完整的行为定制能力
6. **会话即文件** — JSONL 格式持久化，支持 fork、resume、rewind
