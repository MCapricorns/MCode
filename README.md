# MYCode

MYCode 是一个基于 Zed GPUI 的 Windows / macOS 桌面编码 agent（Desk 三栏：项目侧栏 + 对话时间线 + 右侧 inspector）。会话落 durable ledger，模型经三个通用 wire 协议接入，工具由第一方 runtime 提供给模型。

## 功能

- **模型目录**：内置 models.dev 快照（190+ 厂商），粘贴 API key 即可对话；后台 ETag 云同步，离线回退内置快照。
- **三种 wire**：`anthropic-messages`、`openai-completions`、`openai-responses`；自定义 endpoint 同协议即可。
- **项目工作区**：欢迎页或侧栏选择真实项目文件夹。工具 cwd 就是该路径；未绑定时落到 `~/.mycode/scratch`，不再创建 `workspace/ses1-…` 假项目。
- **内置工具**：`read` / `write` / `edit` / `find` / `grep` / `shell` / `exec`，以及 `ask_user`、`todo_write`。
- **Subagent**：`task` 委派 scout / artisan / steward / sentinel；设置页可开关角色并指定模型。
- **MCP**：stdio 与 Streamable-HTTP；支持粘贴 JSON 配置导入；工具并入同一 registry，写入 system prompt。
- **Web 搜索**：默认 Querit 与 AnySearch，只贴 API key；请求自动带 `Authorization: Bearer`。模型使用 `web_search` + `fetch_content`。
- **自动压缩**：Codex 风格——触发为可用窗口（模型 context 的 95%）的 90%，保留约 20k token 尾部；每轮 provider 请求前重估（含工具结果后的 mid-turn）。
- **Durable 会话**：用户消息、工具结果、assistant 回复落 `sessions/<id>/` ledger；`write`/`edit` 前自动 checkpoint。
- **自动更新**：检查 GitHub Releases，SHA-256 校验后暂存，重启换装。

## 下载

前往 [Releases](https://github.com/MCapricorns/MCode/releases) 获取最新版本：

- `mycode-desktop-v<版本>-x86_64-pc-windows-msvc.zip` — Windows 10/11 x64
- `mycode-desktop-v<版本>-aarch64-apple-darwin.zip` — macOS Apple Silicon

自 v0.4.0 起不再发布 macOS Intel（x86_64）构建。版本记录从当前版本开始，见 [CHANGELOG.md](CHANGELOG.md)。

## 构建

```text
cargo build --release -p mycode-desktop
```

工具链：Rust stable（Windows 用 MSVC）。门禁：`cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`。

## 数据位置

`MYCODE_HOME`（默认 `~/.mycode`）：

```text
~/.mycode/
├─ settings.json          # 配置（无密钥）
├─ secrets.json           # API keys（web-<id> / mcp-<id> / provider id）
├─ ui.json                # 最近项目、会话→项目绑定、模型选择
├─ catalog-cache.json     # 云目录缓存
├─ sessions/<ses1-id>/    # ledger + todos.json + compaction.json
├─ checkpoints/<ses1-id>/ # write/edit 文件快照
└─ scratch/               # 未绑定项目时的工具工作目录
```

密钥永不进 `settings.json`。会话目录用内部 id；界面标题用项目路径的文件夹名，不用 `ses1-…`。

## 文档

- [docs/design/](docs/design/README.md) — 架构与契约

## License

Apache-2.0
