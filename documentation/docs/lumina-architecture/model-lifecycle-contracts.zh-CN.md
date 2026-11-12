---
title: lumina 模型生命周期架构和接口契约
sidebar_position: 11
---

# lumina 模型生命周期架构和接口契约

本契约是[完整升级计划](./model-lifecycle-master-plan.zh-CN.md)的实现约束。下文描述完整目标契约；已经实现或接入的部分以[实施进度](./model-lifecycle-progress.zh-CN.md)和实际源码为准，其余仍待交付。原有接口和文件在专门的迁移条目中列明。服务端是来源、状态、安装准入和能力准入的唯一权威。

## 模块和调用边界

新增模块首先放在 `crates/lumina-local-inference/src/lifecycle/`：`domain`、`modelscope`、`gguf`、`classification`、`dependencies`、`compatibility`、`repository`、`jobs`、`installer`、`validation`、`service`。具体文件按职责拆分，不创建承载所有逻辑的单一大文件。

- `management.rs` 保留旧接口适配，逐步调用 lifecycle 应用服务。
- `hf_models.rs` 的 ModelScope HTTP、解析和 GGUF 发现职责迁入对应模块；旧名称先保留转发，迁移完成后删除不可达实现。
- `lumina-download-manager` 只负责受计划约束的字节传输、分段恢复和文件校验，不决定模型 INSTALLED 或 READY。
- `backend.rs`、`lib.rs` 和 `llamacpp/` 承载现有运行时，扩展为可描述支持范围的 Adapter；新增任务型 Adapter 与聊天实现隔离。
- `lumina` 组合根负责把现有配置、密钥、路径和应用级 Provider 注入服务，避免 lifecycle 反向依赖 Agent。
- `crates/lumina/src/acp/server/` 只做协议映射和应用服务调用；不复制解析、安装和依赖规则。
- SQLite 借用项目已有使用经验，模型业务使用独立数据库和迁移，不能复用 MCP 领域记录作为模型记录。

## 来源和文件策略

ModelScope Provider 暴露 `search`、`readRepository`、`resolveRevision`、`listFiles`、`readAllowedMetadata`、`openArtifact`。具体 HTTP 路径和分页方式由 ML-08 依据真实响应确认；业务不得依赖“名称恰好与某平台仓库一致”。

每次获取记录原始请求的 revision、服务端解析后的不可变版本、获取时间、文件清单摘要和响应证据引用。仓库字段不能取得时为 UNKNOWN，不能填 master 作为已解析的版本。不可变版本或足以核验其身份的证据不可取得时，只允许发现和展示，不生成可执行安装计划。

| 文件用途 | 接受规则 | 处理 |
| --- | --- | --- |
| 模型和组件权重 | 扩展名为 gguf 且内容通过 GGUF 结构解析 | 进入组件图并记录角色 |
| tokenizer 元数据 | 明确登记的 tokenizer JSON、词表文本、merges 等，Adapter 声明用途 | 设大小限制，按内容检查，不作为权重 |
| 运行配置 | 明确登记的 JSON、YAML、模板、调度配置 | 按结构解析，禁止执行自定义代码 |
| 文档 | ModelScope 返回的说明及许可证文本 | 作为证据或展示，不参与权重完整性 |
| 其他权重和未知二进制 | Safetensors、ONNX、PyTorch、未知格式等 | 不下载；若为必需组件则阻塞 |
| 归档和脚本 | 模型仓库的 zip、可执行文件、Python 和 shell 脚本 | 不作为模型安装依赖执行或解包；Runtime 二进制由发行渠道单独管理 |

对大小写、双重扩展名、文件内容伪装和跨目录路径进行归一化与检查。Provider 校验重定向来源：ModelScope 官方下载设施可使用经维护和验证的 CDN 映射，第三方模型平台不是允许的回退来源；变更该映射须有真实响应依据。令牌只注入其作用域内的请求，不写入计划、数据库 URL 或日志。

2026-11-01 的真实固定版本 GGUF 下载观察到 `modelscope.cn` 返回 302，目标为 `cdn-lfs-cn-1.modelscope.cn`。ML-17 下载流实现必须支持有版本和真实证据的官方 CDN 映射：仅 HTTPS/443、最多三次跳转，禁止 URL 用户名/密码及向 CDN 转发来源凭据；签名下载 URL 不写入计划、事件或日志。文件仍绑定原 ModelScope 仓库、完整 Revision、文件路径、大小及来源哈希，CDN 不成为另一个模型来源。当前验证资产已按此约束核对完整 SHA256；生产 Provider 的清单请求仍禁止跳转，产品下载 worker 和受限账号跳转验证尚未接入。

## 数据契约和身份

