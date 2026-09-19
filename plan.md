# MCode 交付计划

只记录未完成工作与必须保持的边界。当前分支为 `main`。

## 产品形态

MCode 是一个 GPUI 桌面应用(Windows/macOS),没有其他前端:

- **桌面应用**(crate `mcode-desktop`,Zed GPUI + gpui-component):布局对齐 Cursor/Codex 桌面版 —— 左侧会话与历史栏,中央 agent 对话流 + composer,右侧上下文面板(改动文件/diff/Todo/Usage),顶栏模型选择。
- **一切配置可视化**:Providers(端点/密钥/模型)、UA、Web 搜索、MCP servers、Usage、外观(浅色/深色)等全部在应用内设置界面配置并持久化到 core 的设置存储;没有需要手改的配置文件。
- **不保留**:TUI、headless CLI、第三方/外部 Pack 插件系统(Wasm/WIT/签名安装/generation pack 激活全部移除)。Provider/Web/MCP/Usage 等能力为第一方内置实现。
- **core 与 UI 解耦**:core(`mcode-core/config/provider-api/tools/agent` + session 服务)不包含 UI/渲染/窗口概念;桌面只经 core 公开 typed API 访问会话、Provider、设置与凭据,不直接触文件/密钥/网络。
- HTTP User-Agent 等请求标识可配置,默认沿用 pi agent 的 UA(pinned 参考),其余请求头策略仍由 core 统一管理。

## 当前检查点:T10 桌面应用 + 设置模型(已交付)

- [x] `mcode-desktop` GPUI 应用:Cursor/Codex 布局、会话侧栏(消费 T9 SessionService)、对话流 + composer(Provider 接入前展示会话事件)、上下文面板、浅色/深色切换。
- [x] core 设置存储:`~/.mcode` 下新的 strict 设置文档(providers、UA、web、mcp、usage、appearance),CAS 写入、vault 存密钥;桌面设置页可视化读写。
- [x] 纯状态 view-model 与 GPUI 渲染分离,view-model 无 GPU 依赖、可测。
- [x] 桌面 UI 契约冻结(`docs/design/09-desktop-ui.md`)。

## 内置能力边界

第一方内置:Session、Compaction、Resources、Ask、Todo、Subagents、Workspace、设置与桌面 UI。全部与 core 同版本交付,直接使用 Rust typed API。

- Provider:内置 adapters(OpenAI、DeepSeek、Kimi、Z.AI/GLM、Synthetic、anthropic-messages 兼容端点);端点/密钥/模型全部来自设置;UA 默认 pi agent 值、可配置;secret 只存 vault,UI 输入后不回显。
- Web:内置搜索(Querit 语义的 `/v1/search`+`/v1/contents`,bounded、URL/SSRF sanitization),开关与参数在设置页。
- MCP:内置客户端,server 列表/命令/origin 在设置页配置;进程与网络归 core。
- Usage:内置用量统计与配额展示,按 provider/模型聚合,展示在上下文面板。
- Session:durable ledger/WAL、branch/resume/rewind、replay/recovery(T9 已交付)。
- Compaction:每次 tool result durable 后、下一次 Provider 请求前重新估算;原子 checkpoint。

## 实现参考

实现前审读本仓库 design、用户 GitHub 与下列锁定源码;只迁移行为、状态机和测试。

- 行为基线:对齐 Codex/Grok 的公开优秀实践;旧 MCode 产品行为只复用已验证的安全、持久化 primitives。
- 通用行为:durable goal、resume/fork/rewind/compact、可观察 streaming tool events、typed output/citation provenance、独立工作才并行 fan-out。
- 桌面 UI:Zed GPUI + gpui-component;布局基线 Cursor/Codex 桌面版。
- Ask/Todo:`juicesharp/rpiv-mono@d13677c` 的 `rpiv-ask-user-question`、`rpiv-todo`;1..4 questions、typed answers、preview、可见 todo 状态。
- Usage/Provider 行为:`marckrenn/pi-sub@65deb56`(source/display 分层、quota windows);UA 默认值取 pi agent 的请求标识。
- Provider/BYOK 架构:`MiniMax-AI/minimax-code`(vendored pi + 装配层;BYOK 三个通用 wire 协议、宿主注入 fetch、typed provider 错误、thinking signature 保真、模型发现回退链;笔记见 `docs/research/2026-09-18-minimax-code-notes.md`)。
- Web:`dsh-web-querit`、`pi-querit-search`、`pi-web-access` 的 bounded reader/sanitization 行为。
- Subagents:Codex/Grok 异步委派模型 + 用户现有实现中可靠的队列、worktree、恢复机制。

