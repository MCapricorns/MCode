# MCode

MCode 是一个基于 Zed GPUI 的 Windows 桌面编码 agent（Cursor/Codex 桌面版式三栏布局），围绕一个持久、可回放的会话 ledger 与第一方 provider runtime 构建。

## 功能

- **桌面工作区**：左侧会话侧栏（新建/刷新/切换）、中间对话流（流式回复、思考、工具条目）、右侧上下文面板（Overview / Web 搜索 / Changes / Settings）。
- **Provider runtime**：`anthropic-messages`、`openai-completions`、`openai-responses` 三个通用 wire 协议——新增厂商只加配置。流式输出、thinking 签名保真回放、工具调用。
- **内置工具**：`read`、`write`、`edit`、`shell`、`exec`、`grep`、`find`，外加 `ask_user`（结构化问答，cancel-safe）与 `todo_write`（stable ID + 依赖图 + durable 事件）。
- **durable 会话**：所有用户消息、工具结果、assistant 回复落事件 ledger；崩溃后可恢复、回放。
- **Workspace checkpoint**：`write`/`edit` 改动文件前自动快照，Changes 面板一键回滚到会话前状态。
- **可视化设置**：Provider（含 API key，存独立 secrets 文档）、搜索后端、MCP servers（Context7 内置目录、自填 key）、User-Agent（默认取 pi agent 真实 UA）、浅色/深色主题。
- **MCP 客户端**：stdio 与 Streamable-HTTP 双 transport，`List tools` 查看各 server 工具。
- **Web 搜索**：bounded 客户端（URL/SSRF 防护、响应大小上限），结果展示在 Web 面板。

## 构建

```text
cargo build --release -p mcode-desktop
target/release/mcode-desktop.exe
```

工具链：Rust stable（MSVC）。门禁：`cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`。

## 数据位置

`MCODE_HOME`（默认 `~/.mcode`）下：`settings.json`（配置）、`secrets.json`（API keys，Debug 输出打码）、`sessions/`（durable ledger）、`checkpoints/`（文件快照）、`workspace/<session>/`（工具工作目录 + todos.json + AGENTS.md/MCODE.md 资源）。

## 文档

- [plan.md](plan.md) — 路线图与交付状态
- [docs/design/](docs/design/README.md) — 架构与契约
- [docs/research/2026-09-18-minimax-code-notes.md](docs/research/2026-09-18-minimax-code-notes.md) — MiniMax Code（pi 衍生）架构学习笔记

## License

Apache-2.0
