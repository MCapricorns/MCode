# 工具系统与权限

> 对应 crate:`mcode-tools`
> 参考:grok-build `mcode-tools` 同构设计(Tool/ToolDyn/schemars/ToolStream)

## 1. Tool trait

```rust
/// 七个 canonical builtin 实现此 trait；动态贡献由 Host adapter 实现 ToolDyn，不是 Pack ABI;async fn in trait(edition 2024)
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    type Args: for<'de> Deserialize<'de> + schemars::JsonSchema;
    type Output: Serialize;

    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn prompt_snippet(&self) -> Option<&str> { None }   // 进 system prompt 的用法提示(pi 的 promptSnippet)
    fn search_access(&self) -> Option<SearchAccess> { None }  // grep=Content, find=Metadata
    fn requires_search_preflight(&self) -> bool { self.search_access().is_some() }
    fn file_access(&self) -> Option<FileAccess> { None }  // read=ExistingContent, write=ExistingOrMissing
    fn requires_file_preflight(&self) -> bool { self.file_access().is_some() }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError>;
}
```

单源 schema:`schemars` 派生一份 JSON Schema,同时用于(1) LLM tool spec;(2) 运行时参数校验。grok-build 已验证此模式。

## 2. 类型擦除与注册

```rust
#[async_trait]
pub trait ToolDyn: Send + Sync {
    fn spec(&self) -> ToolSpec;      // name/description/params_schema
    fn search_access(&self) -> Option<SearchAccess> { None }
    fn requires_search_preflight(&self) -> bool { self.search_access().is_some() }
    fn file_access(&self) -> Option<FileAccess> { None }
    fn requires_file_preflight(&self) -> bool { self.file_access().is_some() }
    async fn execute_dyn(&self, args: Value, ctx: &ToolCtx, out: &mut ToolStream)
        -> Result<ToolResult, ToolError>;
}

// blanket impl:任何 Tool 自动成为 ToolDyn(负责 Value → Args 反序列化 + schema 校验)
// Value 仅是 Host adapter 的 schema 校验载体，不是 Manager/Pack/FeatureService ABI。

pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn ToolDyn>>>,
}
// 规则:canonical 名称固定且不可覆盖；动态 namespaced 条目只能由已验证的 Manager+Pack typed contribution 经 Host adapter 加入
// 能力标记:concurrency: Exclusive | Parallel,mutates_fs: bool,
// search_access: Option<Content | Metadata>(默认 None;grep=Content,find=Metadata)
// requires_search_preflight: bool 由 search_access 是否存在派生
// file_access: Option<ExistingContent | ExistingOrMissing>(默认 None;read/write)
// requires_file_preflight: bool 由 file_access 是否存在派生
```

## 3. 流式输出

```rust
pub struct ToolStream { tx: mpsc::Sender<ToolStreamItem> }

pub enum ToolStreamItem {
    Progress(ToolProgress),   // 增量文本/计数,UI 实时渲染
    Terminal(ToolResult),     // 有且仅有一个,流到此结束
}
```

约束(抄 grok-build):`[Progress*]` 后恰好一个 `Terminal`;`ToolResult { content, is_error, details }` 的 `details` 只进 UI 不进 LLM。dispatcher 的 biased select 只为防止 progress producer 饿死已 ready 的 execute 而优先 poll execute:返回值尝试原子 claim terminal 后会停止所有 clone 的后续 progress,但 channel FIFO 仍会先 drain 已入队的 Progress 再观察 Terminal;自终结工具先送出的 Terminal 保持 first-terminal-wins。

## 4. ToolCtx 与渲染描述

```rust
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub prepared_search: Option<Arc<PreparedSearch>>,
    pub prepared_file: Option<Arc<PreparedFile>>, // host-owned; not exposed to Pack
}

// 渲染描述(UI 中立,pi 的 renderCall/renderResult 的协议化):
#[non_exhaustive]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum RenderBlock {
    Text(String),
    Markdown(String),
    Diff(Diff),                 // Diff { path, hunks: Vec<DiffHunk> }
    Table(Table),               // Table { caption, headers, rows }
    Tree(Tree),                 // Tree { root: TreeNode };节点可递归
    Progress(Progress),         // label/current/total/state
    Error(ErrorBlock),          // title/message/details/retryable
}
```

