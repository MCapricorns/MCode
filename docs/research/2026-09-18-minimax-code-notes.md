# MiniMax Code（pi 衍生）学习笔记

> 对象：`MiniMax-AI/minimax-code` v0.4.12（2026-09 开源，MIT）。它是 pi-mono 的下游产品：将 `earendil-works/pi-mono` 以 **vendored source** 形式放在 `third_party/pi-mono`（基线 v0.79.1），自己的扩展层在外层 package 组装，而不是直接改 pi。本笔记提炼对 MCode（尤其 T11 Provider runtime、T16+ agent loop）直接可用的做法。

## 1. 总体架构：vendored pi + 装配层

```text
TUI / exec / ACP → CliService → local Applications → Session/Turn/Agent services
  → @mavis/agent-core (PiTurnRunner 装配层) → vendored pi (agent/ai/coding-agent/tui) → providers
```

- `PiTurnRunner`（agent-core）把 pi 的 Agent 组装成"单 Turn"边界：runner 实例跨会话复用，但每次 `runTurn` 构造**全新**的 Agent、EventBridge、事件队列与 history cursor；per-turn 状态全在 turn 对象里，runner 只持进程级默认值。
- pi 事件 → 规范 `RuntimeEvent` 的 event-bridge 带显示层脱敏（display-sanitize）、工具能力归因（plugin capability attribution）。
- 对 pi 的修改全部记录在 `third_party/pi-mono/MINIMAX_CHANGES.md` ledger（原因/受影响包/generic vs 私有 glue/验证命令与结果），配套 `.minimax-vendor.json` 记录上游 ref 与所有权。**改动纪律值得照抄**：能 upstream 的做成 generic patch，产品私有的做成宿主 glue，绝不静默 fork。

## 2. Provider/Model System（对 T11 最直接）

`local-runtime-v2/src/service/model-system/` 是唯一的模型 owner，两组能力共享同一 profile 配置端口：

- **resolution**：进程配置 + 认证上下文 + 模型引用 → `LLMModelConfig`（见下）。
- **provider management**：BYOK provider 配置、模型目录（catalog/缓存）、连通性测试、OAuth 状态。

要点：

1. **BYOK 只认 3 个通用 wire 协议**：`anthropic-messages`、`openai-completions`、`openai-responses`；默认 `anthropic-messages`。几十家厂商差异只体现在 base URL、凭证、模型元数据与 preset，不写 per-vendor 适配类（OAuth 型 `openai-codex-responses` 单独排除在通用 BYOK 之外，由专属 transport 拥有凭据/base URL）。这与我们"后期接很多家"的目标一致：**kind = wire protocol，厂商 = 数据**。
2. `LLMModelConfig` 装配面：`model + apiKey + streamFn + thinkingLevel + cacheRetention + maxTokens + hostMaxOutputTokens + headers + fetch + payloadTransform(+auxiliary) + responseObserver`。其中 `hostMaxOutputTokens` 是宿主上限，取 min(host, 请求, 模型上限)，宿主已收紧的预算不会被下游放宽。
3. **网络出口全部宿主注入**：`fetch` 注入贯穿推理请求、OAuth refresh、初始 OAuth exchange；注入 fetch 且 transport=auto 时 Codex 自动走 SSE（WebSocket 无法用注入 fetch），SSE 响应头 deadline 放宽到 30s。→ MCode 的 provider runtime 从第一天就把 HTTP client 做成注入 seam，UI/宿主拥有全部 egress。
4. **BYOK 模型发现**：Anthropic 兼容端先 `<base>/v1/models`，仅 404/405 回退 `<base>/models` 再到 origin `/models`；鉴权失败/限流/5xx 原样透出；全部缺失返回 `models_endpoint_missing` 让 UI 提供手填模型入口；每次尝试独立超时。
5. **归因 header 策略**：OpenRouter 官方端点由 runtime 大小写不敏感地强制覆盖 `HTTP-Referer`/`X-OpenRouter-Title`；OpenCode Zen 强制 `x-opencode-session` + `User-Agent: MiniMaxCode`（会话 ID 跨 turn/重试/恢复稳定）。用户自定义 header 不能改变产品归因身份。
6. **凭证**：OAuth（browser/device_code）状态在内存，确认未取消后才落 `codex-auth.json`；取消/超时会拦截迟到结果。BYOK 凭证与配置分离（secret 走独立存储）。

## 3. 对 pi 本体的关键 patch（MINIMAX_CHANGES.md 精选）

Provider/stream 正确性：