全部时间为 UTC，展示使用用户时区。业务 ID 使用不可变内部 ID；`owner/repo:quant` 只作为旧模型别名，不再承担安装版本身份。布尔值有明确默认值；UNKNOWN 用枚举表达，不能用缺省 false 冒充不支持。

| 实体 | 必需字段和约束 |
| --- | --- |
| `RepositorySnapshot` | source 固定 modelscope；真实 repoId；requestedRevision；resolvedRevision；fetchedAt；filesDigest；accessState；证据引用 |
| `Artifact` | snapshotId；远程相对路径；文件用途；大小及其已知状态；可选来源 SHA256；下载后必有 computedSha256；GGUF 解析摘要；校验级别 |
| `ModelDefinition` | 内部 ID；显示名；来源快照引用；架构及证据；模型结构 dense/moe/unknown；能力集合；许可证事实 |
| `Variant` | modelId；主文件或分片集合；逐 Tensor 类型摘要；显示量化；来源版本；不能以量化名独占仓库 |
| `Component` | role；artifactIds；模型归属；兼容元数据；同仓库或跨仓库来源；关联规则版本与验证证据 |
| `DependencyEdge` | fromComponent；toComponent；requirement；条件表达式；证据；匹配结果；失败原因；禁止环 |
| `InstallPlan` | schemaVersion；planId；model/variant；目标能力；完整组件图；快照；Runtime build；兼容分析；空间预算；planDigest；规则版本；有效期 |
| `Installation` | 稳定 installationId；明确的 activeVersionId；旧 ID 别名；不绑定单个不可变目录快照 |
| `InstalledVersion` | 全局不可变 versionId；installationId；plan/model/variant；manifestDigest；正式版本目录；引用的 blob；提交状态；installedAt；历史版本保留 |
| `Job` | jobId；操作；installation/plan；幂等键；阶段；generation；状态版本；lease；heartbeat；事件序号；错误；恢复检查点 |
| `InstallationInput` / `InstallationIntent` | jobId；原计划摘要；绑定时间与规则版本；installationId；全局不可变 versionId；规范化受管根目录；固定 ModelScope 来源或精确缓存文件映射；不得保存令牌或任意下载 URL |
| `FileJob` | jobId；artifactId；目标；逐文件状态；分段检查点；重试；expected/actual bytes；computedSha256；速度及 ETA |
| `RuntimeBuild` | adapterId；引擎版本和源码提交；绑定版本；平台架构；features；二进制摘要；能力声明版本 |
| `CapabilityEvidence` | installationId；文件摘要集合；runtimeBuildId；launchDigest；能力；验证套件版本；输入输出引用；判定；测量；时间 |
| `LegacyAlias` | legacyId；原始配置备份引用；导入 ID；所有权；来源可追溯性；验证状态 |

规则声明支持证据和本机真实运行证据分开记录。来源未提供 SHA256 时不伪造来源哈希：固定版本、大小和结构满足策略后可以下载，下载后计算本地 SHA256，并标明“本地校验，来源未提供哈希”。本地计算不能证明远程真实性，也不能替代版本固定。

GGUF Parser 解析 Header、版本、键值类型、数组、Tensor 名称、维度、类型、偏移、对齐与边界，实施合理资源限制。支持已声明的版本，其他版本返回不支持；对未知元数据保留原值或摘要。分片同时核对名称集合和内容中的 split 信息；缺失标准字段时使用经验证规则，不能只按同量化合组。参数量记录计算依据，不能把文件字节数换算结果冒充准确参数量。

## 分类和组件兼容

分类至少具有 taskFamilies、architecture、modelStructure、modalities、componentRole、evidence 和 confidenceState。GGUF 内容优先确定格式及架构；仓库说明辅助解释用途；名称相似度仅用于候选检索。

组件角色包括 main、vision_projector、mtp、dflash、vae、text_encoder、audio_encoder、audio_decoder、vocoder、codec 和受控扩展角色。角色的必需性依目标能力与 Runtime profile 决定，不能给所有模型一律添加 mmproj 或 MTP。

| requirement | 语义 |
| --- | --- |
| REQUIRED | 当前目标能力缺少即不完整 |
| OPTIONAL | 缺少不影响该目标；用户明确选用后计入计划 |
| CONDITIONAL | 由版本化条件表达式判定，执行计划前必须求值为确定结果 |
| UNSUPPORTED | 格式或 Runtime 不支持；不能当作可忽略的必需依赖 |

解析顺序：同仓库真实文件 → 仓库允许配置和 GGUF 线索 → 版本化支持档案 → ModelScope 跨仓库候选检索 → 元数据与兼容规则匹配 → 生成证据化依赖图。跨仓库候选必须再次获取真实仓库、版本和文件清单。

规则可比较架构族、模型来源/训练关系、embedding 与投影维度、Tensor 形状、tokenizer 身份、Runtime 版本和明确组件版本。并非所有规则适用所有角色；未取得必需证明时为 UNVERIFIED，不能因文件名接近而自动通过。

