---
title: lumina 模型生命周期完整工作包
sidebar_position: 12
---

# lumina 模型生命周期完整工作包

本清单对应[完整升级计划](./model-lifecycle-master-plan.zh-CN.md)和[架构契约](./model-lifecycle-contracts.zh-CN.md)，覆盖全部模型类别及交付阶段。初始状态全部为 PLANNED；任务完成须提交代码、契约变更和[验收证据](./model-lifecycle-acceptance.zh-CN.md)，文档存在不算任务完成。

责任角色：B 为生命周期后端，R 为 Runtime，F 为前端，I 为集成与发行，Q 为验证。角色表示交付责任，不代表已经分配人员或启动并行工作。一个工作包只设一个主责角色，其余为协作。

状态使用 PLANNED、IN_PROGRESS、BLOCKED、IMPLEMENTED、VERIFIED。BLOCKED 必须记录具体来源、组件、接口或硬件条件、替代路径、解除动作和复核日期；IMPLEMENTED 不能作为产品支持证明。

## 文件落点索引

以下现有文件构成调用方和兼容边界。新增目录只表示规划落点。

| 代号 | 现有位置或规划位置 | 责任 |
| --- | --- | --- |
| CORE | `crates/lumina-local-inference/src/` | 现有 management、hf_models、registry、lib 和 backend |
| LIFE | `crates/lumina-local-inference/src/lifecycle/` | 新增领域、来源、解析、依赖、持久化、任务、安装和验证 |
| DL | `crates/lumina-download-manager/src/lib.rs` | 现有字节传输与分段校验，按职责拆分实现文件 |
| RT | `crates/lumina-local-inference/src/llamacpp/` 及规划的新 Adapter 模块 | 推理、启动、硬件、类型化任务 |
| ASR | `crates/lumina/src/dictation/whisper.rs`、`dictation/mod.rs`、`acp/server/dictation.rs` | 本地听写和来源迁移 |
| ACP | `crates/lumina-sdk-types/src/custom_requests.rs`、`crates/lumina/src/acp/server.rs`、`acp/server/local_inference.rs` | 协议定义、分发与旧请求转发；新增模型中心 handler |
| SDK | `ui/sdk/src/`、`ui/sdk/generate-schema.ts` 及现有生成流程 | SDK 能力协商和类型化封装；不手改生成结果作为源 |
| UI | `ui/desktop/src/acp/local-inference.ts`、`components/settings/localInference/` | 旧页面迁移；规划新增 modelCenter 组件和业务 hooks |
| THEME | `ui/desktop/src/theme/theme-tokens.ts`、`styles/main.css`、`main.ts` | 复用主题、断点、最小窗口和 Electron 边界 |
| AGENT | `crates/lumina/src/agents/agent.rs`、`extension_manager.rs`、`mcp_client.rs` | 工具调用、权限、结果回传和取消 |
| PROVIDER | `crates/lumina-provider-types/src/base.rs`、`crates/lumina/src/providers/local_inference.rs`、`crates/lumina-providers/src/` | 现有 Provider 兼容与新服务接入 |
| CLI | `crates/lumina-cli/src/cli.rs` 及既有 CLI 路由 | 受管模型命令调用同一生命周期服务 |
| TEST | `crates/lumina/tests/`、各独立 crate 的 `tests/`、`ui/desktop/src/` 相邻测试、`ui/sdk/tests/` | 优先独立 tests 目录；真实与确定性验证分层 |
| RELEASE | `Cargo.toml`、相关 crate manifests、`ui/desktop/package.json`、`scripts/`、`ui/desktop/scripts/`、`.github/workflows/`、`lumina-self-test.yaml` | 构建 feature、发行戳、依赖、验证配方与发布 |

Runtime 与 GGUF 专用逻辑默认不进入 AGENT；UI 不直接调用 DL。数据库位置经 Paths 注入，不能放在源码目录下。

