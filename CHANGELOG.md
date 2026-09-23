# Changelog

显著变化从 `0.4.5` 记起。更早的发布记录已作废，不再保留。日期为发布日（UTC）。
发布说明在发版时手写，与本文件相互独立。

## [0.4.7] - 2026-09-23

### Changed

- 删除全部测试套件与仅被测试引用的代码（测试钩子、注入缝隙、dev 依赖），共约 1.3 万行；测试将在之后按需重写。
- 工作区内 21 个 `mod.rs` 全部改为现代模块文件布局，并以工作区级 `clippy::mod_module_files` 强制不再出现。
- 删除从未接入产品的 Pack/WASM 插件子系统（约 1.4 万行死代码）。配置库只保留实际使用的设置、密钥、UI 状态、待办、会话与子代理文档。
- 删除只有测试在用的接口：agent 的 steer/follow-up 队列、`AgentHandle` 与队列模式（消息排队由应用层调度负责，`TurnOutcome` 只剩 Completed / Aborted），桌面的 `MentionDismissed` / `UnboundSessionsAssigned` / `DismissError` 动作。
- 编译提速：HTTP 客户端改用 OS TLS 并去掉 http2 特性；`base64` 对齐到 0.22；移除未使用的 `syn` 依赖。
- 发布构建打开 fat LTO、O3 与单 codegen unit，追求最佳运行性能。
- CI 收敛为单个 workflow：push 只做 fmt + clippy 质量门禁（不再跑测试）；发版改为手动触发，发布说明手写、不由 changelog 或提交记录生成。
- 超过 1000 行的桌面端源文件按职责拆分；`exec` 的三个平台实现共享同一份摘要复查与 C 字符串组装助手。
- 设置页头部不再显示 revision 版本号。

### Fixed

- Querit 网页检索的 `crawlTimeout` 改为按秒发送。
- 深浅主题下选中的文本保持可读。

## 0.4.5 - 2026-09-23

### Changed

- 窗口启动时落在主屏幕可用区域的正中。屏幕比默认尺寸小的时候，窗口会先缩小再居中。
- 模型菜单和思考菜单从右侧按钮旁边打开。
- 已经保存的网页搜索密钥不再显示在输入框里，只留一把锁。点锁可以换一把新钥匙，旧密钥不会被填回来。
- MCP 的添加收进二级页：从目录添加、导入 JSON、自定义服务器。已保存的密钥同样只显示锁。
- 思考按钮写出 Thinking / Thinking off / Thinking on，不再只写 On。

[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.4.7...HEAD
[0.4.7]: https://github.com/MCapricorns/mycode/releases/tag/v0.4.7
