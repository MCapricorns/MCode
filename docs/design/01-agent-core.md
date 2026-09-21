# 最小 Agent Core 与产品边界

> Core 只保留 loop 与七个 builtin；它不拥有任何产品 family 的持久化、路由、策略、网络、credential 或生命周期。

## 1. Agent loop

Core 仅消费稳定消息、tool call 与流式结果 DTO：构造 typed request、消费 `ProviderPackService` stream、执行 canonical builtin，并将验证后的 namespaced 动态调用交给 Host adapter 的所有者。

```text
loop {
    request = build_typed_request(state, tools)
    assistant = collect(ProviderPackService.stream(request))
    state.push(assistant)
    for call in tool_calls(assistant) {
        state.push(dispatch_canonical_or_host_adapter(call))
    }
    drain_steer_or_follow_up()
}
```

`read`、`write`、`edit`、`find`、`grep`、`exec`、`shell` 是唯一 builtin，且不可覆盖。没有公开 `bash`、`PermissionEngine`、Core Ask、grant、`--yolo` 或按名称特权。动态 contribution 必须有界、typed、namespaced，并经 Host preflight、provenance、family、generation 和 OS 安全验证。

## 2. Session、Workspace 与 Compaction

- Session 由 `session` Manager、Session Pack 和 `SessionPackService` 定义。durable bytes 只能写入选定 `plugins/session/packs/<pack-id>/data/`；Host 提供 no-follow owned storage、bounded WAL、atomic append、durability、backpressure、generation fence 与 DTO 验证，不解释会话语义或提供全局 Session tree。
- Workspace checkpoint/rollback 属于 `workspace` Manager 与 `WorkspacePackService`；不可证明范围的 exec/shell 标记不可回滚，rollback 不覆盖并发修改。
- Compaction 属于 Host-wide singleton `compaction` Manager 与 Pack。每次 tool result durable 后、下一次 Provider 请求前重新估算；切换必须 cancel/drain 后原子完成，Pack 缺失或失败不得由 Core 回退。

## 3. Provider、Usage 与 auth

Provider 由 `providers` Manager 和独立 ProviderPack world 实现。Host 独占 auth store、HTTP、TLS、DNS、proxy、credential lookup/refresh/insertion、reserved-header policy 与审计；Provider Pack 看不到 credential、socket 或 HTTP client。

唯一 auth 文件是 lazy 的 `~/.mycode/plugins/.host/auth.json`，不属于 Providers 或任何 family。每个签名 Provider/Web/Usage Pack 以精确 canonical account、issuer、auth schema、source/signer、credential-contract 和 operation/method/origin/path/auth slot 获批。Host 自动为任何 active Pack 精确匹配 account 并生成单次、generation-bound injection lease；同一 account 不重复存储 secret，也不逐 Pack 登录。mismatch/new authority 必须 fail closed/rebind。

Provider 和 Usage 相互独立。Host 只在验证的 route/request/terminal 边界生成 immutable `ModelRouteLease`、`UsageContextSnapshot`、`UsageSample`；Usage 不查询 Provider，也不从字符串、Session、widget 或 quota 推测模型。Usage Manager 以根配置顺序组合 unique source Pack 的有界 row/card 到 `status.trailing/usage.summary` 与 `panel/usage.details`。

## 4. Provider Pack 冻结点

Pi Provider Pack 固定 `@earendil-works/pi-coding-agent`/`pi-ai` `0.84.4`，使用签名 snapshot 和 bounded metadata；不存在 provider-list API。Synthetic Provider、Web 与 Usage Pack 是三个独立 Pack，却共同声明 canonical `synthetic/<account-id>`，由 Host 复用一份 credential 并为各自精确 authority 单独批准。

Minimax CN live smoke 仅在 T11 后显式 opt-in，默认 skip 且不进入 CI；它走正式 Providers Manager、Host ProviderPack Service、签名 Pi generation、`minimax-cn` 与 `anthropic-messages` 路径。`minimax.txt` 不得读取、打印、复制或 stage；secret 只能由用户经 anonymous pipe/stdin 交给 redacted Host harness。