## M0 全量基线和契约

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-01 | I | 无 | 固定基线 SHA、版本、工作区状态、平台、现有构建 feature 和接口调用清单；落实全部目标追踪表 | 源码版本、桌面版本和构建戳分别记录，原有行为可回溯 |
| ML-02 | B | ML-01 | 扫描模型搜索与下载的所有入口、来源 URL、HF/MLX、Whisper、缓存导入、CLI 和推荐项；形成策略接入台账 | 每个入口映射到统一策略或明确历史兼容边界，不只覆盖模型设置页 |
| ML-03 | R | ML-01 | 对 LLM/MoE/VLM、Embedding、Reranker、ASR、TTS、Audio、Image/Video、MTP/DFlash 逐项验证引擎、绑定、GGUF 依赖、平台和最小硬件；选择确切 Runtime 版本 | 每类都有实际试验记录、缺口和实现路线；未通过类别有具体阻塞，不能仅填写“后续支持” |
| ML-04 | Q | ML-02，随 ML-03 逐类推进 | 从 ModelScope 真实发现每类代表模型和组件，记录实际仓库、Revision、文件、大小、可用哈希及许可证事实；建立正反例样本库 | 不填写虚构仓库；每类有可执行代表组合，缺失则记录精确组件和搜索证据，解除前该类未完成 |
| ML-05 | B | ML-01 | 按契约定义领域类型、Manifest、规范摘要、模型/变体/安装身份、能力记录和支持档案版本 | Schema 有明确必需字段、UNKNOWN、扩展和版本规则，能表达全部类别 |
| ML-06 | B | ML-05 | LIFE/repository 建立独立 SQLite schema、事务、迁移、外键、索引、状态版本、任务租约和事件序号 | 可并发幂等写入，旧 worker 无法覆盖新 generation，数据库检查用例通过 |
| ML-07 | B | ML-05 | 实现状态机、错误码、计划准入、取消恢复、证据失效、字段脱敏和能力判定 | 每个状态有进入/退出条件；错误有阶段和可恢复性，未知不自动通过 |

## M1 来源解析和安装计划

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-08 | B | ML-02、ML-05 | 从 hf_models 抽出 ModelScope Provider；真实接口适配搜索、详情、访问鉴权、分页文件、允许元数据和下载流；作用域令牌 | 真实公开、受限、缺失和故障响应正确分类；无其他平台回退 |
| ML-09 | B | ML-06、ML-08 | Repository Verifier 固定 Revision 与文件清单摘要，记录证据；检测来源变化、分页不完整和计划过期 | 下载使用同一固定快照；无法确定版本时阻止可执行计划 |
| ML-10 | B | ML-08、ML-09 | 搜索五类策略、真实仓库精确/相关分类、去重分页、有限并发核验和部分失败；移除推荐项伪造文件回退 | 故障不变空结果；候选不冒充目标；卡片未知字段保留待核验 |
| ML-11 | B | ML-05、ML-09 | GGUF Parser 检查版本、KV、Tensor 描述、量化、tokenizer、模板、参数计算及分片元数据；远程有限元数据读取与本地重验 | 多种真实 GGUF 可解析；截断/偏移越界/伪格式拒绝；无界数组和巨大元数据受限 |
| ML-12 | B | ML-11 | 分类器分离任务、架构、模态、Dense/MoE、组件角色；保存依据和 UNKNOWN；不依赖名称认定能力 | 相同名称的不同内容、未知架构及混合类型输出正确，参数口径明确 |
| ML-13 | B | ML-07、ML-11、ML-12 | 同仓库依赖图与版本化规则；分片、mmproj、VAE、编码器、解码器、MTP/DFlash 的必需/可选/条件/不支持表达 | 量化同名不错误合组；缺失、循环、无法确定条件均不能通过 |
| ML-14 | B | ML-04、ML-08、ML-13 | ModelScope 跨仓库候选查找、内容线索、维度/架构/版本规则和支持档案；规则证据可追溯 | 真实兼容组合通过；名称相似但不兼容组合拒绝；未验证候选禁止自动安装 |
| ML-15 | R | ML-03、ML-11、ML-13 | 硬件探测、Adapter 资源估算、Context/Batch/KV/多模态预算、磁盘增量与兼容分级；输出安装 profile | 不以文件大小作为唯一内存依据；全分片计入；未知架构不强行估算；资源不足阻塞 |
| ML-16 | B | ML-02、ML-05 | 统一格式来源准入；已有目标文件、缓存和共享 blob 复验；移除 exists 跳过；不合规新下载入口拒绝 | 错误同大小文件能检测；所有受管下载遵循同一策略；历史资产不被误删 |