## 后续 TODO

- [x] T11:Provider runtime + 内置 adapters(含 UA 配置);桌面对话流接入(流式输出、tool 调用展示)。(已交付:`mcode-providers` 三协议适配器 + secrets store + 桌面流式回合)
- [x] T12:移除旧插件系统:删除 mcode-tui、mcode-cli、mcode、mcode-render、mcode-plugin-host(wasmtime/WIT/pack ABI 一并移除);session/generation/task-runtime 独立为 `mcode-session` crate;孤儿 workspace 依赖(wasmtime、wat、ratatui、crossterm、clap 等)已清理。
- [x] T13:Web 内置搜索(`mcode-web` bounded 客户端 + URL/SSRF 防护)+ 桌面 Web/Changes 面板 + 设置页 backend 管理。
- [x] T14:MCP 内置客户端(`mcode-mcp` stdio + Streamable-HTTP 双 transport,Context7 内置目录、用户自填 key)+ 设置页 servers 管理 + List tools。
- [ ] T15:Usage 内置统计 + 面板/配额展示;设置页 usage 选项。(用户指示暂缓)
- [x] T16:内置 Workspace checkpoint/rollback(write/edit 前自动快照、Changes 面板一键回滚)+ agent 工具执行接入桌面(tool call/result 流、ledger 落盘、history replay)。
- [x] T17:内置 Resources(AGENTS.md/MCODE.md 发现、bounded 读取、system prompt 注入、Overview 展示)。
- [x] T18:内置 Ask(`ask_user` 工具、1..4 结构化问题、cancel-safe 等待、桌面应答面板)。
- [x] T19:内置 Todo(stable ID、blockedBy 图校验、revision CAS、durable Task 事件、Overview 展示)。
- [x] T25:删除旧路径的识别、读取、兼容代码和 dead code(旧 crate 全删、mcode-plugin-api 收口、孤儿依赖清理)。
- [x] T26:最终文档(README 重写、design 文档收口为 00/01/02/09 + plan.md)。
- [x] T15:Usage 内置统计(durable Usage 事件、Overview token 面板、设置页开关)。(v0.1.0)
- [x] T27:Provider 目录云控同步(`mcode-catalog`:models.dev 归一化快照内嵌 + ETag 条件请求后台刷新 + 本地缓存;设置页目录预设,选厂商粘 key 即用;模型选择器按目录/配置发现)。(v0.1.0)
- [x] T28:桌面打磨(zcode 式布局:活动栏 + 齿轮图标进整页设置、欢迎页项目选择器 + 系统目录对话框 + 最近项目持久化 `ui.json`、顶栏 project/model chip、会话流气泡/工具行重绘)。(v0.1.0)
- [x] T23:自动更新(`mcode-updates`:GitHub Releases 最新版检查 + semver 比较 + SHA-256 校验下载 + 暂存换装脚本重启;Windows release 关闭控制台)。CI 发布 Windows x64 与 macOS arm64/x64 可执行文件到 Release。(v0.1.0)

### Backlog(v0.1.0 后)

- [ ] T20:内置 Subagents async fan-out、bounded queue、steer/follow-up/cancel、worktree lease 与 crash recovery。
- [ ] T21:内置 Compaction adaptive scheduling、Provider child completion 与 atomic checkpoint。
- [ ] T22:产品 export/import。
- [ ] T29:目录预设的 provider 行内多模型勾选(当前默认单模型;更多模型经自定义 endpoint 或重改名预设添加)。
- [x] T30:opencode v2 式桌面重绘(去系统标题栏 + 自绘窗口控制、应用图标、项目侧栏 + 会话分组/首条消息标题、composer 卡片化、整页设置二级导航、会话↔项目映射 ui.json、JSON 配置落盘格式化、bridge 命令改 tokio channel 修复回合饿死)。

### v0.1.0 收口

- [x] final:workspace 全量 audit(含目录下载/更新下载的安全边界)、发布 `v0.1.0` tag + GitHub Release(Windows/macOS 双平台产物)。

依赖主线:`T9 -> T10 -> T11 -> T12`;T13–T15 依赖 T11;T16+ 依赖 T12。

## 开发门禁

- 一个完整 feature 一个 commit/push;`plan.md` 状态清理独立提交。
- 先 targeted,再跑相关 format/lint/build/test;提交前审阅 exact staged diff。
- `minimax.txt` 永远不得读取、打印、复制、修改或 stage。
