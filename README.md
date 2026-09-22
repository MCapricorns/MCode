# MYCode

MYCode 是本机桌面编码代理。会话、模型和工具都在你的电脑上跑。

## 窗口

左边是工作区。一个工作区可以同时挂几个文件夹，比如前端和后端。会话挂在对应的文件夹下面。当前会话的工作目录决定相对路径；其它文件夹用绝对路径。

中间是对话。输入框旁可以直接换模型和思考强度。待办显示在输入框上方，做完的项会消失。

右边是这个会话正在用的模型，以及当前文件夹的 git 改动。点一个文件可以看到 diff。

设置里可以换浅色或深色，以及几套配色。

## 模型

内置 models.dev 目录。填 API key 就能用。自定义 endpoint 走这三种协议之一：

- `anthropic-messages`
- `openai-completions`
- `openai-responses`

## 工具

内置 `read`、`write`、`edit`、`find`、`grep`、`shell`、`exec`、`ask_user`、`todo_write`。

`task` 可以委派 scout、artisan、steward、sentinel，也可以委派 `agents/` 里的自定义角色。子代理可以使用已连接的 MCP 和 skills。

MCP 用 stdio 或 Streamable HTTP。先 `search_tool` 取 schema，再 `use_tool`。网页检索用 `web_search`，引用前再用 `fetch_content`。

工具行会写出目标，例如读了哪个文件、搜了什么、跑了哪条命令。

## 下载

[Releases](https://github.com/MCapricorns/MCode/releases)

- `mycode-desktop-v<版本>-x86_64-pc-windows-msvc.zip` — Windows 10/11 x64
- `mycode-desktop-v<版本>-aarch64-apple-darwin.zip` — macOS Apple Silicon

从 0.4.0 起不再发布 macOS Intel 构建。变更见 [CHANGELOG.md](CHANGELOG.md)。

## 构建

需要 Rust stable。Windows 用 MSVC。

```text
cargo build --release -p mycode-desktop
```

检查：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## 数据

`MYCODE_HOME` 默认是 `~/.mycode`。

```text
~/.mycode/
├─ settings.json
├─ secrets.json
├─ ui.json
├─ catalog-cache.json
├─ sessions/<id>/
├─ checkpoints/<id>/
└─ scratch/
```

密钥只在 `secrets.json`。

## License

Apache-2.0
