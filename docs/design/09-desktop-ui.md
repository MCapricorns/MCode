# 桌面 UI 实施契约

> 本文约束 MYCode 桌面应用（`mycode-desktop`）：唯一产品前端。TUI、headless CLI 与第三方插件系统已从产品目标中移除；Provider/Web/MCP/Usage 均以第一方内置、数据驱动（settings 文档）的方式接入。UI 层不直接触碰文件、secrets 或网络——一切持久化、网络与运行时操作经 core 桥接线程完成（唯一例外：自更新的换装重启属进程机制，见 `mycode-updates`）。

## 1. 技术栈与窗口结构

- UI 框架：`gpui-kit 0.6`（umbrella crate，传递 `gpui-pre 0.3.5`）。**禁止**再直接依赖 `gpui` crate——双版本会产生跨 crate 类型冲突；所有类型经 `gpui_kit::` 与 `gpui_kit::component::` 引入。
- 图标：`gpui_kit::assets::IconName`（Lucide 全目录），资产源在 `main.rs` 以 `AllAssets` 注册。
- 布局（Zed/Codex 桌面式）：
  - 最左活动栏（46px）：顶部落款块 + Chat/Settings 两个视图图标；底部主题切换。
  - Chat 视图：会话侧栏（232px，New + 会话行）+ 中央区（顶栏 project chip 与 model 选择器、对话流、composer 卡片）+ 右侧上下文面板（Overview / Web / Changes 三个 tab）。
  - Settings 视图：整页设置（Appearance / Model providers / Web search / MCP servers / Usage / Updates & catalog），分节卡片滚动。
  - 无会话且无项目时中央显示欢迎页：Open project folder（系统目录选择器）、Just start chatting、最近项目列表。
- 主题：仅内置浅色/深色，经 `Theme::change(ThemeMode)` 切换；状态由 view model 的 `dark_theme` 镜像。
- 目标平台：Windows 与 macOS；不支持 Linux。

## 2. 线程与桥接模型

GPUI executor 与 tokio 不兼容，因此 `CoreBridge`（`bridge/mod.rs`）在专属 std 线程上运行一个 current-thread tokio `Runtime`，承载 `SessionService`、目录刷新、UI 状态与更新检查。`bridge/` 按职责拆分：`mod.rs`（命令/事件/回复类型、命令循环、`CoreState`）、`turn.rs`（模型回合与 ledger 泵）、`sessions.rs`（ledger 读取与展示投影）、`settings.rs`、`copilot.rs`、`mcp.rs`（MCP 连接池与工具适配）、`web.rs`、`hosts.rs`（ask/todo/task 宿主）、`compaction.rs`、`projects.rs`；子模块仅通过 `pub(super)` 项互相引用。

```text
UI (GPUI main thread)
  -> SyncSender<WithReply> (bounded 256)
  -> core thread (current-thread tokio): SessionService / catalog refresh / updates / owned-file IO
  -> oneshot::Sender<BridgeReply> + SyncSender<BridgeEvent> (bounded 512, 流式事件)
  -> UI 侧 await oneshot / 事件泵 (50ms) -> apply_reply/apply_event -> reduce -> cx.notify()
```

- `CoreState` 以 `Arc` 共享；`SessionService` 的克隆共享同一 inner（actor + generation fence），仅最后一个克隆 drop 时才 retire publication 并中止 actor worker，因此 per-turn 任务（`HeadWriter`、`RecallMessage` 等）可以安全持有克隆。
- 长任务（`RefreshCatalog`、`CheckUpdate`、`DownloadUpdate`、`ChatTurn`）在 core 线程 spawn 为并发任务，reply 经 oneshot 返回；流式事件与目录/更新通知走 `BridgeEvent`（`CatalogUpdated`、`UpdateAvailable` 等）。
- `CoreState` 持有 per-session 项目目录映射；`SetProjectDir` 校验目录存在后绑定，模型回合的工具 cwd、资源发现均使用项目目录（缺省回退 `workspace/<session>`）。
- `CoreState.mcp` 是进程级 MCP 连接池（`McpPool`）：每个启用的 server 只连接一次并跨回合复用；配置或 key 变化时重连，传输层故障后标记失效并在下一回合重建；从 settings 移除或禁用的 server 在回合开始时优雅关闭（先关 stdin 再 kill）。连接失败不会让回合失败，而是经 `BridgeEvent::Notice` 提示到 UI。stdio 传输在 Windows 上按 `PATHEXT` 解析 `npx`/`uvx` 这类 `.cmd` 启动器，并以 `CREATE_NO_WINDOW` 启动，stderr 尾部随错误信息一并上报。
- ledger 泵按顺序提交回合中的每条 assistant 消息：发出工具调用的中间步骤在其 ToolCall/ToolResult 事件之前提交（`BridgeEvent::AssistantStep`），最终文本回复在回合结束时提交并连同整回合累计的 usage 写入。`ledger_history` 回放时修复 tool_use/tool_result 配对（丢弃孤儿结果、剔除无结果的调用块），旧 ledger 因此不再被 provider 以 "tool call not found" 拒绝。
- UI 侧不持有任何 tokio handle；core 线程退出时所有 pending 请求立即收到错误 reply。

## 3. View model 纯函数化

`view_model.rs` 是纯状态 + 纯 reducer：`WorkspaceState`（sessions、active、composer、settings、dark_theme、error、view、catalog、project_dir、recents、auto_update、update、selected_provider/model、preset 表单状态等）+ `DesktopAction` + `reduce()`。规则：

