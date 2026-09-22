# MYCode 设计文档

本目录描述当前 7-crate Desk 产品的实现契约。没有旧名兼容、没有 Pack/WASM 插件体系、没有 `mycode-web` / `mycode-mcp` 独立 crate。

## 阅读顺序

| 文档 | 内容 |
| --- | --- |
| [00-architecture.md](00-architecture.md) | 产品形态、crate 拓扑、home 布局 |
| [01-agent-core.md](01-agent-core.md) | Agent loop、ledger、自动压缩 |
| [02-tools-permissions.md](02-tools-permissions.md) | 工具 registry、subagent / MCP / web |
| [09-desktop-ui.md](09-desktop-ui.md) | Desk 布局、桥接、设置与欢迎页 |