`ToolCtx` 不持有 session 对象或 event emitter；SessionPack 若发起调用，只能经 Host 注入有界 operation scope。`RenderBlock` 是可 serde 序列化的纯数据,不含回调、终端句柄或后端样式。每个既定变体都实现 `to_plain_text(width)`:清除 ANSI/OSC 等终端控制序列,按 `unicode-width` 的整串显示宽度截断,并在固定标量预算内保留扩展字素边界;单行宽度不超过 `min(width, 4096)`,总行数不超过 4096,截断标记为 `…`。它不定义通用 `Widget`/JSON 扩展点；Pack UI 必须走其 typed Service，未知或不合规 DTO 在 Host 处拒绝。

终端投影属于 `UiPackService` / UiPack 的 typed UI 路径，不是 Core 到 ratatui 的直接后端。当前 `mcode-tui::render` 仅是迁移前实现状态；目标终端 substrate 在不支持 Unicode 时使用 ASCII 框线、将非 ASCII 内容替换为 `?`，并把截断标记改为 `.`，保证交给终端的整个缓冲区只含 ASCII。

## 5. Dispatch

canonical builtin 中已注册且 schema 合法的调用由 `ToolRegistry` / agent loop **直接执行**。已验证的动态 namespaced 调用由 Host adapter 转交其 Manager+Pack typed Service；Pack 不直接操作 Registry，也不能覆盖 builtin。Core 没有 `PermissionEngine`、`PermissionMode`/`Rule`/`Prompt`、持久 grants、默认 Ask、headless deny,也没有 `--yolo`。未知工具、尝试覆盖 canonical 名称、无 owner/provenance 的动态贡献、非法参数、取消和工具错误仍按生命周期失败,回填为 `is_error` tool result,不是用户授权。

在 builtin 或 Host adapter dispatch 前，Host 都只按已校验参数绑定一次 `PreparedSearch` / `PreparedFile`（能力绑定，不是授权）；不能复用旧参数、路径或句柄。动态 contribution 的 schema、名称空间、Manager、Pack ID/version/hash/generation 与资源边界均须在 adapter 建立时固定；通用 JSON 不可作为 Pack 语义通道。Ask 不属于工具 dispatch；产品 Ask 仅由 `com.mcode.ask` Pack 经 typed Service 实现。

内建 `write`、`edit`、`shell` 与 `exec` 持有同一 process-wide execution lease,串行化同进程写者与 identity-bound 启动;`edit` 的规划、规划失败与 no-op 路径不获取 lease,只有形成真实 changed payload 后才在 publication 前获取。`write`/`edit` 把 lease 移入 supervised publication worker,丢掉外层 future 也不会在仍可能 publish 时释放。不把跨进程 advisory lock 或 per-file 队列夸大为安全边界。生命周期错误不得回显 `write.content` 或其他正文。

## 6. 内建工具清单

read / write / edit / shell / exec / grep / find —— 唯一的 builtin，全部实现 `Tool`。Registry 还可容纳 Host adapter 的 namespaced 动态条目，但 Manager/Pack 只能经有界 typed contribution 请求它们，不能直接注册或覆盖。

