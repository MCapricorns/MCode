# MYCode

MYCode 是一个基于 Zed GPUI 的 Windows/macOS 桌面编码 agent（Zed/Codex 式布局：活动栏 + 会话侧栏 + 对话流 + 上下文面板），围绕一个持久、可回放的会话 ledger 与第一方 provider runtime 构建。

## 功能

- **开箱即用的 Provider 目录**：内置 190+ 家模型厂商（来自 models.dev 快照），选择厂商、粘贴 API key 即可对话；目录在后台自动云控更新（ETag 条件请求、本地缓存、离线兜底），新厂商/新模型无需升级应用。
- **模型选择器**：顶栏按厂商/模型切换，选择持久化；支持 `anthropic-messages`、`openai-completions`、`openai-responses` 三家通用 wire 协议与自定义 endpoint。
- **项目工作区**：启动页选择项目文件夹（或从最近列表打开），agent 的文件/命令工具直接工作在项目目录；`write`/`edit` 前自动快照，Changes 面板一键回滚。
- **内置工具**：`read`、`write`、`edit`、`shell`、`exec`、`grep`、`find`，外加 `ask_user`（结构化问答）与 `todo_write`（stable ID + 依赖图）。
- **durable 会话**：所有用户消息、工具结果、assistant 回复落事件 ledger；崩溃后可恢复、回放。
- **MCP 客户端**：stdio 与 Streamable-HTTP 双 transport，Context7 内置目录、自填 key。
- **Web 搜索**：bounded 客户端（URL/SSRF 防护），结果展示在 Web 面板。
- **Usage 统计**：按 provider/模型聚合 token 用量，durable 记录，Overview 面板展示。
- **自动更新**：自动检查 GitHub Releases，下载并校验 SHA-256 后暂存，重启即完成换装（Windows 无控制台闪现）；Release 产物提供 Windows x64 与 macOS arm64/x64 可执行文件。

## 下载

前往 [Releases](https://github.com/MCapricorns/MCode/releases) 获取最新版本：

- `mycode-desktop-v<版本>-x86_64-pc-windows-msvc.zip` — Windows 10/11 x64
- `mycode-desktop-v<版本>-aarch64-apple-darwin.zip` — macOS Apple Silicon
- `mycode-desktop-v<版本>-x86_64-apple-darwin.zip` — macOS Intel

应用内置自动更新；也可以手动下载覆盖安装。

## 构建

```text
cargo build --release -p mycode-desktop
target/release/mycode-desktop.exe
```

工具链：Rust stable（MSVC）。门禁：`cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`。

## 数据位置

`MYCODE_HOME`（默认 `~/.mycode`）下：`settings.json`（配置）、`secrets.json`（API keys，Debug 输出打码）、`ui.json`（最近项目/更新偏好）、`catalog-cache.json`（provider 目录缓存）、`sessions/`（durable ledger）、`checkpoints/`（文件快照）、`workspace/<session>/`（缺省工具工作目录 + todos.json）。

## 文档

- [docs/design/](docs/design/README.md) — 架构与契约

## License

Apache-2.0
