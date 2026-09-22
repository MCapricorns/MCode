# Changelog

显著变化从当前版本 `0.4.0` 记起。更早的发布记录已作废，不再保留。日期为发布日（UTC）。

## [0.4.0] - 2026-09-22

### Added

- Desk 三栏：项目侧栏、对话时间线、右侧 inspector。
- 模型目录来自 models.dev 快照；`anthropic-messages`、`openai-completions`、`openai-responses` 三种 wire。
- 内置工具、MCP、skills，以及 `task` 委派 scout / artisan / steward / sentinel。
- 应用内文件夹选择：先列盘符，再列该盘下的文件夹和文件；把文件夹拖到窗口上即可打开。
- 模型菜单的 Suggested 区优先列出 o3、gpt-5、opus 这类高强度模型。

### Changed

- 发布目标是 Windows x64 与 macOS Apple Silicon。不再构建 macOS Intel 产物。
- 会话 actor 跑在调用方 runtime 上，阻塞存储走 `spawn_blocking`，不再单独起线程。
- skills、MCP 工具和 subagent 写进每轮系统提示，模型不必等用户点名才使用。

### Fixed

- WindowsApps 里的 `pwsh` 执行别名可以被发现并选中。
- 最近项目的删除会立刻从列表消失，相近路径不再抢同一个点击目标。
- `edit` 的结果按 diff 预览增删行；`grep` / `find` 的命中逐行展开，不再收成一块截断文本。

[0.4.0]: https://github.com/MCapricorns/MCode/releases/tag/v0.4.0