完整性只取 COMPLETE、INCOMPLETE、INCOMPATIBLE、UNVERIFIED。只有选定目标能力的必需和条件组件全部确定满足，才是 COMPLETE。选择纯文本与选择视觉是不同安装 profile，分别出计划。

## 硬件和安装准入

兼容分析输出 RUNNABLE、LIMITED、NOT_RECOMMENDED、INCOMPATIBLE、UNKNOWN，并附原因、估算依据、误差范围和推荐配置。不能从模型大小或设备营销名称直接推出适配。

估算按 Adapter 提供的架构规则分解：全部权重驻留分布、CPU/GPU Offload、组件权重、KV cache、激活及工作缓冲、Batch、Context、图像分辨率、音频长度、视频帧数和输出编码开销。MoE 区分驻留总参数与激活计算量；非标准 KV 架构没有专用估算规则时为 UNKNOWN。

磁盘预算包含尚未下载的共享 blob、staging、复制或链接策略、校验临时开销和受支持回退版本；不能把模型总大小直接当安装增量。执行时重新检查空间与硬件，因为搜索时快照不能保证执行时仍可用。

READY_TO_INSTALL 的条件是来源已验证、版本固定、文件已发现、分类足够、完整性 COMPLETE、Runtime 支持、硬件满足所选 profile、磁盘满足、计划未过期且摘要一致。NOT_RECOMMENDED 默认不生成自动执行计划；用户调整到满足规则的 profile 后重算。UNKNOWN、INCOMPATIBLE 和缺少必需依赖均阻塞。

## 状态和合法流转

生命周期阶段使用原始目标中的 DISCOVERED、REPOSITORY_VERIFIED、FILES_DISCOVERED、MODEL_CLASSIFIED、DEPENDENCIES_RESOLVED、COMPATIBILITY_CHECKED、READY_TO_INSTALL、DOWNLOADING、VERIFYING、INSTALLING、INSTALLED、STARTING、VALIDATING。READY 只作为某能力或某目标 profile 的聚合结果，不是文件存在状态。

| 流转 | 前置条件 | 失败或恢复 |
| --- | --- | --- |
| DISCOVERED → REPOSITORY_VERIFIED → FILES_DISCOVERED | 真实仓库响应和可追溯版本、完整分页清单 | 来源失败保持当前证据，记录分类错误 |
| FILES_DISCOVERED → MODEL_CLASSIFIED → DEPENDENCIES_RESOLVED | GGUF 内容或足够元数据、依赖图求值 | 不确定即 BLOCKED，不向下一阶段伪造值 |
| DEPENDENCIES_RESOLVED → COMPATIBILITY_CHECKED → READY_TO_INSTALL | 完整组件、明确 Runtime 和资源结果 | 改目标或配置时创建新计划 |
| READY_TO_INSTALL → DOWNLOADING → VERIFYING | 原子接受幂等执行、文件白名单及锁 | PAUSED 保留部分文件；失败按文件恢复 |
| VERIFYING → INSTALLING → INSTALLED | 文件和依赖全部通过、Manifest、提交日志 | 依据提交检查点恢复或隔离，不暴露半成品 |
| INSTALLED → STARTING → VALIDATING | 目标能力对应 LaunchConfig、资源调度和有效安装 | 运行与能力失败单独记录，保留已安装文件 |
| VALIDATING → profile READY | 该 profile 的全部必需验收通过 | 部分能力通过不覆盖其他能力失败 |
| READY → STARTING 或 VALIDATING | 卸载后重启或证据失效重验 | 会话有明确版本绑定，不静默切换 |

BLOCKED、FAILED 是任务结果，并保存失败阶段；PAUSED、CANCELLING、CANCELLED 是任务控制状态；运行状态独立取 STOPPED、STARTING、RUNNING、STOPPING、FAILED；能力状态取 UNVERIFIED、VALIDATING、READY、FAILED、UNSUPPORTED、STALE。数据库以状态版本 CAS 更新，旧 generation 和租约过期 worker 不得覆盖新状态。

普通重试沿原不可变计划恢复；来源或规则变化必须重新生成计划。取消不删除已验证共享文件；暂停保留部分文件，只有显式丢弃才清理部分文件。进程重启先恢复任务，再开放安装和卸载写操作。

## 下载和事务安装

所有路径从现有 `Paths` 派生。建议新增模型命名空间 `model-lifecycle/`，包含独立 `models.db`、`blobs/sha256/`、`staging/`、`installations/` 和任务证据目录；现有模型路径作为迁移源保留，不硬编码 Windows 用户目录。