`edit` 在一次 snapshot 上规划 `operations`：`literal`（memmem / Aho-Corasick）、`regex`（Rust regex 捕获替换）、`line_range`（1-based 行）、`fuzzy`（独立 op，不是 literal 的 occurrence 开关：对 token/空白归一化后的形式做带状 Levenshtein，`max_distance` 必填且仅允许 1..=3；次优排名会扫描到 `max_distance + margin - 1`，只提交唯一最佳且相对次优满足 margin 的匹配，margin 为 `1 + floor(normalized_len / 8)`，否则拒绝并返回有界 near-miss 行预览；token 元数据和 attempted candidate windows 分别有硬上限，tokenization/scoring 轮询取消；不得静默猜测或回退到 exact memmem）、`ast`（tree-sitter 对 snapshot 只 parse 一次并供同语言 op 共用；每个 query 同时受 in-progress、completed match/capture 与取消上限约束，`language` 可写或按扩展名推断；先筛选目标 named capture，再按跨平台可移植的 ASCII capture-name 语法（首字符 `[A-Za-z0-9_-]`，后续另允许 `.?!`）绑定目标节点的 `@capture`（`@@` 产生字面量 `@`）并直接进入统一 replacement 预算；其他重复 capture 引用视为 ambiguity；禁止 whole-file pretty-print；发布前 reparse；syntax error 遍历对 inspected nodes/error 数量设硬上限并轮询取消，使用 before/after 各自去 BOM 的坐标、累计 edit delta 与完整 range/error-or-missing identity 做多重集比较，若引入原先没有的 error 则拒绝且不写盘）。所有 op 共用同一 snapshot 的 byte-range 排序与 overlap 拒绝，预算在 `reserve_planned` 时强制（`MAX_MATCHES` / `MAX_WRITE_BYTES`），query 上限 64 KiB，一次持 lease 的 `write_file_with_lease` 按 snapshot revision 发布。Grammar 由 Host 经 `cc` 编译（Windows 需 MSVC，Unix 需 C compiler）。

