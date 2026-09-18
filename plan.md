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

- [ ] T11:Provider runtime + 内置 adapters(含 UA 配置);桌面对话流接入(流式输出、tool 调用展示)。
- [ ] T12:移除旧插件系统:删除 mcode-tui、mcode-cli、mcode、mcode-render;从 plugin-host 剥离 wasmtime/WIT/pack ABI,session 服务独立成 crate;清理 root composition/pack installation 等被设置存储取代的路径。
- [ ] T13:Web 内置搜索 + 右侧上下文面板的改动文件/diff 视图。
- [ ] T14:MCP 内置客户端 + 设置页 servers 管理 + 上下文面板工具状态。
- [ ] T15:Usage 内置统计 + 面板/配额展示;设置页 usage 选项。
- [ ] T16:内置 Workspace checkpoint/rollback;no-follow handle、并发冲突和不可回滚证据。
- [ ] T17:内置 Resources catalog/read/render-prompt/contributions。
- [ ] T18:内置 Ask interaction、typed progress/result、cancel-safe wait。
- [ ] T19:内置 Todo stable ID、dependency graph、revision/CAS 与 durable task event。
- [ ] T20:内置 Subagents async fan-out、bounded queue、steer/follow-up/cancel、worktree lease 与 crash recovery。
- [ ] T21:内置 Compaction adaptive scheduling、Provider child completion 与 atomic checkpoint。
- [ ] T22:产品 export/import。
- [ ] T23:Core 自动更新。
- [ ] T24:设置页收口(全部设置可视化、导入导出预设)与桌面打磨。
- [ ] T25:删除旧路径的识别、读取、兼容代码和 dead code。
- [ ] T26:最终文档。
- [ ] T27:Windows/macOS 安全、offline/crash、redaction 与 e2e 门禁(不做 Linux)。
- [ ] final:workspace 全量 audit/cleanup、Windows/macOS CI、secret/provenance/release review,发布 `v0.0.1`。

依赖主线:`T9 -> T10 -> T11 -> T12`;T13–T15 依赖 T11;T16+ 依赖 T12。

## 开发门禁

- 一个完整 feature 一个 commit/push;`plan.md` 状态清理独立提交。
- 先 targeted,再跑相关 format/lint/build/test;提交前审阅 exact staged diff。
- `minimax.txt` 永远不得读取、打印、复制、修改或 stage。