## M2 下载安装迁移和恢复

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-17 | B | ML-06、ML-07、ML-09、ML-16 | jobs 编排器接入 DL，逐文件持久化，创建阶段原子去重，固定来源，任务租约和恢复游标 | 重复安装返回同一有效任务；重启可发现并恢复；任务不依赖 UI 挂载 |
| ML-18 | B | ML-17 | 暂停/恢复/取消/丢弃分离，按文件重试，三层有界并发；Range 不支持和远程文件变化处理 | 已完成文件不重下；暂停保留检查点；令牌更新可恢复；退避和速率受控 |
| ML-19 | B | ML-13、ML-15、ML-17 | 不可变 InstallPlan、安装准入、同卷 staging、完整验证、Manifest、内容存储、目录提交及登记 | 全部必要依赖校验后才进入正式列表；计划过期和空间变化明确拒绝 |
| ML-20 | B | ML-06、ML-19 | 组件引用、磁盘核算、卸载计划、运行占用保护、垃圾回收和版本切换 | 卸载不损坏其他安装或外部缓存；回收空间可解释；活动会话不静默换版本 |
| ML-21 | B | ML-18、ML-19、ML-20 | PREPARED/FILES_COMMITTED/REGISTERED 检查点及卸载恢复、孤儿隔离、状态 CAS 和任务 fencing | 每个提交边界进程终止后可恢复；重复恢复无额外副作用 |
| ML-22 | I | ML-06、ML-19、ML-21 | registry.json 备份与导入；保留路径所有权、旧别名、设置和默认项；版本不明资产重新验证；旧数据库回退策略 | 旧用户模型与设置不丢失；重复迁移幂等；损坏记录可诊断，不能默认空注册表掩盖错误 |

## M3 Runtime 核心和实际能力

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-23 | R | ML-03、ML-05 | 扩展 backend trait 和 RuntimeDescriptor；引擎/绑定/build/features 与支持矩阵关联 | 同一 Adapter 不同打包构建可准确区分 CPU/GPU、架构和任务支持 |
| ML-24 | R | ML-07、ML-15、ML-19、ML-23 | LaunchConfig、资源排队、缓存键、取消卸载、实际配置回传；进程内和 helper 生命周期 | 改组件、GPU 层或 Runtime 不复用错误缓存；崩溃可隔离和恢复；端口仅用于 helper |
| ML-25 | R | ML-11、ML-13、ML-23、ML-24 | 现有 llama.cpp 接入新配置；完整分片、模板、mtmd、CPU/GPU 混合和降级；消除推荐表覆盖真实视觉判断 | LLM/MoE/VLM 按真实元数据和组件加载；视觉失败不冒充成功；原有生成路径回归 |
| ML-26 | Q | ML-04、ML-07、ML-25 | Validator 框架及文本、多轮、原生/模拟工具、视觉与视觉多轮套件；按能力登记证据 | 真推理真工具闭环；输入图片有可判别正确条件；结果绑定安装和 Runtime |

## M4 全部模型类型和加速组件