内建工具的产品目标平台统一为 Windows、Linux 与 macOS；交叉编译只算补充证据，完成声明必须有对应平台原生 runner 的 runtime/权限/竞态测试。`read`/`edit`/`write` 在内建实现上分别声明 `file_access = ExistingContent / ExistingContent / ExistingOrMissing`,`requires_file_preflight` 由该能力派生。路径锚定已打开的 session cwd 句柄,逐组件 no-follow,拒绝 `..`/symlink/reparse/ADS/device/FIFO/socket 与 mount escape。hidden/dotfile 可读写,不套用 Search 的 ignore 策略。read 保留 1-based offset/limit 与 2000 行/50KiB 截断,完整 raw bytes 做 Blake3,返回不泄露字段的 opaque `mcode-rev1-` revision(镜像 `details.revision`)。write 对 missing 目标原子 create-only(可安全创建 missing parents);existing 必须 `expected_revision` 匹配或 `overwrite=true`,二者同时设置则拒绝。默认无条件覆盖已移除。existing hardlink 通过新 inode 原子替换断开该 directory entry,并报告 `detached_hardlink=true`。进程内 CAS 由全局写锁串行化同进程写者;不把跨进程 advisory lock 夸大为安全边界。Unix 上 payload 临时文件从创建起即持有 `0600`(显式 `fchmod` 同时压掉父目录 default ACL 可能赋予的 group 位),并以私有模式完成 rename;missing 目标的有效 mode 由独立的、永不写入内容的 `0666` probe 文件探测(内核 umask/default ACL 生效后 `fstat` 记录,probe 在失败路径上由 guard 兜底删除,在成功路径上显式、可失败地删除——删除失败会报告失败而非吞掉,绝不把残留静默呈现为成功;观察者即使持有其句柄也只能读到空文件),发布后经保留句柄恢复记录值;existing 目标的 mode/owner 同样在 rename 后经保留句柄恢复,因此 payload inode 在发布前不存在任何 group/other 可读窗口。发布 rename 是不可逆操作,紧邻 `publish_replace`/`publish_create_only` 前有最后一道取消闸门,该闸门拒绝已经观察到的取消/超时；一旦进入单次原子 publish syscall 就不能回滚。非 renameat2 平台 `linkat` 发布后删除临时名失败会返回包含清理错误的失败而非成功。发布后 `finish_write` 要求重开名称仍解析为刚发布的 temp inode 且内容哈希与写入内容一致,外部同长度替换(同 inode 改写或换新 inode)都以失败报告,不会为其生成 revision。新目录以 `0777` 创建交由 umask 收紧,不再固定 `0644/0755`。Linux 使用 `openat2(BENEATH|NO_XDEV|NO_SYMLINKS)`、`fstat`/`st_dev` 和 `renameat2(RENAME_NOREPLACE)`；macOS(Apple Silicon)使用 `openat(O_NOFOLLOW|O_DIRECTORY)` + `fstat`/`st_dev` 验证,create-only 优先 `renameatx_np(RENAME_EXCL)`,耐久性用 `F_FULLFSYNC`(失败不静默降级)。Windows 用相对 NT API,身份为 128-bit FileId+volume,DACL 经 handle 上的 Get/SetSecurityInfo 保留(含 protected DACL 控制位,SACL/integrity label 不声称)。payload 临时文件在 `NtCreateFile(FILE_CREATE)` 时使用受保护的 owner+SYSTEM DACL,且只共享 `FILE_SHARE_DELETE`(不共享 READ),因此 permissive parent 下的外部读者在首字节前也无法打开 payload;existing 目标在写完后、publish 前把源 DACL/属性复制到仍持有的 temp 句柄上,并显式镜像 `PROTECTED`/`UNPROTECTED` 控制位,避免把继承 DACL 冻成 protected。missing 目标在 publish 后从同目录 never-written probe 经保留创建句柄复制继承 DACL(含 unprotected 控制位);probe 的复制与删除始终走该句柄上的 `DELETE`,不按名称重开。payload temp 创建成功后立即在原创建句柄上武装 checked delete-on-error guard,并保留带 `DELETE` 的重复句柄;失败路径通过显式 finalizer 合并 primary/cleanup 错误,`Drop` 只作 unwind 兜底。payload 清理使用忽略 read-only 的 POSIX disposition(旧系统回退为先在句柄上清除 `FILE_ATTRIBUTE_READONLY` 再用旧式 disposition),经保留 DELETE 句柄完成,不按名称重开。cleanup 失败会进入返回错误,此时可以留下已记录的残留,不得把残留伪装成干净失败。发布后关闭所有拒绝 READ 共享的 payload 句柄再按名验证。canonical 工具名不可被 Pack 覆盖。原生 PASS 只认 `.github/workflows/ci.yml` 的 Windows x86_64 / Linux x86_64 GNU / macOS Apple Silicon runner 上的 `cargo test --workspace --locked`;交叉编译只算补充证据。Android 与 BSD 不是产品目标。特权 FS fixture 维持 `#[ignore]`,CI 不设置 `MCODE_PRIVILEGED_FS_TESTS`、不传 `--ignored`。

`grep`/`find` 在内建实现上分别声明 `search_access = Content / Metadata`,`requires_search_preflight` 由该能力派生。dispatch 按 registry 里的实际对象(不是工具名)决定是否绑定本地 search capability。local search dispatch 必须在可取消 worker 上把 session cwd + `path` 解析成 `PreparedSearch`,用 parent handle + identity 得到唯一 on-disk spelling。missing、sharing、alias、ignore 或唯一拼写证明失败均终止,没有 lexical fallback;成功后同一 prepared capability 只能被执行消费一次,且 `Content`/`Metadata` 模式必须一致。`find` 的最终普通文件保留 metadata-only handle,目录在 metadata/content 两次打开 identity 相同后才用于 listing;`grep` 直接保留 content handle。Windows 身份使用 `GetFileInformationByHandleEx(FileIdInfo)` 的完整 128-bit `FILE_ID_128` 加 volume serial,查询失败 fail closed。执行阶段用与外层 timer 相同的绝对 deadline 刷新 limiter,已累计的 ignore/handle 预算保留。canonical 工具名不可被 Pack 覆盖。