1. 在同一数据库事务中登记执行请求、不可变安装输入、幂等键及计划摘要，预留空间并取得任务租约。
2. 按 artifact 身份下载到同卷 staging。已有目标和共享 blob 都重新核对必要哈希、大小及验证证据，不能 exists 即跳过。
3. 逐文件确认远程身份与分段检查点一致；支持 Range、ETag/If-Range 等实际可用机制；不支持 Range 时明确从头重下该文件。
4. 全部必要文件完成后核对 SHA256、GGUF 结构和组件图，生成规范序列化 Manifest 及摘要。
5. 将已验证文件放入内容寻址存储，正式安装引用只读 blob；不允许两个安装通过可写硬链接互相修改。Windows 链接不可用时执行受空间预算覆盖的复制。
6. 提交日志写 PREPARED，完成目录原子重命名，日志写 FILES_COMMITTED；数据库事务登记安装和引用后写 REGISTERED，最后收尾。
7. 只有登记成功的 installation 出现在正式已安装列表；待下载项目和任务单独展示。INSTALLED 后运行 Validator，不改变文件提交事实。

Manifest 显式包含 installationId 和 versionId。版本目录按两者分别派生，更新生成新版本而不覆盖旧目录；首次登记可建立默认活动版本，之后的更新保留现有 activeVersionId。切换必须带 expectedActiveVersion，冲突拒绝；运行会话持有原 versionId 和 manifestDigest，不因默认项变化而替换。安装日志和引用以 versionId 关联，跨版本相同摘要的文件仍各自保留引用。

任务和逐文件账本不能早于安装输入独立提交。同一幂等键只重放同一输入，改变版本、受管根目录、传输方式或缓存选择必须拒绝。底层 Manifest 提交也须核对该身份；文件验证见证携带实际受管根目录，不能用另一目录中内容相同的文件完成登记。恢复从持久化输入重建执行请求，鉴权凭据在执行时重新读取。历史任务若缺少输入，必须显式补录并核对既有提交日志，不从旧别名或目录名称猜测；暂停、取消和有效租约仍分别遵循原控制语义。

文件系统和数据库没有共同原子事务。不得把“数据库 transaction + rename”描述成整体原子提交。

| 重启时检查点 | 恢复动作 |
| --- | --- |
| 下载或校验中 | 重新取得租约，确认版本和部分文件身份，恢复未完成文件 |
| PREPARED 且只有 staging | 重验 Manifest 后继续提交；不完整则保留任务失败原因 |
| FILES_COMMITTED 但未登记 | 核对正式目录 Manifest，幂等完成数据库登记 |
| 已登记但响应丢失 | 按幂等键返回同一 installation，不再下载或提交 |
| 数据库存在但正式文件缺失 | 标记安装损坏、能力 STALE，提供修复计划 |
| 目录未知或摘要不符 | 隔离并标明原因，不自动加入正式列表 |
| 卸载中断 | 恢复引用解除和删除日志，不删除仍被引用或正在使用的文件 |

下载并发分为全局任务、文件和文件内 Range 三层，统一受限，不能相乘造成无界并发。默认策略由性能实测确定；每文件重试预算独立，网络瞬时失败可退避，鉴权、格式、哈希和兼容失败不做无界重试。

## Runtime 和 Unified AI Service

`RuntimeDescriptor` 至少声明 adapterId、RuntimeBuild、任务类型、架构、GGUF 版本、Tensor 量化类型、分片、视觉、音频、工具、GPU、混合模式、最小引擎版本、组件角色和验证档案。支持状态区分 DECLARED、VERIFIED、UNSUPPORTED、UNKNOWN。

`LaunchConfig` 固定 installation/version、Manifest 摘要、目标能力、完整组件路径和分片、Runtime build、模板来源与摘要、Context、GPU 层、CPU 线程、KV key/value 类型与预算、Batch、Flash Attention、设备分配、运行模式、输出限制和取消策略。进程内运行不要求端口；helper 模式使用受控本机端口和进程句柄。禁止模板字段混入 projector 路径。

参数先由规则和资源分析生成，再合并经过验证的用户配置。不能支持的参数明确拒绝，不能静默丢弃。GPU 回退 CPU 必须重新确认资源、报告实际配置；mtmd 失败单独使视觉失败，文本目标是否继续由 profile 决定。

缓存键包含 installation Manifest、Runtime build、影响加载的配置摘要、组件和设备。修改 GPU 层或组件时旧加载实例不能被错误复用。资源调度覆盖聊天和长任务，提供排队、取消、停止、租约、内存释放与运行中卸载保护。

Unified AI Service 先包装现有 Provider 边界，按类型提供 `chat`、`embed`、`rerank`、`transcribe`、`synthesize`、`generateImage`、`generateVideo` 和受控 `audioTask`。聊天继续复用现有消息和工具类型；非聊天结果是类型化向量、分数、文字或媒体引用，不能塞进聊天文本伪装成功。

