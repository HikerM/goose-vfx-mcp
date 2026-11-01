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

1. 原子登记执行请求、幂等键及计划摘要，预留空间并取得任务租约。
2. 按 artifact 身份下载到同卷 staging。已有目标和共享 blob 都重新核对必要哈希、大小及验证证据，不能 exists 即跳过。
3. 逐文件确认远程身份与分段检查点一致；支持 Range、ETag/If-Range 等实际可用机制；不支持 Range 时明确从头重下该文件。
4. 全部必要文件完成后核对 SHA256、GGUF 结构和组件图，生成规范序列化 Manifest 及摘要。
5. 将已验证文件放入内容寻址存储，正式安装引用只读 blob；不允许两个安装通过可写硬链接互相修改。Windows 链接不可用时执行受空间预算覆盖的复制。
6. 提交日志写 PREPARED，完成目录原子重命名，日志写 FILES_COMMITTED；数据库事务登记安装和引用后写 REGISTERED，最后收尾。
7. 只有登记成功的 installation 出现在正式已安装列表；待下载项目和任务单独展示。INSTALLED 后运行 Validator，不改变文件提交事实。

Manifest 显式包含 installationId 和 versionId。版本目录按两者分别派生，更新生成新版本而不覆盖旧目录；首次登记可建立默认活动版本，之后的更新保留现有 activeVersionId。切换必须带 expectedActiveVersion，冲突拒绝；运行会话持有原 versionId 和 manifestDigest，不因默认项变化而替换。安装日志和引用以 versionId 关联，跨版本相同摘要的文件仍各自保留引用。

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
