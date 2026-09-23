# Changelog

显著变化从 `0.4.0` 记起。更早的发布记录已作废，不再保留。日期为发布日（UTC）。

## [0.4.5] - 2026-09-23

### Changed

- 窗口启动时落在主屏幕可用区域的正中。屏幕比默认尺寸小的时候，窗口会先缩小再居中。
- 模型菜单和思考菜单从右侧按钮旁边打开。
- 已经保存的网页搜索密钥不再显示在输入框里，只留一把锁。点锁可以换一把新钥匙，旧密钥不会被填回来。
- MCP 的添加收进二级页：从目录添加、导入 JSON、自定义服务器。已保存的密钥同样只显示锁。
- 思考按钮写出 Thinking / Thinking off / Thinking on，不再只写 On。

## [0.4.4] - 2026-09-23

### Changed

- 窗口标题是 MYCode Harness，前面放程序图标。
- 一个工作区里的多个文件夹属于同一个项目。选中的文件夹是当前会话的工作目录，其余文件夹仍然可用。
- 右侧重新显示子代理进度。提问和 todos 都在输入框上方。每个正在跑的子代理都会写出自己的当前步骤。
- 提示按工具、skills、MCP、子代理四段编排。同一轮里的 `task` 和 MCP 一起执行。
- 配色增加 Rose、Ink、Moss。浅色背景渐变拉开，不再只是换一块近白底。

### Fixed

- Windows 上读取 git 状态和 diff 不再弹出控制台黑框。
- 停止会取消正在跑的子代理，发送和窗口关闭不再被进度窗挡住。

## [0.4.3] - 2026-09-22

### Changed

- 窗口改回 MYCode。浅色和深色不再是同一套黄色，设置里可以换配色。面板是实色，不是半透明。
- 左侧是工作区：一个工作区可以挂多个文件夹，会话挂在对应文件夹下面。
- 模型和思考强度在输入框旁直接选。子代理模型也一样。
- 右侧是当前会话的模型和这次改动的 git 状态，不再放一排静态数据。待办留在对话上方。
- 工具行写出目标。`read` 显示路径，`grep` 显示模式，`shell` 显示命令。子代理进度也带目标。
- 子代理可以使用已连接的 MCP 和 skills。MCP 仍然先 `search_tool` 再 `use_tool`。

## [0.4.2] - 2026-09-22

### Changed

- 窗口改称 MYCode Harness。整窗是同一套半透明磨砂，标题栏的 DAY/NIGHT 可以直接切换。
- 去掉 TAPE 条和不会消失的整行错误横幅。提示改到右下角，三秒后消失。
- About 里可以手动检查更新，并写上作者 MaMy 与感谢名单。
- 一个会话只属于一个文件夹。切换项目或拖入另一个文件夹时，正在干活的会话留在原目录，新文件夹用自己的会话。

### Fixed

- Windows 上 `exec` / `shell` 的输出在控制台代码页不是系统 ANSI/OEM 页时，仍按 OEM 再 ANSI 解码，中文不再变成替换字符。
- 对话栏铺满中间列，随窗口缩放，不再留出两侧空白。

## [0.4.1] - 2026-09-22

### Changed

- Skills 只给短索引。任务对上了就用 `read` 读 `SKILL.md`，不再把正文写进每轮提示。
- MCP 改为 Grok Build 的调用方式：先 `search_tool` 取 schema，再 `use_tool`。参数 schema 不再每轮内联。
- 当前网页事实用 `web_search`，引用前再用 `fetch_content`。

### Fixed

- 切换项目只过滤侧栏，不再把所有会话绑进 This Project。切到另一个会话时，工作目录和 This Project 跟着那个会话的项目走。
- Inspector 用量跟随当前模型，任务进行中就刷新，不再等整轮结束。网关把 `input_tokens` 报成字符串时也不再显示 in=0。
- 模型和思考强度在同一个面板里选，不再拆成三个下拉。
- 子代理不再因为整段 SSE 累计超限而失败。
- 会话头落后时跟上真实 tip，不再提示 “the session moved on, reopen it”。
- 切出当前项目后，TASKS 和子代理界面会隐藏。Inspector 里的子代理可以点开进度小窗。
- 对话栏铺满可用宽度；排队消息说明发送时机，并可打断后立即发送。

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

[0.4.2]: https://github.com/MCapricorns/MCode/releases/tag/v0.4.2
[0.4.1]: https://github.com/MCapricorns/MCode/releases/tag/v0.4.1
[0.4.0]: https://github.com/MCapricorns/MCode/releases/tag/v0.4.0
