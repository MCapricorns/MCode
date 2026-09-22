# 桌面 UI

`mycode-desktop` 是唯一产品前端。视觉是暖纸墨 Desk：平涂、发丝线、小圆角、mono 账本，蜂蜜色作唯一强调色。

## 1. 布局

- 顶栏：`MYCODE//UI`、项目名（文件夹名，不是 `ses1-…`）、DAY/NIGHT。
- Chat：248px 项目侧栏 + 中央时间线 + 右侧 inspector。
- 欢迎页：字标、能力 chips（AGENTS / MCP / WEB / FILES）、打开项目 / 开始聊天、最近项目（hover 显示删除）。
- 设置：General / Models / Agents / MCP / Web / Data / About。

时间线条目：`YOU` / `THINKING`（虚线推理盒） / `AGENT`（浅底气泡） / `TOOL`。

## 2. 桥接

`CoreBridge` 在专属线程跑 current-thread tokio。UI 只 `dispatch` + 泵 `BridgeEvent`。命令通道无界，避免卡住 spawn 出的 turn。

## 3. 项目选择

应用内 GPUI 文件夹浏览器绑定真实路径到当前会话（无会话则先创建），不再调系统目录框。`ui.json` 记 `session_projects` 与 `recent_projects`。侧栏按项目文件夹名分组。新聊天继承当前项目。

未绑定项目时工具 cwd 是 `~/.mycode/scratch`，不会在 home 里生成以会话 id 命名的项目文件夹。

## 4. View model

`view_model.rs` 纯 reducer：无 IO。模型选择、设置 CAS、更新状态机都经 action。

## 5. 门禁

`cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`。窗口本身需 GPU，CI 只编测。