Git 边界按句柄相对的父目录有界发现:Unix 用 `openat("..", O_NOFOLLOW)`;Windows 对候选 parent 做 no-follow 逐组件打开,再从该 parent 精确重开 child 并比较完整身份。只有结构化的文件系统根才停止上溯;打开失败、重命名/reparse race 或身份不匹配 fail closed,不把 `NotFound`/`InvalidInput` 猜成根。cwd 在仓库子目录时仍加载上级 `.gitignore`;linked worktree 的 `.git` 文件会解析 `gitdir:` 与相对/绝对 `commondir`,再读 common-dir 的 `info/exclude`。无法安全建立边界时终止,不静默省略。ignore layer/rule 在 `add_line`/compile 前按共享 limiter 原子预留;目录枚举在 materialize 前预留同一 entry 预算,Unix canonical-name / Git 父级扫描也计入该预算;显式目标组件深度在打开前受 `max_walk_depth` 限制。find/grep 结果堆 intern 每文件 path key,并有累计字节预算;path intern 只在该 path 仍留在 provisional/全局结果堆时占预算,discard、零保留行、最后一条 eviction 与 `max_results=0` 均归还,不会在空堆上耗尽预算。Windows hidden 查询失败计入 I/O/终止,只有真实 hidden 才静默跳过。

取消 authority 是 per-invocation `CancellationToken`。Unix 的 pollable read 还监听 per-worker wake socket,因此即使运行中 `SIGURG` 被改成 `SA_RESTART` handler 或 `SIG_IGN`,取消/drop/abort 仍能唤醒这些 read;`SIGURG` 只作为已进入其他内核 syscall 的 best-effort wake。启动握手仅在 disposition 为 DFL/本 crate handler 时安装 `SIGURG`,显式 `SIG_IGN` 或已有 foreign handler 都 fail closed;最后一个 owner 也只在当前 handler 仍属本 crate 时恢复旧 disposition,不覆盖后来安装的 handler。同进程 native 组件若在调用运行中篡改 signal disposition,可能阻止一个 non-pollable Unix syscall 被中断;WASM 插件没有该能力,该 native-host 约束以及真正不可中断的 kernel D-state 不作有限时终止承诺。Windows 在握手中取得 owned duplicated thread handle,用 `CancelSynchronousIo`。任一步失败都不执行工作。dispatch 用本地 pinned execution future 与 progress receiver 的结构化 `select!`,对 execution future 的 poll 与析构都 `catch_unwind`,不 `tokio::spawn` detach;捕获到的 panic payload 在 catch 边界内规范化为 owned 消息,`String`/`&str` 复制后安全释放,未知 payload 在生成通用消息后 `mem::forget`,避免其 Drop 再次 panic;工具 poll panic 以及完成后的 execution future Drop panic 映射为错误 `ToolResult`;poll panic 后 cleanup Drop 若再次 panic，保留首次 poll panic 并吞掉第二条已规范化的析构消息；abort/drop 取消路径也吞掉已规范化的析构消息，避免 unwind prompt task。supervisor 从启动起唯一拥有 worker join 与平台 authority;调用 future 被 drop/abort 时发布 cancellation,supervisor 继续唤醒/中断并 join,再释放句柄,不会把 worker ownership 无声丢弃。取消或超时在发布结果前转为 execution error,不返回带 `stopped_early` 的成功部分报告。ignore parse/build/read 或 nested ignore 边界失败返回终止性 tool error;普通逐路径 I/O 仍可返回已确认结果,但模型正文会明确标记 incomplete lower bound。

`exec` 使用 `program`、逐项保真的 `args[]` 与可选 `timeout_secs`,不插入 shell、不解析 quoting/expansion,也不执行 shebang、batch 或隐式 interpreter fallback。只接受经 magic/header 验证且属于当前平台的 PE、ELF 或 Mach-O;bare name 只搜索 host `PATH` 中 absolute entry(跟随最终 symlink/reparse 到 regular target),显式 path 按 session cwd 解析。已注册且 schema 合法的内建 `exec` 在校验后直接 dispatch,不经过 Core 权限提示,也不预实现 policy hook。Preparation 一次性快照 cwd、排序后的 OS runtime/locale/用户/terminal/temp allowlist 与从 absolute host entry 重建的 `PATH`;bare-name 解析和三个平台 spawn 均消费同一不可变快照,spawn 阶段不再次读取进程环境。credential 与 loader/interpreter injection 变量不传播。该工具不是 sandbox:子进程仍以当前用户拥有正常文件和网络权限,同账号外部进程不在边界内。