Agent 的规划模型仅从通过所需文本和工具能力准入的模型中选择。图像和音视频任务可经既有受管工具边界调用 Unified AI Service，由 MCP 或平台工具返回结果；安装 Runtime 不会自动注册任意工具。工具权限、取消和结果可信处理沿用现有 Agent/MCP 实现。

外部 Provider 连接保留原接口，服务端标明 managed_local 或 external。外部请求不取得本地 installation READY 标记；“仅本地”策略下不能静默路由到外部。

## ACP 和错误契约

新增方法前缀建议 `_lumina/unstable/model-center/`，schemaVersion 从 1 开始；协议从 `crates/lumina-sdk-types` 定义，经 `ui/sdk` 现有生成流程发布，桌面只使用 SDK 或本地业务类型，不恢复 `ui/desktop/src/api`。

| 方法后缀 | 核心输入 | 核心输出 |
| --- | --- | --- |
| capabilities/read | 客户端支持的 schemaVersion | 支持的方法、schema、Adapter 和任务类别 |
| search | query、目标任务、分页 cursor、filters | 真实条目、匹配类型、核验状态、逐轮结果、partialFailure、cursor |
| repositories/inspect | 真实 repoId、revision 请求 | Snapshot、文件、可访问性、解析进度 |
| variants/list | snapshotId、目标能力 | 可区分的 Variant、GGUF 状态、未知字段及原因 |
| plans/create | variantId、目标能力、可选组件、资源配置 | InstallPlan、依赖图、完整性、兼容结果、blockingReasons |
| plans/read | planId | 当前计划及有效性，不静默更换计划内容 |
| installations/install | planId、planDigest、idempotencyKey | jobId、已接受的计划版本或确定错误 |
| jobs/read 和 jobs/list | jobId 或 cursor、afterSequence | 阶段、文件状态、速度、ETA、序号、恢复能力 |
| jobs/pause 和 jobs/resume | jobId、expectedVersion、idempotencyKey | 新任务版本和控制状态 |
| jobs/cancel 和 jobs/retry | jobId、expectedVersion、idempotencyKey | 取消中或新执行 generation；保留关联关系 |
| installations/list 和 installations/read | cursor 或 installationId | 正式安装、组件、版本、文件状态、能力和运行状态 |
| installations/repair | installationId、idempotencyKey | 可审查修复计划；执行仍走 install 的摘要准入 |
| installations/uninstall-plan | installationId | 被引用、被占用文件与准确空间回收说明 |
| installations/uninstall | planId、planDigest、idempotencyKey | 可恢复的卸载 jobId |
| runtimes/list 和 hardware/read | 安装或目标上下文 | 实际构建支持矩阵和资源快照 |
| launches/plan 和 runtimes/start | installationId、profile、配置；执行含计划摘要 | LaunchConfig 或运行 jobId 和实际配置 |
| runtimes/stop | runtimeInstanceId、idempotencyKey | 停止或已停止状态 |
| validations/start 和 validations/read | installationId、profile、suiteVersion | 验收 jobId 或分能力证据 |
| evidence/read | jobId、cursor、字段过滤 | 已脱敏业务证据、错误和运行诊断，限制大小 |
| defaults/update | 明确 installation/profile、expectedVersion | 能力准入通过后更新；会话中的绑定另行处理 |

请求带 requestId；写操作有幂等键及预期版本；冲突返回现状态，客户端不能自行强制覆盖。任务事件先持久化再推送，客户端凭 sequence 去重和补拉；首版可轮询同一游标接口，不能要求组件挂载才能维持任务。

错误统一包含 code、stage、messageKey、messageArgs、retryable、recoverable、jobId、causeId 和不含凭据的诊断引用。错误族包括 SOURCE_NOT_FOUND、SOURCE_AUTH_REQUIRED、SOURCE_FORBIDDEN、SOURCE_TIMEOUT、SOURCE_RATE_LIMITED、SOURCE_PARTIAL_FAILURE、REVISION_UNRESOLVED、REVISION_CHANGED、NO_GGUF、GGUF_INVALID、FORMAT_FORBIDDEN、SHARD_INCOMPLETE、DEPENDENCY_MISSING、DEPENDENCY_INCOMPATIBLE、DEPENDENCY_UNVERIFIED、RUNTIME_UNSUPPORTED、HARDWARE_INSUFFICIENT、DISK_INSUFFICIENT、HASH_MISMATCH、DESTINATION_LOCKED、PLAN_STALE、STATE_CONFLICT、DOWNLOAD_FAILED、INSTALL_FAILED、START_FAILED、VALIDATION_FAILED、CANCELLED。无结果是业务结果，不伪装 HTTP 错误，也不吞掉搜索部分失败。

