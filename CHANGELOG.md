# Changelog

各版本显著变化。日期为发布日（UTC）。

## [0.4.0] - 2026-09-22

### Changed

- 发布目标收敛为 Windows x64 与 macOS Apple Silicon（arm64）；不再构建 macOS Intel（`x86_64-apple-darwin`）产物，Intel 客户端的更新检查将不再匹配到资产。
- 助手回复气泡改用 markdown 渲染：代码块、链接与行内代码样式跟随主题。

### Fixed

- 修复 v0.3.0 之后主干的 Windows 编译失败（`mycode-tools` 内 `runtime_shell` 导入被误删，仅影响 Windows 目标）。
- 修复 macOS 构建的 `unix_clear_errno` 可见性错误——v0.3.0 因此只挂出了 Windows 资产；本版 macOS Apple Silicon 包恢复发布。

## [0.3.0] - 2026-09-21

### Added

- Desk 三栏布局：项目侧栏 + 对话时间线 + 右侧 inspector，frost/glass 视觉主题。
- GitHub Copilot 接入：device-flow OAuth 登录，每轮请求换取 bearer。
- MCP 工具进入会话执行；Querit web 搜索后端（`web_search` + `fetch_content`）。
- adaptive thinking、live jobs（工具执行实时上屏）、自动压缩（context 95% 窗口的 90% 触发，保留尾部约 20k token）。
- 长会话记录折叠，保持聊天 UI 响应。

### Fixed

- Windows shell 检测与队列发送、项目会话绑定等一系列桌面问题。
- pwsh 控制台输出固定为 UTF-8，避免区域设置乱码。
- 流式文本里的 XML tool-call 标记解析。
- 会话可恢复性与后台 worker 保活；桌面状态变更统一走 reducer。

### Notes

- crate 更名为 `mycode-*`，发布资产改名 `mycode-desktop-*`（保留 `mcode-desktop-*` 别名供旧客户端更新）。
- 本版 Release 因 macOS 编译错误仅含 Windows 资产；macOS 包在 0.4.0 恢复。

## [0.2.0] - 2026-09-19

### Added

- opencode 式布局：项目侧栏与会话标题。
- Subagent：`task` 工具委派子代理。
- 会话压缩与原子 checkpoint；多模型目录预设；产品数据导出/导入。
- 自定义标题栏与应用图标。

### Fixed

- darwin exec 路径与私有锁权限加固；核心运行时卡顿修复。

## [0.1.0] - 2026-09-19

### Added

- models.dev 提供商目录（190+ 厂商）与云同步。
- GitHub Release 自更新：SHA-256 校验、暂存、重启换装。
- 桌面 UI 状态持久化；项目选择器与模型选择器。

## [0.0.1] - 2026-09-18

首个开发者预览：GPUI 桌面工作区（会话侧栏、流式对话、上下文面板）、durable 会话 ledger、三种 wire 协议接入（anthropic-messages / openai-completions / openai-responses）、内置工具（read/write/edit/shell/exec/grep/find、ask_user、todo_write）、写前 checkpoint 与一键回滚、MCP 客户端（stdio 与 Streamable-HTTP）、可视化设置。

---

发布对比：

- [0.3.0...0.4.0](https://github.com/MCapricorns/MCode/compare/v0.3.0...v0.4.0)
- [0.2.0...0.3.0](https://github.com/MCapricorns/MCode/compare/v0.2.0...v0.3.0)
- [0.1.0...0.2.0](https://github.com/MCapricorns/MCode/compare/v0.1.0...v0.2.0)
- [0.0.1...0.1.0](https://github.com/MCapricorns/MCode/compare/v0.0.1...v0.1.0)