pin 跟随最终 alias,打开 regular target,从 retained handle 记录 canonical path、native file identity 与 SHA-256,并在 launch 前重算 digest。版本化、domain-separated、长度分帧的 invocation SHA-256 绑定 canonical native path、文件 identity、image digest、effective `argv[0]`、逐项 argv、cwd 和排序后的 effective environment;结果只公开 digest 与有界的 key/value native-byte length 摘要,不公开环境值。Structured Exec 的 Linux 启动仅支持 x86_64 GNU（非 musl/Android/BSD）:从 retained fd 执行 `execveat(AT_EMPTY_PATH)`,并在 `pre_exec` 用 raw `close_range(CLOSE_RANGE_CLOEXEC)` 把 fd ≥ 3（含 launch fd）标 CLOEXEC,ENOSYS/EINVAL/任意内核错误 fail-closed,不回退到继承。Windows 用 Unicode `CreateProcessW` suspended launch,在 dedicated kill-on-close Job 中校验 process image handle/identity/digest 后才 resume;macOS Apple Silicon 以 pinned canonical path 做 suspended `posix_spawn`(Darwin 无法从描述符执行:`/dev/fd/N` 的 `execve` 恒为 `EACCES`),校验 parent/stopped state、`proc_pidpath` 与 canonical path 身份、architecture 与 digest 后才 `SIGCONT`。stdout/stderr 并发排干且总展示上限 50 KiB;timeout/cancel 等待 terminate-and-reap并传播 teardown 错误;future drop 把 pin/lease 所有权交给 supervisor。macOS waiter 与 termination 在共享 live/reaped state lock 下协调,只有尚未 reaped 的 PID/PGID 可收到信号。

内建 `write`、`edit`、`shell` 与 `exec` 持有同一 process-wide execution lease。`edit` 的规划、规划失败与 no-op 路径不获取 lease,只有形成真实 changed payload 后才在 publication 前获取;`write`/`edit` 随后把 lease 移入 supervised publication worker,因此丢掉外层 future 也不会在仍可能 publish 时释放。`shell`/`exec` 在 pin 前获取 lease,并从 pin 持有到 child cleanup 完成;一个 cleanup owner 在 terminate-and-reap 期间保留 pin 与 lease。deadline 在不可逆 spawn 前关闭 gate;若平台已经真正 launch/resume,timeout 结果明确报告程序已启动并被终止,只有未跨过该平台边界时才报告未启动。

`shell` 是唯一公开的平台 shell 工具名,没有 `bash` alias,继续使用 `command`/`timeout_secs`;实际后端在 macOS/Linux 按 `/bin/bash` → PATH `bash` → `sh` 选择,Windows 只支持 PowerShell 7 `pwsh.exe`,`details.shell` 固定记录该可执行文件,没有其他 Windows shell 兼容分支。每次调用只快照一次 cwd、allowlisted environment 与重建后的 PATH,所有候选都通过 Structured Exec 的 retained pin、identity/digest 校验和 contained spawn 启动,同时保留所选候选 spelling 作为 effective `argv[0]`;只有 typed executable `NotFound` 允许尝试下一候选,其他解析、权限、identity 或 image 错误都 fail closed。PATH 解析得到的 Windows `pwsh.exe` 虽会 pin 并校验 identity/digest,仍是同账号 host input,不是 sandbox 边界;managed cache 才是经认证的 Microsoft distribution fallback。Windows PATH `NotFound` 时按仓库 JSON matrix 固定的版本、架构、asset URL/字节数/SHA-256,按需把 Microsoft 官方 portable ZIP 配置到 `<mcode-home>/bin/powershell/`;仅允许 HTTPS,限制重定向、总时长、下载量、entry 数和解压量,校验 archive SHA-256 与必需 signed runtime chain 的 Authenticode,拒绝 traversal/链接/重复路径,在跨进程锁下 staging 后原子 rename。安装记录保存完整文件清单、大小、mtime 与逐文件 SHA-256;复用时拒绝缺失、额外或大小变化的文件,始终重算必需 runtime 文件并在 mtime 变化时重算其他文件,因此 `pwsh.dll` 等依赖缺失或损坏会触发重建。离线且没有有效缓存时失败关闭。配置单测只注入本地 ZIP;Windows real-shell e2e 仅在 PATH 已有可用的 `pwsh.exe` 时运行,默认测试不会触发下载。

