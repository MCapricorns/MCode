# 桌面 UI 实施契约

> 本文约束 MCode 桌面应用（`mcode-desktop`）：唯一产品前端。TUI、headless CLI 与第三方插件系统已从产品目标中移除；Provider/Web/MCP/Usage 后续均以第一方内置、数据驱动（settings 文档）的方式接入。UI 层不直接触碰文件、secrets 或网络——一切持久化与运行时操作经 core 桥接线程完成。

## 1. 技术栈与窗口结构

- UI 框架：`gpui-kit 0.6`（umbrella crate，传递 `gpui-pre 0.3.5`）。**禁止**再直接依赖 `gpui` crate——双版本会产生跨 crate 类型冲突；所有类型经 `gpui_kit::` 与 `gpui_kit::component::` 引入。
- 布局：Cursor/Codex 桌面版三栏——左侧会话侧栏（Sessions + New/Refresh）、中间对话流 + 底部 composer、右侧上下文面板（Overview / Settings 两个 tab），顶部 TitleBar（主题切换 + 错误横幅在标题栏下方）。
- 主题：仅内置浅色/深色，经 `Theme::change(ThemeMode)` 切换；状态由 view model 的 `dark_theme` 镜像。
- 目标平台：Windows 与 macOS；不支持 Linux。

## 2. 线程与桥接模型

GPUI executor 与 tokio 不兼容，因此 `CoreBridge`（`bridge.rs`）在专属 std 线程上运行一个 current-thread tokio `Runtime`，承载 `SessionService` 与全部 settings 读写：

```text
UI (GPUI main thread)
  -> SyncSender<WithReply> (bounded 256)
  -> core thread (current-thread tokio): SessionService / mcode-config owned-file IO
  -> oneshot::Sender<BridgeReply>
  -> UI 侧 await oneshot (executor-agnostic) -> apply_reply -> reduce -> cx.notify()
```

- `BridgeCommand` 当前为 `ListSessions | CreateSession | OpenSession | SendMessage | LoadSettings | SaveSettings`；每条 reply 内嵌 `Result<T, String>`，错误统一浮出到横幅。
- UI 侧不持有任何 tokio handle；core 线程退出时（runtime 建立失败）所有 pending 请求立即收到错误 reply。
- `SendMessage` 携带 `expected_head: HeadStamp`，core 侧 CAS 失败以错误字符串返回；UI 不自行重试。

## 3. View model 纯函数化

`view_model.rs` 是纯状态 + 纯 reducer：`WorkspaceState`（sessions、active、composer_draft、sending、context_tab、settings、dark_theme、error）+ `DesktopAction` + `reduce()`。规则：

- reducer 内不做 IO、不读文件、不触网络；所有副作用都在 `Workspace` 方法里先构造 action 再 reduce。
- composer 输入上限 `MAX_COMPOSER_CHARS = 64 KiB`（字符级截断）。
- `SettingsState` 是 `AppSettings` 的可编辑投影：`from_settings` / `to_settings` 往返；任何编辑置 `dirty`，保存成功以新 revision 回写并清除 `dirty`。
- 会话列表刷新保留 `active` 标记（按 session id 匹配）。

## 4. 可视化设置页

设置 tab 编辑 `settings.json` 的全部内容，用户不需要手改文件：

- User-Agent：文本输入，空值 = `default_user_agent()`（与 pi agent 0.85.1 的 `pi (<platform> <release>; <arch>)` 逐字符一致）；下方常显 effective 值。
- Providers：行式列表（id · kind）+ Remove；内联 Add provider 表单（id / kind / base URL / model），新增默认 `enabled: true`。kind 词表与 `AppSettings::validate` 一致。
- 保存走 `SaveSettings { expected_revision, settings }`，core 侧 `replace_app_settings` 以 `AuthorityRevision` CAS 落盘；revision 冲突返回错误，由用户重新加载。
- Web backends / MCP servers 的可视化编辑分别属于 T13 / T14，当前以占位文案标示。

## 5. 会话流

侧栏 New -> `CreateSession` -> 成功后插入列表并自动 `OpenSession`；对话条目（`ConversationEntry`）按 `EntryKind`（user / event）渲染为气泡。发送后 `sending` 置位、composer 清空并禁用，直到 `Sent` reply 到达（`MessageSent` action 更新 head 并追加条目）。真实模型流式回复在 T11 接入 Provider runtime 后填充。

## 6. T10 验证门禁

- `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked` 全绿。
- 单元测试覆盖：reducer 全 action 分支（含 settings 往返、dirty/cas revision、composer 截断、错误横幅）；bridge 命令/reply 配对（noop-waker 轮询 oneshot，覆盖 runtime 失败路径的错误 reply）。
- 桌面窗口本身需 GPU，CI 只验证编译与单元测试；人工冒烟（开窗、建会话、改设置保存、切换主题）在 Windows 本机执行。