### 当前应用服务接入范围

本批接入实现位于 `lifecycle/application.rs`、`service.rs` 和 `api.rs`，请求类型位于 `lumina-sdk-types::custom_requests::model_lifecycle`。以下为当前增量的确切接口，不替代上表完整目标；验证和交付状态以实施进度及对应证据为准。

| 当前 ACP 方法，统一前缀 `_lumina/unstable/local-inference/` | 输入及结果 | CLI 对应入口 |
| --- | --- | --- |
| `installations/submit` | planId、planDigest、installationId、idempotencyKey；服务端原子生成 jobId 和 versionId，重放相同幂等请求返回原身份 | `local-models install-plan` |
| `installations/jobs/list` | afterId、limit；返回任务状态版本、阶段、generation、分页游标、应用服务状态、fileRetries 和各任务最近的 failures；查询空命名空间不创建安装数据库或启动下载 | `local-models jobs` |
| `installations/jobs/events` | jobId、afterJobSequence、afterFileSequence、limit；分别返回持久化任务状态和逐文件进度事件、各自的续读游标及是否还有下一页 | `local-models job-events <id>` |
| `installations/jobs/control` | jobId、expectedVersion、action、idempotencyKey；以 CAS 执行 pause/resume/cancel/retry，返回 job 与 replayed；v8 回执持久化 | `local-models job-control`，要求 `--idempotency-key` |

`local-models wait-job` 等待已有任务，`local-models installation-worker` 运行同一个应用服务。ACP 工厂内的 Agent 共用安装服务；ACP stdio 启动、工厂创建 Agent 及 CLI 长会话入口会扫描已有命名空间，未完成初次恢复扫描时新提交等待。HTTP/HTTPS 启动路径调用同一工厂的恢复入口，主动恢复不依赖首个客户端连接；实际 HTTP 无客户端启动恢复已通过。Windows 实测通过证书校验的 TLS 1.3 连接及 HTTP/HTTPS 两种监听的正常关闭和检查点续传。没有模型数据库时保持休眠，模型存储损坏不阻止普通 Agent 创建。正常调用应用关闭时保留网络检查点，将当前 worker 所有的执行任务重新排队；用户暂停或取消的意图优先保留。HTTP 与 HTTPS 收到停止信号后停止接收连接，最多等待 10 秒排空，然后等待应用服务关闭。应用安装服务和分页恢复入口在同一个受管根目录共享两个任务槽。任务先获取自身调度锁，再竞争槽位；两个锁从读取凭据前持有至任务结束，由操作系统在进程退出时释放。槽位满时任务留在持久化队列中，后续扫描继续调度。不同任务发布相同内容时按 SHA256 排序获取共享文件锁，并在等待期间检查取消、应用停止及续租。两个槽位是当前实现的上限；文件和 Range 层预算、并发性能基准仍待完成。逐文件持久化退避见下方 schema v9 契约。

SQLite schema v8 在同一事务中提交控制状态、任务事件和幂等回执。相同键必须绑定相同任务、动作、expectedVersion 和受管根目录；改变其中任何一项均拒绝。回执同时绑定历史任务事件，损坏或伪造结果不能仅凭重算摘要通过。`replayed=true` 返回首次接受时的历史结果，即使任务后来已恢复或完成，也不会重复执行旧动作；客户端需要展示最新状态时仍查询任务列表并比较任务版本。新键提交过期版本被拒绝；写入回执失败使状态和事件一起回滚。终态取消可以保存无副作用的回执，不增加任务事件。

SQLite schema v9 增加逐文件重试账本和历史事件，保持已有任务、文件及控制回执的序列化格式。`installation-file-retry-v1` 每个文件在一个显式重试周期内最多执行 3 次逻辑尝试；首次执行前持久化次数，同一次传输的暂停、正常关闭或租约恢复继续原尝试。超时、限流、来源暂时不可用在预算内分别按 2 秒、4 秒等待重试，并取传输层返回的有界 Retry-After 与本地退避的较大值；当前传输层 Retry-After 上限为 60 秒。恢复扫描和直接领取任务均检查等待时间，退避期间不持有 worker 租约。第三次失败保留为 FAILED/EXHAUSTED；鉴权、哈希、格式及其他非瞬时错误进入 FAILED/NEEDS_ACTION。

失败码、阶段、时间、文件状态、预算和排队结果在同一事务提交；不保存原始异常、令牌或下载 URL。显式 Retry 为失败文件开始新周期，保留累计已记录次数及上次失败，同键重放控制回执不重复重置；Pause/Resume 不重置周期。文件验证通过与 VERIFIED 预算状态同事务提交。旧数据库升级标记 `legacy_untracked`，从升级后开始统计，不补造历史尝试次数。`installations/jobs/list` 的独立 `fileRetries` 返回文件身份、策略版本、周期、次数、历史起点、下次重试时间、类型化失败和预算版本；控制响应仍返回首次接受时的历史任务快照。