- **thinking 语义**：Claude 默认开启 thinking 的模型族（Opus/Sonnet 5 等）不发显式 `thinking.type=enabled`+budget，只保留 `output_config.effort`；**签名空 thinking 块必须在序列化时保留**（空字符串+signature 的块被丢弃会导致后续请求签名校验失败、丢失受保护推理连续性）。→ T11 Anthropic 适配器必须把 thinking signature 当一等公民 round-trip。
- **onProviderError seam**：SDK 异常被格式化成 `errorMessage` 前先回调原始错误（cause/DNS code/HTTP status/SDK 字段），同步且异常隔离。→ 我们的 provider 错误要在边界保留 typed facts（错误码、HTTP status、retryable），不要先 stringify。
- **parsed stream event observer**：可选同步回调暴露 provider-native 流事件，宿主做 inspector 不用 tee HTTP body。

Agent loop 控制（对 T16+ 有用）：

- tool hook 可返回 `terminateAgent`（区别于"批内全部 terminate 才停"）：顺序执行立即停；并行批先停止 admit 未启动的调用，**未执行的调用补合成 error ToolResult 保证 provider history 完全配对**。
- steering 之后的 `agent_end` seam：queued `UserPromptSubmit` hook 返回 `continue:false` 时不再发下一次 provider 请求、不报 failed turn。
- `Agent` 包装层透传 `shouldStopAfterTurn`（turn 边界、在消费 steering/follow-up 队列之前停）。
- `steerBatch`：显式批量 steering 一次 provider hop 消费、保留每条消息身份。

Windows/进程健壮性（对我们的 bash 工具直接相关）：

- `taskkill.exe` 从 `SystemRoot` 解析、每个 spawn 挂 `error` 监听（异步 spawn 失败不再炸宿主进程）。
- 终止分级 SIGTERM → grace → SIGKILL，让 shell `trap` 清理逻辑有机会执行；grace SIGKILL 只在进程组真正清空后解除。
- detached 进程树的 parent-death guardian（host IPC lease 消失则整树终止），Windows 用最小环境 Node launcher，防 `NODE_OPTIONS` 污染。
- PowerShell 5.1 ConstrainedLanguage 兼容：探测 LanguageMode，受限模式经一次性 Unicode 环境变量 + `Invoke-Expression` 传递命令，不碰受限 .NET API。
- stdout/stderr 独立流式 UTF-8 解码器（截断的完整输出文件也要合法文本）。

Edit 工具保真（对我们的 edit 工具直接相关）：

- fuzzy 匹配只用于**定位**：归一化命中映射回原文 span（per-line NFKC 列映射 + 单调前缀二分），替换只拼接该 span，全文绝不重写归一化；否则一个 fuzzy edit 会把全文的智能引号/全角空格/U+3000/行尾空白全部悄悄改掉。
- **保真 guard fail-closed**：back-map 向外取整若吞掉模型没完整命中的兼容字符（如 `ﬁ` ligature），映射后 re-normalize 必须逐字复现命中文本，否则返回 not-found 让模型重试，绝不误写。

其他值得记的：

- 工具结果保留**结构化事实**（Bash 的分流 stdout/stderr、Read 的路径/页/行区间/总行数/截断信号），下游不用解析装饰过的英文提示。
- `onToolExecutionStart` 在**准入之后**、execute 之前触发（排除 permission/PreToolUse 延迟的纯执行时延）；被 block 的调用不触发。
- 图片：Photon 不可用时"本来合规"的原图直接透传（头部尺寸+体积校验），需要缩放/转码才报 `ImageProcessorUnavailableError`。

## 4. 对 MCode 的落地结论

1. **T11 settings 词表改造**：`ProviderSettings.kind` 从厂商枚举（openai/deepseek/kimi/zai…）改为 **wire protocol 枚举**（`anthropic-messages | openai-completions | openai-responses`），厂商差异进数据（base_url/models/preset）；后续加厂商 = 加数据不加代码。OAuth 型专用 provider（如 codex）未来单独建模，不塞进通用 BYOK。
2. **T11 provider runtime 形状**：请求装配面含 host 上限（min 语义）、注入 HTTP client、headers；错误在边界保留 typed（status/code/retryable/cause），渲染成字符串只发生在 UI 层。Anthropic 适配器保留 thinking signature 块（含空 thinking）。
3. **T24 连通性/模型发现**：抄 `<base>/v1/models → <base>/models → origin /models`（仅 404/405 回退）+ `models_endpoint_missing` 手填入口。
4. **T16+ agent loop**：terminate/steering 语义按 3.3 的配对规则设计（未执行调用补合成 ToolResult；steering 后的终止不再发 provider 请求）。
5. **bash/edit 工具**（未来 T 项）：终止分级 + Windows taskkill 解析 + error 监听；fuzzy edit 只定位不重写 + fail-closed 保真 guard。
6. **改动 ledger**： vendored/借用上游行为的偏差全部记录原因+验证，MCode 的 plan.md 继续承担该职责。