Windows 将用户脚本本身直接编码为 UTF-16LE Base64 后交给 `-EncodedCommand`,不再执行 `UTF8Encoding::new`、`ScriptBlock::Create`、`Encoding.GetString` 或 `Convert.FromBase64String` launcher;因此 `using namespace/module/assembly` 仍是脚本首条,ConstrainedLanguage 允许的基础 cmdlet 也不会被 transport 阻断。`-ExecutionPolicy Bypass` 只处理 execution policy,不假定其解除 WDAC/AppLocker 的 language mode。命令行预算精确计入可执行路径、固定参数、空格、Base64 payload 和 UTF-16 NUL;输出只按 UTF-8/带 BOM UTF-16 安全解码,不使用 ANSI code page。

`timeout_secs` 从工具调用开始计时,覆盖按需配置、install lock 等待、下载与命令执行;预先取消的调用在 shell lookup、配置和 spawn 之前返回。解压、完整缓存校验、Authenticode 与原子发布统一在 blocking task 中执行,外层可在 await 期间观察 deadline/取消并丢弃后续 shell-spawn continuation,因此不会越过已到达的超时/取消启动用户脚本。blocking task 本身不可强制中止,会持有 staging 与 install lock 到结束并可能在后台完成缓存。超时/取消时,Unix 先并发排干 stdout/stderr,之后才 poll/reap leader;只要脱组后代仍持 pipe,live/zombie leader 就继续保留 PID/PGID。终止前必须同时满足当前 `Child::id` 等于保存的 leader、`getpgid(pid)` 等于保存的 PGID、PGID 大于 1 且不是调用者组,否则拒绝 `killpg` 并只走 Child fallback。`setsid` 等脱组进程不在终止保证内,读端会在 containment 清理后关闭。

Windows 以 `CREATE_SUSPENDED` 启动 shell,优先让它继承宿主 Job 并加入 Windows 8+ nested dedicated Job;仅 nesting 失败时尝试 `CREATE_BREAKAWAY_FROM_JOB`,且任一路径都必须在 resume 前成功 assign。专用 Job handle(启用 `KILL_ON_JOB_CLOSE`)是后代 containment/termination 的唯一 authority:超时/取消执行 `TerminateJobObject`,再通过 `CreateProcessW` 返回并由 cleanup owner 持有的 process handle 等待和回收 leader;不使用 `taskkill`、`OpenProcess(PID)` 或任何裸 PID tree cleanup。启动路径直接对 `CreateProcessW` 返回的 owned `hThread` 调用 `ResumeThread`,不通过 PID 重开进程或枚举线程。通过外部 broker/service 创建、未继承该 Job 的进程不在保证内。

## 7. 待决策

- [ ] `concurrency: Exclusive` 工具(`shell`/`exec`)与 parallel 工具的混合调度策略

MCP工具必须由`com.mcode.mcp` typed Service提供bounded namespaced contribution，再经Host adapter进入Registry；不得走旧direct transport。Ask只经`com.mcode.ask` typed Service运行，不是工具授权钩子。