每个类型工作包内部均按“固定真实样本 → 完整 GGUF 组件 → 类型化请求响应 → Adapter → 资源模型 → 真验收 → UI 和 Agent 映射”推进。ML-03 的验证失败会阻塞该项，不删除工作项。若需引擎补充开发，必须在本项下列出明确接口和补丁、构建与回归范围，再估算投入。

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-27 | R | ML-04、ML-19、ML-23、ML-24、ML-26 | Embedding Adapter；pooling/归一化/维度与输入长度；批量向量；语义检索验证 | 至少一个真实 ModelScope GGUF 完整安装、生成有限有效向量并通过固定语义测试 |
| ML-28 | R | ML-04、ML-19、ML-23、ML-24、ML-26 | Reranker Adapter；query/document 格式、分数含义和排序；不依赖聊天解析假分数 | 实际 rank 接口、有限分数、预定义相关性排序和输入边界通过 |
| ML-29 | R | ML-02、ML-04、ML-19、ML-22、ML-23 | 现有 Candle Whisper GGUF 接入；模型来源迁移 ModelScope；音频预处理及既有听写设置映射 | 真实音频转写、语言/长度/噪声边界、取消恢复和旧听写回归通过 |
| ML-30 | R | ML-03、ML-04、ML-13、ML-19、ML-23、ML-24、ML-26 | 选定 TTS 引擎和锁定 build；文本前端、声学/vocoder/codec 全 GGUF 依赖；合成流或文件输出 | 真实文本生成可解码且有语音内容的音频；不得只创建静音 WAV 或假文件 |
| ML-31 | R | ML-03、ML-04、ML-13、ML-14、ML-19、ML-23、ML-24、ML-26 | Image Diffusion Adapter；主模型、VAE、文本编码器、调度配置；种子/尺寸/步数和资源约束 | 全部必要权重 GGUF 且来自 ModelScope；真实图片可解码、规格正确并完成内容验收 |
| ML-32 | R | ML-03、ML-04、ML-13、ML-19、ML-23、ML-24、ML-26 | Video Diffusion Adapter；文本/图像输入、时序组件、帧数/FPS、编码和预览；独立于视频理解 | 真实生成帧和视频可解码播放；帧数、时长、输入关联正确；无重复静帧伪成功 |
| ML-33 | R | ML-03、ML-04、ML-13、ML-19、ML-23、ML-24、ML-26 | 通用 Audio 任务 Adapter；音频 encoder/decoder/codec 完整图；按理解与生成区分能力 | 实际输入输出、采样率/声道/长度与内容任务通过，不以 ASR 或文本生成代替所有音频任务 |
| ML-34 | R | ML-03、ML-04、ML-13、ML-24、ML-25、ML-26 | MTP 适配；核验内置/外置组件及目标模型配对；正确性基线、统计质量、延迟与内存测量 | 对真实配对模型完成开关对照；不支持时拒绝；无收益不虚标加速，普通 draft 不算 MTP |
| ML-35 | R | ML-03、ML-04、ML-13、ML-14、ML-24、ML-25、ML-26 | DFlash 专用适配；算法版本、训练关系、配置、tokenizer 和组件图；故障回退 | 真实 DFlash 对照验收，不用任意小模型替代；质量、性能、取消和工具行为满足 profile |
| ML-36 | B | ML-05、ML-12、ML-13、ML-23、ML-26 | 专用 GGUF 扩展规范与 Adapter 注册；未知元数据、角色和任务版本协商 | 一个受控扩展的真实集成例通过；未知类型可展示但不能冒充可运行 |

## 服务协议和模型中心

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-37 | I | ML-07、ML-19、ML-23、ML-25 | Unified AI Service 包装现有聊天 Provider；能力准入、模型/版本绑定、外部 Provider 分流和默认模型行为 | Agent 不需要 llama.cpp 私有参数；云端和本地旧入口兼容；本地策略不隐式外发 |
| ML-38 | I | ML-23、ML-24、ML-37；逐类依赖 ML-27 至 ML-33 | embed/rerank/transcribe/synthesize/image/video/audio 类型化服务、进度和取消；媒体文件引用 | 每类真实 Adapter 可经统一业务入口使用；超时、资源队列和取消状态一致 |
| ML-39 | I | ML-26、ML-37、ML-38 | Agent 与既有 MCP 的任务映射；规划模型准入、工具权限、结果回传、失败重规划和取消 | 理解→规划→工具→执行→结果→再分析真实闭环；感知与生成模型不被当作工具规划模型 |
| ML-40 | I | ML-05、ML-07；具体方法依赖相应服务 | 新 ACP 方法、schema 和 SDK；旧 local-inference/huggingface 方法适配、能力协商、游标事件、CLI 同服务 | SDK 生成和类型检查通过；旧方法不能绕过新策略；客户端重连不丢任务 |
| ML-41 | F | ML-10、ML-13、ML-14、ML-15、ML-40 | 发现、分类、详情、量化、依赖树、来源版本、兼容分析和安装计划组件 | 所有搜索卡片字段有真实服务映射；未知/不可访问/无 GGUF/部分失败独立展示 |
| ML-42 | F | ML-17、ML-18、ML-19、ML-40 | 后端持久化任务队列、逐文件和总体进度、速度 ETA、暂停/恢复/取消/重试与检查点 | 离开页面和重启后恢复；传输完成仍显示校验/安装阶段，不能提前自动选模 |
| ML-43 | F | ML-20、ML-22、ML-24、ML-26、ML-38、ML-40 | 已安装、版本、组件占用、运行配置、验证状态、证据和日志、卸载/修复流程 | 每种任务能力有明确展示与使用入口；失败原因和实际执行配置可读 |
| ML-44 | F | ML-41、ML-42、ML-43 | 现有 theme token、中文 i18n、480×400 至宽屏布局、键盘和焦点、长依赖树与低高度体验 | 五组窗口尺寸和系统缩放验收；关键操作无遮挡；无假数据和只可截图的页面 |