schema v10 增加不可变安装失败记录，绑定状态变更后的历史任务事件，摘要或历史快照不一致时拒绝读取。绑定受管目录的任务在凭据获取或安装准入失败时，以预期任务版本进入 BLOCKED；失败记录与状态、generation 和事件同事务提交。暂停、取消及其他 worker 的有效租约优先，旧检查结果不能覆盖新状态。历史未绑定输入、其他目录和损坏输入继续保留各自恢复诊断，不写入推断的失败归属。取消确认不要求读取凭据。

生产 ModelScope 凭据桥接区分未配置与配置值或存储读取失败；错误返回固定诊断，不携带原始值。凭据失败不领取文件尝试，也不会在后续轮询或重启时自动重新领取，用户处理后需显式 Retry。拥有有效租约的非文件执行及提交失败进入 FAILED；文件失败同时记录已有重试预算和任务失败证据。当前状态与失败记录写入中任一步失败时事务整体回滚。

任务列表的 `failures` 返回任务身份及失败时的版本、code、stage、origin、artifactId、messageKey、受限 messageArgs、retryable、recoverable、causeId、automaticRetryAt、occurredAt，以及失败快照是否与返回任务一致的 isCurrent。origin 区分 credentials、admission、transfer、verification、commit。保留最近失败不表示它仍阻塞当前任务；重试或完成后相应 isCurrent 为 false。控制回执继续返回首次接受的历史任务快照，不附加会变化的失败详情。v9 升级不补造此前未记录的失败历史。

事件轮询按任务隔离。两个序号来自独立事件表，客户端分别保存 `nextJobSequence` 和 `nextFileSequence`，初次读取使用 0。每个流按序号升序返回，每页上限默认 64、允许 1 至 256。空页保留原游标。逐文件事件含阶段状态、已完成与预期字节、任务代次、文件版本、更新时间及可选的失败码和重下原因。事件查询不会启动 worker，空命名空间不创建安装数据库。原始路径、URL、令牌、检查点摘要和下载验证器不进入响应。桌面总体进度、速度和 ETA 展示，以及旧 spec 下载转发继续执行 ML-40、ML-42 范围。

SQLite schema v11 增加 installation-storage-v1 预留及不可变历史，与接受任务、输入绑定及新 Retry/Resume 的状态变化同事务提交。同一数据库按真实磁盘卷协调任务；当前预留完整计划峰值，暂停持有，阻塞/失败/取消需写入停止后释放，完成与版本登记原子释放。回执重放不重复预留；升级标记 needs_admission，恢复前复验卷。列表增加 storageReservations（jobId、planDigest、volumeId、peakBytes、reservedBytes、state、version、createdAt、updatedAt），不返回内部 storeRoot。精确已落盘字节、共享内容节省量、跨数据库协调和操作系统物理预分配不在该保守基础实现范围，三层全局并发和精确空间核算仍待交付。

schema v12 在安装提交的 staging 阶段记录剩余发布预留。按摘要锁定本次涉及的 blob 后，计算缺失的共享内容、独立版本文件和 Manifest 的逻辑字节数。已同步并写入受管路径的文件才能扣减预留。数据库更新失败时保留已写文件，恢复后重新计算并验证内容。已准备完成的任务可持有零字节预算，登记或不再活跃时照常释放。历史 v11 事件沿用原规则。首次准入与下载阶段仍预留完整峰值，运行引用和卸载清理继续按 ML-20、ML-21 实施。

schema v13 对同步后的下载前缀进行空间抵扣。文件检查点、抵扣账本与预留事件同事务写入，旧诊断检查点接口不授予抵扣。重下事务恢复预算后，持有传输写锁的 worker 才能截断文件。进入 staging 时转为发布预算并清除逐文件抵扣。暂停和正常关闭保留抵扣，释放预留时清除抵扣。首次准入和释放后重新准入继续按保守峰值计算。

schema v14 允许失败或阻塞的传输释放空间预留并保留逐文件抵扣。Retry 在控制事务内以传输写锁保护路径、身份和长度核对。只有同卷上仍具备完整已同步前缀长度的受管文件可抵扣，已知哈希失败文件和缺失、截短文件不抵扣。恢复 worker 按原检查点校验内容哈希。取消及完成清理抵扣，首次准入保持保守峰值。

schema v15 允许冻结的 GGUF 计划在首次准入时按已登记且物理存在的共享 blob 抵扣一次写入预算。同摘要多文件只预留一份 blob，独立下载与发布文件分别计费。预留和临时引用随任务原子提交。发布核验文件内容并重算缺失字节。释放预留清除临时引用。计划生成仍使用保守峰值，后续卸载和垃圾回收必须同时检查正式版本引用及安装临时引用。