- reducer 内不做 IO、不读文件、不触网络；所有副作用都在 `Workspace` 方法里先构造 action 再 reduce。
- 模型选择在 settings/UI 状态变化后由 `ensure_model_selection` 回退到第一个可用 enabled provider/model，并经 `SaveUiState` 持久化。
- 自更新状态机：`Idle -> Checking -> UpToDate|Available -> Downloading -> Ready -> (重启安装)`，失败进入 `Failed`；`last_offer` 保存可下载的 release offer。
- composer 输入上限 `MAX_COMPOSER_CHARS = 64 KiB`（字符级截断）；流式缓冲上限 `MAX_STREAMING_CHARS`。

## 4. Provider 目录（云控自动更新）

`mycode-providers` 对齐 pi 的模型数据策略：

- 内置快照：`scripts/generate_catalog.py` 从 models.dev `api.json` 生成归一化快照（192 家可用厂商、全部三家 wire 协议映射、排除云控制台类厂商），编译期嵌入二进制作离线兜底。
- 云同步：core 线程启动时后台 `refresh`（HTTP ETag 条件请求，6h 新鲜度窗口，32MB 上限），成功后 CAS 写 `catalog-cache.json` 并发 `CatalogUpdated`；失败静默保留旧目录。设置页可手动 Refresh。
- 设置页 "Add from catalog"：按名称/ID 过滤厂商 → 选默认模型（目录列表）→ 粘贴 API key → 一键生成 provider 条目并存 key、保存 settings。

## 5. 可视化设置页

设置页编辑 `settings.json` 的全部内容，用户不需要手改文件：

- Appearance：浅色/深色分段按钮；User-Agent 输入（空值 = pi agent 默认 UA），常显 effective 值。
- Model providers：已配置行（名称、协议、host、模型数、key 状态图标、On/Off、Remove）+ 目录预设流 + 自定义 endpoint 表单；保存走 `SaveSettings` 的 revision CAS。
- Web search / Usage：行式列表与内联表单，行为与 T13/T14 契约一致。
- MCP servers：每行显示 id、传输、目标（endpoint 或完整命令行）、key 状态与状态灯；`Probe` 按编辑器当前配置（未保存亦可）经连接池连接并把工具名以 chip 列出；stdio 表单接受整行命令（`npx -y @scope/server --flag "a b"`，按 shell 规则切分为 program + args）与可选 `KEY=VALUE` 环境变量；新加的 server 默认启用。
- 二级菜单（左侧设置导航）：Desk 风格 `SETTINGS` pane head，按 WORKSPACE / CONNECT / SYSTEM 分组，每行含序号、图标、标签与提示，右侧为实时计数（启用的 provider / server / backend）或状态灯（缺 key、可用更新），页脚显示版本与未保存标记。
- Updates & catalog：当前版本、手动 Check now、自动检查开关（持久化在 ui-state）、下载安装/重启按钮、目录来源与刷新时间。

## 6. 会话流与项目

- 侧栏 New -> `CreateSession` -> 自动 `OpenSession`；对话条目按 `EntryKind` 渲染为气泡/工具行/usage 行。
- 项目选择：欢迎页或顶栏 chip 触发系统目录选择器（`prompt_for_paths`），绑定到当前会话（无会话则先创建）；最近项目持久化到 `ui.json`（`mycode-config::ui_state`，owned-file 事务写入）。
- 发送后 `sending` 置位、composer 清空并禁用，直到 `Sent` reply 到达；流式回复、工具调用、usage 记录实时追加。

## 7. 验证门禁

- `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked` 全绿。
- 单元测试覆盖：reducer 全 action 分支（settings 往返、模型选择回退、更新状态机）、bridge 命令/reply 配对（noop-waker 轮询 oneshot）、目录解析/缓存/新鲜度、更新校验和与平台资产匹配。
- 桌面窗口本身需 GPU，CI 只验证编译与单元测试；人工冒烟（开窗、选项目、目录添加 provider、流式对话、检查更新）在 Windows 本机执行。

## 8. 已知问题：Windows 显示缩放变更后的窗口裁切

症状：系统显示缩放从一档改为另一档（例如 125% → 150%）后启动应用，窗口物理尺寸停留在未缩放的大小（1280×840 物理），但渲染层按新 scale（1.5）绘制 1280×840 逻辑场景（1920×1260 物理），右下约 1/3 的 UI 被裁出屏幕外；composer 与侧栏 footer 不可见。键鼠命中测试与渲染使用同一 scale，仍互相对齐，已显示部分完全可交互。

诊断（2026-09，gpui-kit 0.6 / gpui 0.2.2 / Windows 11 150%）：进程为 PerMonitorV2 感知（exe 清单由 gpui-pre 资源提供），`GetDpiForWindow` 返回 144；`window.scale_factor()` 为 1.5、`viewport_size()` 为 1280×840 逻辑，但窗口创建路径 `retrieve_window_placement` 的 `bounds.to_device_pixels(scale)` 未把物理窗口放大到 1920×1260。属上游 gpui Windows 平台在缩放变更后创建窗口的缺陷。

已尝试并否决的应用层绕过（不要重试）：

- 打开后 `window.resize(logical × scale_factor)`：该构建的 `resize` 以传入值为物理像素执行，物理窗口与布局对齐、UI 完整显示，但随后客户端区鼠标输入非确定性失效（WM_NCHITTEST 的标题栏按钮仍可用、键盘可用）。
- 按 `GetClientRect` 实测与 `viewport × scale` 的比值压缩根布局：聊天视图可接受，但设置页 `mx_auto + max_w(rems)` 列几何被推挤溢出右缘。

处置：等待 gpui-kit / gpui 升级修复后验证关闭；期间建议用户以 100%/125% 缩放运行，或在系统缩放变更后注销重登再启动应用。
