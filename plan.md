# MCode 交付计划

只记录未完成工作与必须保持的边界。当前分支为 `main`。

## 产品形态

一套 core 后端,两个前端:

- **Core 后端**(mcode-core/config/provider-api/tools/agent/plugin-host):Session、Provider、Pack 插件系统、凭据、安装与 generation substrate 全部在 core;不包含任何 UI 逻辑、渲染类型或窗口/终端细节。
- **桌面前端**(新 crate `mcode-desktop`):Zed GPUI 构建,布局对齐 Cursor/Codex 桌面版 —— 左侧会话与历史栏,中央 agent 对话流 + composer,右侧上下文面板(改动文件/diff/Todo/Usage),顶栏模型与 route 选择;插件系统有完整管理界面(Pack 列表、签名安装/更新/启停、Provider 凭据)。皮肤只用内置浅色/深色两套默认主题,不提供外部 Theme/Wallpaper 扩展。
- **TUI 前端**(`mcode-tui`):保留,与桌面共享同一 core typed API,不复制后端逻辑。
- 两个前端只消费 core 的公开 typed API(Session/Provider/插件/安装服务);解耦边界:界面是界面,core 是 core,禁止 UI 依赖渗入 core,也禁止前端绕过 core 直接触文件/凭据/网络。
- 目标平台仅 Windows 与 macOS;不构建、不测试、不发布 Linux。headless CLI 保留 typed surface,与前端共享同一 core。

## 当前检查点:T10 GPUI 桌面应用

- [ ] 新 crate `mcode-desktop`:GPUI 应用骨架、Cursor/Codex 布局、会话侧栏(消费 T9 SessionService)、对话流与 composer 视图(Provider 接入前展示会话事件)、上下文面板骨架、插件系统管理界面(读 T6 RootComposition/PackInstallation,安装动作在 T11 后激活)、generic login 骨架、内置浅色/深色主题切换。
- [ ] 纯状态 view-model 与 GPUI 渲染层分离;view-model 无 GPU 依赖、可测。
- [ ] 新增 `docs/design/06-desktop-ui.md` 冻结桌面布局、浅色/深色主题、解耦边界与 GPUI 约束;TUI 规格保留于 06-tui.md。

## 产品与扩展边界

第一方内置:Session、Compaction、Resources、Ask、Todo、Subagents、Workspace、两个前端 UI。它们与 Core/Host 同版本交付,直接使用 Rust typed API,不经过 Wasm Manager、JSON task wire、Pack discovery 或独立安装。

| 外部扩展 | 安装数 | 同时激活 | 说明 |
| --- | ---: | ---: | --- |
| Provider Packs | N | N | provider/model identity 全局唯一 |
| Web Packs | N | 0..1 | Querit 与 Synthetic Web 互斥,无 fallback |
| MCP Packs | N | N | 每个 Pack 可挂 N 个 server/tool;identity 全局唯一 |
| Usage Packs | N | N | 按 canonical source identity 隔离 |

第一方和第三方外部 Pack 使用同一签名、安装、更新、限额、generation fence 和故障隔离路径。Pack 无 WASI,不取得任意 filesystem/network/process/socket/credential authority;Host 独占 transport、secret、storage、process 和 workspace handle。

外部实现硬边界:

- Provider:Pi importer 必须可重复并对未知上游变化 fail closed;Synthetic 固定 `POST https://api.synthetic.new/v1/chat/completions`,保留 requested/returned model provenance。
- Web:Querit 固定 `/v1/search` 与 `/v1/contents`,query/count/fetch/body/deadline 全部有界;Synthetic 固定 `/v2/search` 且不提供 fetch fallback。URL、redirect、DNS/IP、credential 和 remote-text sanitization 归 Host。
- MCP:stdio/HTTP command/origin/auth/config 进入 signed binding;Host 独占进程、网络、重连、cancel/drain 和 backpressure。
- Usage:外部 quota snapshot 与 Host accounting 分离;Pack 不查询 Provider、不猜当前模型、不直接读取 credential source。
- UI:Host/core 独占窗口、输入、IME、剪贴板与 OS capability;桌面渲染只经 GPUI 且只有内置浅色/深色主题,TUI 只经终端 safety/sanitization 与内置 terminal 主题。

## 实现参考

实现前审读本仓库 WIT/goldens/design、用户 GitHub 与下列锁定源码;只迁移行为、状态机和测试,不迁移本机 authority、monkey patch、动态加载或配置写入。