## 界面和状态契约

面向 Windows Electron 桌面，复用 React、现有主题 token、Button、Dialog、Tabs 和国际化，不另建视觉系统。属于跨模块复杂业务界面；原生文件路径和进程信息经后端适配，渲染端不直接访问数据库、文件或启动命令。

组件树为 ModelCenter → DiscoveryView、ModelDetailView、TaskQueueView、InstalledView；共享 RepositoryCard、VariantPicker、DependencyTree、CompatibilitySummary、InstallPlanPanel、FileProgressList、CapabilityStatus、RuntimePanel、VersionList 和 EvidenceViewer。视图只组合组件和调用服务，规则不藏在卡片中。

| 页面 | 主要数据和动作 | 必备状态 |
| --- | --- | --- |
| 发现 | search、inspect、variants | 精确与相关、未核验、无 GGUF、空结果、鉴权、超时、部分失败 |
| 详情与安装计划 | plans/create、install | 量化、完整组件树、来源版本、总量及增量、硬件、阻塞原因、计划过期 |
| 任务队列 | jobs、pause/resume/cancel/retry | 排队、逐文件进度、网络重试、暂停、取消中、校验、提交、失败和恢复 |
| 已安装与版本 | installations、repair、uninstall | 完整、损坏、历史导入待验证、共享组件、运行中占用、版本回退 |
| 运行与验收 | launches、runtimes、validations | 实际 CPU/GPU、排队、启动失败、各能力通过/失败/未知、证据失效 |

搜索卡片字段：名称、真实仓库 ID、任务类型、架构、GGUF 状态、量化概况、文件数量和大小、组件完整性、Runtime 支持、硬件适配、安装可行性；尚未核验字段显示“待核验”，不显示 0、无依赖或已支持。核验完成后增量更新，不能为了填满卡片伪造结果。

语义 token 复用 `ui/desktop/src/theme/theme-tokens.ts` 与 `styles/main.css` 中 background、text、border、info、warning、danger、success、disabled。新增生命周期状态映射到已有语义色，图标和文字同时表达状态。

窗口最小尺寸沿用源码的 480×400；930px 为现有 md 断点。930px 以下详情进入单列独立视图或抽屉，以上列表详情双栏；宽屏内容设约 1600px 阅读上限。依赖树及日志独立滚动，小高度窗口保持主要动作可达。长仓库名允许换行、路径可复制，不能用裁切隐藏阻塞原因。

明确覆盖加载、空、错误、禁用、选择、焦点、长内容、权限、并发任务和断线恢复。支持键盘导航、可见焦点、Dialog 焦点恢复、树节点展开语义和非颜色状态提示。目标检查尺寸为 480×400、930×700、1366×768、1920×1080、2560×1440，并覆盖系统缩放及长中文文本。

## 迁移和协议兼容

旧模型先读后备份，产生 migrationId 和导入日志。LuminaManaged、HuggingFaceCache、ManualPath 保留各自所有权；不得删除共享缓存或用户原始模型。无法追溯版本时标记 imported_unverified，允许检查文件，不能自动成为受管完整安装或 READY。

旧 HF/MLX 已安装资产保留记录和显式兼容访问边界，不新增非合规下载；既有可运行功能的保留与新受管安装政策分开说明。待该路径及发行依赖评估完成后再移除下载实现，不能通过直接删除 crate feature 破坏其他平台已有功能。

旧 ACP 请求先转发到新服务；旧 download 请求也必须经过完整计划和格式策略，不能因兼容而绕过。新增能力协商区分旧客户端；无法表示完整业务时返回升级提示或阻塞原因，不压缩成虚假 Downloaded。SDK 与桌面同批发布，随后按实际调用台账移除历史名称。

旧默认模型和会话绑定保留；升级不主动替换用户选择。支持回退的数据库采用兼容读取或并行版本策略，先备份再迁移；不能简单声称旧二进制能读取新 schema。

## 验收证据和日志

Validator 使用真实模型和真实 Runtime，生成结果必须实际可读；不允许 Mock 推理、伪造工具返回或仅检查 HTTP 200。每种能力的通过条件见验收文档。

每份证据绑定文件摘要集合、引擎构建、启动配置、规则与验证套件版本。相关维度变化使对应证据 STALE；不相关的展示字段变化不触发全量重验。用户模型任务与基准验证任务分离，超时或资源失败不等价于模型结构不兼容。

业务任务事件存入证据存储，包含阶段、文件、状态和测量；不为每次轮询新增普通日志。错误与安全事件使用既有日志体系，明确敏感字段脱敏、证据分页、保留和清理。媒体证据按文件引用存储，不把大二进制写入数据库或 ACP 普通文本。
