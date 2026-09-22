# MYCode Harness

MYCode Harness 是跑在你电脑上的编码 harness。窗口是一块暖色磨砂桌面：项目在左，对话在中，用量在右。每个文件夹有自己的会话，模型通过三种通用协议接入，工具由本机 runtime 执行。

作者 **MaMy**。感谢 MaMy、YangChengxxyy、iKunCai。

## 桌面

- 标题栏是 **MYCode Harness**。DAY / NIGHT 直接切换外观。
- 表面是同一套半透明磨砂，蜂蜜色只做强调。
- 提示出现在右下角，三秒后消失。
- 设置 → About 里检查更新，并写着作者与感谢名单。

## 项目是隔离的

一个会话只属于一个文件夹。工具的工作目录就是这个文件夹。

切换项目，或把另一个文件夹拖进窗口，不会把正在进行的会话改绑过去。原来的会话留在原来的目录里继续；新文件夹打开自己的会话。还没有会话时，会为这个文件夹新建一场对话。

未绑定项目的会话使用 `~/.mycode/scratch`。界面上的名字是文件夹名，不是内部会话 id。

## 模型与工具

- 内置 models.dev 目录，粘贴 API key 即可对话。目录可手动刷新，离线时用内置快照。
- 三种 wire：`anthropic-messages`、`openai-completions`、`openai-responses`。自定义 endpoint 走同一协议即可。
- 内置工具：`read`、`write`、`edit`、`find`、`grep`、`shell`、`exec`，以及 `ask_user`、`todo_write`。
- Windows 上命令输出按 OEM / ANSI 代码页解码，中文目录列表不再变成乱码。
- `task` 可委派 scout、artisan、steward、sentinel。
- MCP 支持 stdio 与 Streamable HTTP。网页检索走 `web_search`，引用前再 `fetch_content`。
- 会话、工具结果和回复写入 ledger。`write` / `edit` 前自动做文件快照。

## 下载

[Releases](https://github.com/MCapricorns/MCode/releases)

- `mycode-desktop-v<版本>-x86_64-pc-windows-msvc.zip` — Windows 10/11 x64
- `mycode-desktop-v<版本>-aarch64-apple-darwin.zip` — macOS Apple Silicon

自 v0.4.0 起不再发布 macOS Intel 构建。变更见 [CHANGELOG.md](CHANGELOG.md)。

## 构建

```text
cargo build --release -p mycode-desktop
```

Rust stable。Windows 使用 MSVC。门禁：

```text
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## 数据

`MYCODE_HOME` 默认是 `~/.mycode`：

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

密钥只在 `secrets.json`。`settings.json` 不保存密钥。

## 文档

架构与契约在 [docs/design/](docs/design/README.md)。

## License

Apache-2.0