- 内置能力行为基线:Session、Compaction、Resources、Ask、Todo、Subagents、Workspace 和两个前端 UI 全面对齐 Codex/Grok 的公开优秀实践;旧 MCode 产品行为不再作为参考,只复用已验证的安全、持久化和 generation primitives。
- 通用行为:durable goal、resume/fork/rewind/compact、明确 approval boundary、可观察的 streaming tool events、typed output/citation provenance,以及只对独立工作并行 fan-out;实现保持 MCode-owned。
- Ask/Todo:`juicesharp/rpiv-mono@d13677c` 的 `rpiv-ask-user-question`、`rpiv-todo`;保留 1..4 questions、typed answers、preview、abandon、可见 todo 状态、dependency/replay 语义。
- Usage:`marckrevv/pi-sub@65deb56`;复用 source/display 分层、缓存快照与 quota windows,不读取其他工具的 `auth.json` 或环境凭据。目标覆盖 DeepSeek、OpenAI、Synthetic、Kimi、Z.AI/GLM 等真实可验证 source adapter。
- 桌面 UI:Zed GPUI(gpui)+ gpui-component(Dock/Tab/Input/List 等桌面组件);布局基线 Cursor/Codex 桌面版;view-model 与渲染分离沿用 mcode-tui 的纯状态模式。
- TUI:沿用现有 mcode-tui 纯状态基座与 `06-tui.md` 规格及内置 terminal 主题。
- Web:`dsh-web-querit`、`pi-querit-search`、`pi-web-access`。
- Subagents:以 Codex/Grok 的异步委派模型为主,吸收用户现有 GitHub/本地实现与 `pi-subagents` 中可证明可靠的队列、worktree 和恢复机制;父 agent 不以同步 wait 驱动正常进度。

## 后续 TODO

- [ ] T11:外部 Pack 与 asset 的签名安装、更新、回滚和 crash-safe WAL;bundle 不执行 build/npm/Git hook;消费 T6 staging 与 T8 fence/loading/activation;桌面插件界面安装动作接通。
- [ ] T12:Provider runtime、Pi/Synthetic Packs;补 OpenAI、DeepSeek、Kimi、Z.AI/GLM 等 canonical adapters;桌面与 TUI 的对话流接入。
- [ ] T13:TUI 前端接入同一 core(Session/Provider/插件服务),与桌面共享 view-model 语义。
- [ ] T14:内置 Workspace checkpoint/rollback;no-follow handle、并发冲突和不可回滚证据。
- [ ] T15:内置 Resources catalog/read/render-prompt/contributions 与 Host-owned large payload sidecar。
- [ ] T16:内置 Ask interaction、typed progress/result、cancel-safe Host wait。
- [ ] T17:内置 Todo stable ID、dependency graph、revision/CAS 与 durable task event。
- [ ] T18:Web runtime、Querit/Synthetic Packs;bounded reader、URL/SSRF、provenance 与 sanitization。
- [ ] T19:MCP multi-Pack composite catalog、owner routing、transport binding 与 replacement fence。
- [ ] T20:Usage multi-source sampling、accounting、quota/status snapshots 与 UI widgets。
- [ ] T21:内置 Subagents async fan-out、bounded queue、steer/follow-up/cancel、worktree lease 与 crash recovery。
- [ ] T22:内置 Compaction adaptive scheduling、Provider child completion 与 atomic checkpoint。
- [ ] T23:产品 export/import。
- [ ] T24:Core 自动更新。
- [ ] T25:最终产品组合:headless CLI + 桌面 + TUI。
- [ ] T26:删除旧路径的识别、读取、兼容代码和 dead code。
- [ ] T27:最终文档与扩展指南。
- [ ] T28:Windows/macOS 安全、offline/crash、redaction 与 e2e 门禁(不做 Linux)。
- [ ] final:workspace 全量 audit/cleanup、Windows/macOS CI、secret/provenance/release review,发布 `v0.0.1`。

依赖主线:`T9 -> T10`;`T10 + T6/T7/T8 -> T11 -> T12`;T11 在 Web/Usage(T18、T20)前完成;T12 后双前端同步接入。

## 开发门禁

- 一个完整 feature 一个 commit/push;`plan.md` 状态清理独立提交。
- 先 targeted,再跑相关 format/lint/build/test;提交前审阅 exact staged diff。
- `minimax.txt` 永远不得读取、打印、复制、修改或 stage。
