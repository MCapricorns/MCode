# MYCode 架构

> 本文描述当前实现的产品架构；历史 Manager/Pack/WASM 插件体系已从产品目标中移除。

## 1. 产品形态

MYCode 是一个 GPUI 桌面应用（Windows 优先），没有 TUI/CLI/headless 前端。一切配置（Provider、搜索、MCP、User-Agent、主题）都在应用内的可视化设置页完成；密钥永不进入设置文件。

```text
mycode-desktop (GPUI, Windows)
  ├─ 纯 view-model reducer（无 GPU 依赖、可测）
  ├─ 渲染层：三栏布局（会话侧栏 / 对话流 / 上下文面板）
  └─ CoreBridge：一条专用线程 + current-thread tokio runtime
       ├─ mycode-agent   持久会话 ledger（分支/恢复/回退、expected-head CAS）
       ├─ mycode-providers 三个 wire 协议适配器 + 可注入 SSE transport
       ├─ mycode-agent     双循环 agent（流式 → 工具 → 停止/steer）
       ├─ mycode-tools     内置工具 + ask_user/todo_write 交互
       ├─ mycode-web       bounded 搜索（URL/SSRF 防护）
       ├─ mycode-mcp       stdio + Streamable-HTTP 客户端
       └─ mycode-config    settings/secrets/todos/checkpoints/resources 严格文档
```

## 2. 关键边界

- **UI 不触碰文件、密钥、网络**：一切经 `BridgeCommand`/`BridgeReply`（oneshot）与 `BridgeEvent`（有界流通道）进出 core 线程。
- **一切持久化走严格文档事务**：`settings.json`、`secrets.json`、`todos.json`、session manifest/branch log 都用 owned-file 事务 + revision CAS。
- **模型回合 = durable agent turn**：用户消息、工具结果、assistant 消息按事件落 ledger；`write`/`edit` 前自动 checkpoint（`~/.mycode/checkpoints/<session>/`），可一键回滚到会话前状态。
- **Provider = 数据不是代码**：`kind` 是三个通用 wire 协议（`anthropic-messages`/`openai-completions`/`openai-responses`），新增厂商只加配置。密钥存 `secrets.json`（Debug 输出只列 id 不泄密钥）。
- **网络出口宿主独占**：所有 egress 经注入 transport seam；User-Agent 默认取 pi agent 的真实 UA，可配置。
- **内置工具**：`read`、`write`、`edit`、`shell`、`exec`、`grep`、`find`，外加交互型 `ask_user`（结构化问答，cancel-safe）与 `todo_write`（stable ID、依赖图、durable Task 事件）。Windows shell 走 PowerShell 7 `-EncodedCommand`（UTF-16LE），输出按控制台代码页解码回退，中文环境不乱码。
- **MCP**：stdio 与 Streamable-HTTP 双 transport；HTTP 支持 bearer/`x-api-key` 凭证头与 `Mcp-Session-Id`；Context7 在内置目录中，key 由用户输入并存 secrets。

## 3. 相关文档

| 文档 | 内容 |
| --- | --- |
| [01-agent-core.md](01-agent-core.md) | Agent loop、turn 模型、steer/follow-up、事件契约 |
| [02-tools-permissions.md](02-tools-permissions.md) | canonical tools 与安全契约 |
| [09-desktop-ui.md](09-desktop-ui.md) | 桌面布局、桥接模型、设置页结构 |