## 系统验证和发行

| ID | 主责 | 依赖 | 实施与交付物 | 完成条件 |
| --- | --- | --- | --- | --- |
| ML-45 | B | ML-06、ML-07、ML-17、ML-26、ML-40 | 持久化业务事件、错误阶段、证据引用、脱敏、保留和清理；复用既有错误日志 | 诊断能定位到文件、组件和 Runtime；无令牌泄漏；不增加轮询噪声日志 |
| ML-46 | Q | ML-21、ML-22、ML-26；逐类依赖 ML-27 至 ML-44 | 按验收文档执行逻辑、真实网络、真实推理、故障注入、UI、旧功能、资源压力；更新 self-test | 每条硬需求有通过记录；缺设备/跳过/ignore 是未验证，不能算通过 |
| ML-47 | I | ML-23、ML-25；逐类依赖 ML-27 至 ML-36 | Windows Rust 正确入口、CPU/CUDA/Vulkan feature、绑定/helper、DLL/资源、发行戳、SDK/后端兼容检查；既有平台边界 | 安装包实际能力与 UI 声明一致；干净机器可安装、运行、升级和卸载 |
| ML-48 | I | ML-39、ML-44、ML-45、ML-46、ML-47 | 全量需求证据复核、发行清单、回退演练、支持矩阵、来源规则维护及版本变更流程 | 全类别达到总计划完成定义；未满足类别保持未完成；不创建虚假的“全部支持”发行说明 |

## 开发批次和依赖执行

第一条关键路径是 ML-05 → ML-06/ML-07 → ML-08/ML-09 → ML-11/ML-13/ML-15 → ML-17/ML-19/ML-21 → ML-24/ML-25/ML-26 → ML-37/ML-40 → UI 集成 → ML-46/ML-47/ML-48。

第二条关键路径是 ML-03/ML-04 的全类别可行性 → 对应 ML-27 至 ML-36 → ML-38/ML-39/ML-43 → 全量验收。两条都完成才能关闭全部目标，不能用第一条完成替代第二条。

允许在契约稳定后并行：来源与解析、SQLite 与任务、Runtime 技术验证、前端视图和 SDK；依赖未满足的视图可以使用明示契约的开发夹具进行布局开发，但任何展示或验收都不得把夹具当成真实模型结果。产品界面不内置演示模型和伪进度。

每次实现变更附本工作包 ID、实际改动文件、调用方、接口变化、迁移影响、验证命令与结果、剩余阻塞。大型 Runtime 引入单独变更，避免同一次提交混入整站重构或无关依赖升级。

## 后续实施的检查命令

本次只交付计划，不执行以下构建和测试。实现阶段按用户的构建测试授权和仓库 AGENTS 执行；发生实际合并前必须满足其中的 Windows Clippy 要求。Rust 代码改动必须完成规定格式化。

```powershell
& .\bin\cargo.cmd fmt
& .\bin\cargo.cmd test -p lumina-local-inference
& .\bin\cargo.cmd test -p lumina-download-manager
& .\bin\cargo.cmd test -p lumina-agent --test local_inference_integration -- --ignored
& .\bin\cargo.cmd clippy --all-targets -- -D warnings
```

当前目录 `crates/lumina` 的 Cargo package 名为 `lumina-agent`，`crates/lumina-cli` 的 package 名为 `lumina-agent-cli`，库名和二进制名仍为 lumina。完整 CLI 构建使用 `& .\bin\cargo.cmd build -p lumina-agent-cli --bin lumina`。执行时按真实 Cargo 清单选择包名，不能以目录名代替包名。

真实模型测试运行前必须配置对应的已验证模型和硬件；已有集成测试的 model 环境变量按实际测试要求设置。新独立 crate 测试创建后进入 tests 目录，并在命令中选定实际用例。现有 ignore 用例不被执行时报告未验证。

UI 和 SDK 使用各自 package.json 现有脚本执行生成、typecheck 和定向测试；新增命令在实现时登记，不能用不存在的脚本作为证据。新增功能同步更新 `lumina-self-test.yaml`，授权构建后重建并执行 `lumina run --recipe lumina-self-test.yaml`。

Windows 人工依赖变更使用 `D:\DevTools\Rust\cargo\bin\cargo.exe add` 的仓库规定入口，保持锁文件一致。不得为计划阶段提前加入 Runtime 依赖、替换工具链或变更发行版本。
