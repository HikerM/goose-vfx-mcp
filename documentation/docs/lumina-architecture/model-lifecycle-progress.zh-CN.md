---
title: lumina 模型生命周期实施进度
sidebar_position: 14
---

# lumina 模型生命周期实施进度

执行目标为完成[七阶段计划](./model-lifecycle-master-plan.zh-CN.md)，每阶段实现和验证后提交并推送到 `testing`。唯一源码检出为 `D:\Project\goose`；不创建其他项目副本或 worktree，构建、模型、缓存、测试数据和临时输出均使用 D 或 E 盘。

四份完整规划文档已作为规划基线提交 `4f6265905` 并推送到 `origin/testing`。该提交只交付完整计划，不表示 M0 或后续实施阶段完成。

M0 技术基线提交为 `7aa6cfbe9fad627faf705a389b24369a3c2a5daa`，已推送到 `origin/testing` 并核对远程 SHA 完全一致；提交后工作区干净。当前继续实施 M1，不以 M0 提交作为整体任务结束。

## 阶段状态

| 阶段 | 当前状态 | 已取得证据 | 尚需完成 |
| --- | --- | --- | --- |
| M0 全量契约和技术验证 | VERIFIED，完整工作区 lint 失败保留 | ML-01～07、全类别真实试验、确切引擎/来源/组件/许可证事实、SQLite v4、状态契约、最新 CPU/CUDA 和完整 recipe；见阶段退出复核 | 产品接入属于 M1～M6；视频质量、旧 Whisper 和完整工作区 lint 仍未通过 |
| M1 来源解析和完整依赖 | IN_PROGRESS | 有界 GGUF、固定版本清单、真实 HTTPS 分支解析、远程原名与安装路径分离、Wan 63 文件完整复核和真实两页 85 个搜索结果 | 搜索发现服务实际调用、远程元数据、依赖、资源预算和全部入口准入 |
| M2 可靠安装和数据迁移 | IN_PROGRESS，产品入口未接入 | 逐文件、检查点、只读内容存储与独立目录提交、版本和引用；三处真实子进程强制终止后恢复通过 | 实际传输、启动恢复调度、产品服务接入、引用使用保护、卸载、迁移 |
| M3 Runtime 和核心真实验收 | PLANNED | 既有 Dense/MoE CPU 文本 smoke 与小模型一次严格工具闭环可运行 | 新 LaunchConfig 和 Runtime 接入、完整文本/工具/视觉验收 |
| M4 全部类型和加速 | PLANNED，M0 探针已交接 | 全类别实际 Runtime 技术记录；Embedding、Reranker、音频、图像、MTP/DFlash 探针通过；视频内容与旧 Whisper 失败保留 | 产品 Adapter、受管安装、类型化服务、质量与边界验收、视频和 Whisper 修复 |
| M5 完整模型中心 | PLANNED | 完整界面与服务契约已编写 | 实际页面、SDK、任务及能力状态接入和尺寸验收 |
| M6 系统验收和发行 | PLANNED | 原计划需求与验收映射 | 全类别、硬件、旧功能、安装包和回退验收 |

M0 技术基线已完成阶段退出复核，阶段提交和远程 SHA 以实际 Git 记录为准；这不表示产品支持或后续阶段完成。某个局部测试通过不代表对应阶段或全部能力通过。M1 至 M6 的原始范围保持完整。

## 已实现的基础代码

新模块为 `crates/lumina-local-inference/src/lifecycle/`，由现有 crate 导出：

- `domain.rs` 定义 ModelScope 仓库和安全相对路径、SHA256、不可变来源快照、模型及变体、组件图、安装计划、Manifest、Runtime build 和按 Adapter 区分的启动参数。
- `state.rs` 定义生命周期阶段、任务控制、各能力状态、验证身份和证据失效；文本通过不能覆盖视觉失败。
- `failure.rs` 定义可分类错误和有限自动重试策略。
- `repository.rs` 提供独立 SQLite schema、不可变快照和计划、原子幂等入队、状态 CAS、worker generation/租约隔离、事件游标和能力记录。
- `catalog.rs` 与 `repository/catalog.rs` 保存模型、变体、文件、组件及关联的不可变目录；schema v2 支持从 v1 快照摘要升级，损坏数据使迁移事务回滚。
- `files.rs` 与 `repository/files.rs`、schema v3 提供原子逐文件登记、检查点摘要、明确重下原因、文件事件和租约隔离。已有任务按原始接受时间恢复；新请求仍检查计划过期。完整文件由实际大小、SHA256、GGUF 或元数据内容检查产生验证凭据，不用下载字节数代替验证。
- `gguf.rs` 已实现版本及字节序、有界 KV/数组、张量块布局、尺寸溢出、偏移/重叠、分片总数和共享元数据检查，10 项局部检查通过。通用容器允许有界长名称和五维等张量，独立 llama.cpp profile 保持其加载限制；大型数值元数据以分块方式散列，只保留有限样本。
- `modelscope.rs` 已实现固定完整 Revision 的文件清单 Provider、作用域鉴权、有界响应、错误分类及固定版本下载地址。递归列表达到已知 3000 项上限时按目录重取；无法枚举的平铺目录、请求或字节预算耗尽均返回不完整错误。
- `modelscope/revisions.rs` 通过有界 Git smart HTTP advertisement 解析完整分支/标签 SHA，拒绝同名歧义、截断和冲突响应，处理 annotated tag 的 peeled commit；不依赖外部 git 可执行文件。`inspect_repository` 将解析证明与相同固定版本文件清单绑定，真实 master 查询已通过。完整搜索和下载流尚未接入旧调用方。
- `installation.rs`、`repository/installation.rs` 与 schema v4 建立 installation/version 分离、不可变 Manifest、STAGING/PREPARED/FILES_COMMITTED/REGISTERED 提交日志、版本登记和显式活动版本 CAS。Prepared/FilesCommitted 要求实际目录和全部文件重验生成的不可伪造凭据；Windows 验证句柄在凭据持有期间阻止文件写入/删除。注册事务同时登记文件摘要引用、活动版本和任务完成，更新保留旧版本且不自动切换。
- `LaunchConfig` 显式绑定 versionId、Manifest 与 Runtime build；`validate_binding` 拒绝跨版本、未计划能力、缺失/额外/重复组件文件。该检查只覆盖安装绑定，Adapter 参数及资源检查仍由后续 Runtime 工作包完成。
- `installer.rs` 的 `InstallationStore` 接上逐文件账本、实际只读 blob、独立安装副本、同根 staging 发布和数据库登记。逐块复制/哈希检查任务租约，接近过期时续期；版本/摘要分别加文件锁；已有文件重验，损坏或未知内容保留并拒绝。支持 PREPARED、rename 后未登记及登记后响应丢失的恢复，不通过可写硬链接共享安装文件。
- `bin/test-lumina-real-text.ps1` 为 Windows self-test 提供短调用入口，仍执行同一个真实 Rust 推理用例，不复制推理判定。它隔离子注册表、明确执行 ignored 用例、按实际测试数量拒绝跳过、使用有界 UTF-8 读取、保留原生日志，并以稳定 JSON 和非零失败码返回。PowerShell 5.1/7 及 LF/CRLF 实际矩阵已经通过。

本模块当前尚未替换旧下载和注册业务编排；仅 GGUF 结构校验接入旧下载完成后的入口。InstallationStore 已能从已验证逐文件账本完成真实文件提交及恢复，但网络传输 worker、启动时自动发现/调度恢复、运行占用保护及用户 registry 迁移仍需接入。子进程故障检查直接运行该模块，尚不是完整产品 ACP/CLI 安装入口的 A29 验收；样本 GGUF 不构成模型推理证据。

依赖通过仓库要求的 D 盘 cargo add 入口加入：工作区已有的 sha2 和 rusqlite 0.32.1 bundled；Cargo.lock 同步更新。没有创建新项目、复制源码或变更产品版本。

## 当前验证结果

| 检查 | 结果 | 范围 |
| --- | --- | --- |
| Windows Rust 入口 | PASS | cargo/rustup/rustc 位于 D:\DevTools；工具链 1.94.1 |
| 工作区数量 | PASS | 两条失效 worktree 登记已清理，实际登记仅 D:\Project\goose |
| 本地推理单元测试 | PASS | 104 项通过，包含截断 GGUF、来源目录遍历与作用域鉴权检查；不代表真实多模态验收 |
| 新生命周期契约集成测试 | PASS | 47 项通过，包括文件安装、真实子进程终止恢复、Windows junction 拒绝、版本及 v1/v2/v3→v4 迁移与回滚；新增完整物理分片组合和合法单文件错误组合拒绝检查 |
| GGUF、分类和来源局部检查 | PASS | 分类 10 项、GGUF 10 项、ModelScope 5 项、Revision 5 项；最新整包回归 188 项通过，默认跳过 6 项真实环境测试，不把跳过计为通过 |
| 真实 GGUF 结构检查 | PASS | qwen2 291 个张量、qwen35moe 733 个、clip 334 个、qwen3 310 个，检查真实尺寸及偏移；不是推理能力通过 |
| 真实固定版本来源 | PASS | 新 Provider 读取 Qwen 公开仓库固定 40 位提交返回 13 个文件，大小、来源哈希和清单摘要一致；真实受限仓库访问尚未验证 |
| 真实分支解析与固定清单 | PASS | 新 Provider 直接通过 HTTPS 将 Qwen3-1.7B master 解析到 cdd105fbd3a69efc373ba5d0451f71335630920a，再读取该 SHA 的 6 个文件；不调用 git.exe |
| 真实文本 smoke | PASS | Qwen2.5-0.5B 和 Qwen3-1.7B GGUF 分别通过 CPU 实际输出检查；不代表安装生命周期或工具验收 |
| 35B MoE 实际文本 smoke | PASS | 已有 Qwen3.6-35B-A3B Q4_K_M 实际输出 4，生成 token 为 1；12 CPU 线程、GPU 0、Context 8192；运行前后完整 SHA256 一致，不代表视觉或 MTP |
| SHA256 与来源核对 | PASS | 本机 428730208 字节 GGUF 与固定版本来源哈希一致 |
| 严格真实工具 smoke | PASS | Qwen3 实际执行指定 PowerShell 命令，请求/响应 ID 匹配，随机标记与实际结果 58 均匹配，最终回答也匹配；不代表全部工具场景通过 |
| Embedding 实际 Runtime 探针 | PASS | 固定来源 Qwen3-Embedding-0.6B Q8_0 在锁定 0.1.146 引擎上实际生成 9 个 1024 维有限归一化向量，3 个固定中英文检索用例均 top1 正确；不是完整产品安装或批处理 Adapter 验收 |
| 真实来源文档 | OBSERVED | 11 个固定版本候选仓库的 16 份许可证/模型说明实际大小和哈希已核对；不解释许可条件，不代表依赖许可完成审查 |
| Clippy | 来源文档增量 PASS | 该 crate 全 targets、无默认 features、-D warnings；依赖 vendor 仍有 3 条原有警告，不代表全工作区通过 |
| 完整 CLI 构建 | 分类与物理分片批次 PASS | 分类批次 CPU CLI 摘要 0fd77e7d3019c7fc258666705350df161eeada7ccc17eba5706ab2af5687ad2b；更新的来源文档批次另见下方构建记录。实际 package 为 lumina-agent-cli；核心遗留 605 条警告尚需处理，不能声称全工作区 -D warnings 已通过 |
| CUDA 构建及实际文本 | PASS | MSVC 环境修复后完整 cuda CLI 构建通过；Qwen3-1.7B 实际 29/29 层 offload、CUDA model buffer 1743.77 MiB、输出 4/正 token，明确检查无 CPU fallback；不代表全模型、视觉或发行验收 |
| PowerShell 真实推理入口 | PASS | 两种运行时静态检查均 0 问题；LF/CRLF 四组合、两种默认仓库路径调用及两次完整基础检查和报告，共 8 次真实输出均为 4；各 12 项解析/报告检查及 14 个失败反例通过。PSScriptAnalyzer 未安装，没有将其记为通过 |
| self-test recipe | 最新六组整轮 PASS；历史失败保留 | 35B CUDA 规划模型实际调用唯一入口，77 项基础检查和 1 次真实推理通过，报告读回/哈希及最终摘要一致。绝对路径与相对路径被原检查器误判，原失败保留且另存独立功能审计。最新 CPU CLI 已完成六组/84 项、1 次真实推理、唯一工具调用、最终事实摘要及冻结文件前后哈希核对；早期小模型工具错误仍保留 FAIL |

真实输入输出证据见 [文本 smoke 记录](./model-lifecycle-evidence/2026-11-01-real-text-smoke.json)。测试使用隔离的 `target/lifecycle-validation/app`，原始模型只按路径引用，没有复制权重或改写用户模型注册表。

[解析器及 Provider 真实记录](./model-lifecycle-evidence/2026-11-01-gguf-parser-real.json)保存真实文件 Header 摘要、张量数量、编码字节数及量化组成。35B 文件实际存储张量元素约 346.6 亿；不能以 8 个活跃专家视为只需驻留少量权重。该模型首次文本尝试在长时间没有生成输出后中断，保留为 UNVERIFIED，没有记录为支持或不支持。

[Qwen3 验证记录](./model-lifecycle-evidence/2026-11-01-qwen3-smoke.json)对应固定提交 cdd105fbd3a69efc373ba5d0451f71335630920a 的实际 Q8_0 资产：1834426016 字节和 SHA256 已核对，310 个张量结构检查及 CPU 文本 smoke 已通过。该文件放在 D 盘 target 验证资产目录，不代表通过产品安装流程。第一次算术场景因向 cmd.exe 提交裸 PowerShell 表达式而失败；[严格工具验证记录](./model-lifecycle-evidence/2026-11-01-real-tool-smoke.json)保存随后通过的真实请求、响应、模型哈希和 CLI 二进制哈希。

[35B MoE 文本记录](./model-lifecycle-evidence/2026-11-01-moe-text-smoke.json)对应固定提交 1f423dd35457209ba58aa7e154e5d7facd317431 的实际 21166758336 字节用户资产。独立注册表只读引用原路径，前后完整 SHA256 均为来源的 04c1756f604eaee089556f1604dbb3a16a4ec991b02a1512dfb389793b488b3f；实际生成 4 和正 token 计数。此前中断尝试保留为历史 UNVERIFIED，本次明确 CPU 参数的成功记录不扩展到视觉、工具或 MTP。

[硬件与工具观察](./model-lifecycle-evidence/2026-11-01-hardware-tools.json)记录实际约 96 GiB 内存、28 逻辑处理器、RTX 5070/12227 MiB、驱动 610.88 和 compute capability 12.0。设备存在和工具版本不作为兼容性或 GPU 验收通过；当前 CPU 二进制与正在构建的 CUDA 产物分别记录。

一次并行回归曾触发 Windows 对运行中测试程序的锁，出现 LNK1104。已等待真实测试结束后串行重跑，该批次 147 项通过；该构建失败没有被计入通过。

后续 [schema v4 安装日志证据](./model-lifecycle-evidence/2026-11-01-installation-journal.json)对应该批次 159 项通过、5 项默认跳过及该 crate 严格 Clippy。新增 7 项契约检查覆盖登记前隐藏、数据库重开恢复、接受后过期、旧 worker、物理文件与 Manifest、事务失败回滚、共享摘要引用、版本切换及 schema v3 升级。

[文件安装编排证据](./model-lifecycle-evidence/2026-11-01-filesystem-installer.json)保存后续 169 项整包回归、最终 10 项专项复核及严格 Clippy。真实子进程分别在 PREPARED、rename 后和 REGISTERED 后被父进程强制终止并等待退出，随后重新打开数据库恢复，逐次断言只有一个版本、一个引用、任务完成且无遗留提交；Windows broken junction 也保持原样并拒绝发布。完整产品故障注入和用户能力验收仍待接入。

[分类与组件复验记录](./model-lifecycle-evidence/2026-11-01-classification-components.json)保存最新 181 项通过、5 项默认跳过和该 crate 严格 Clippy。分类器分别保存观察架构、全部存储张量元素、专家总数/活跃数、任务候选、来源声明和组件提示，不产生 Runtime 能力 PASS。安装文件见证在持有只读文件句柄期间，按组件复验实际完整分片集合；即便逐文件大小和 SHA256 都合法，缺片、重复分片编号、跨片重复张量及多个未分片文件错误合组也会被拒绝。组件配对规则、来源声明生产和产品入口接入仍未完成。

[报告失败记录](./model-lifecycle-evidence/2026-11-01-selftest-report-failure.json)保留操作说明报告、真实工具响应和重复调用的失败。该轮在已知失败后明确终止，不是观察超时。最新短入口由程序直接执行六组基础目标和真实推理，使用 CreateNew 写不可覆盖的 UTF-8 报告、读回实际内容及 SHA256；报告失败或未执行结果不被表示为零失败。Windows PowerShell 5.1 环境缺少 Get-FileHash 的首次报告检查失败保留在验证目录；改用 .NET 流式 SHA256 后，完整 8 次真实执行、14 个失败反例矩阵通过。

[本轮实际 self-test 记录](./model-lifecycle-evidence/2026-11-01-selftest-with-tools.json)保存重建二进制摘要、真实工具失败 ID 和报告摘要。CLI 正常退出、模型最终声称成功及报告存在均不能覆盖实际失败，记录保持 FAIL。

[空响应 self-test 记录](./model-lifecycle-evidence/2026-11-01-selftest-empty-response.json)保存后一次运行的实际 65 项通过、3 项跳过以及没有推理和报告的事实，CLI 退出 0 没有被当成成功。随后将长嵌套命令改成短脚本调用；[PowerShell 入口质量记录](./model-lifecycle-evidence/2026-11-01-real-text-powershell-gate.json)绑定脚本哈希、两种运行时版本、静态结果、LF/CRLF 四次真实推理、解析反例和失败退出检查。

[入口缺陷及恢复记录](./model-lifecycle-evidence/2026-11-01-selftest-entry-recovery.json)保留第一次省略 RepositoryRoot 时的参数绑定错误，以及修复后实际输出 4 的工具响应。原四组合显式传参，未覆盖默认路径；已将路径解析移至执行阶段，并额外完成两种运行时的默认调用，最终入口质量记录共 6 次实际推理。带有原错误、且脚本在运行中修复的轮次不计为干净 PASS；终止该失败轮次是为冻结修复版本重新验证，并非观察超时。

CUDA 首次构建同时占用宿主与平台 Cargo 锁，使 CPU 推理用例编译等待。已保留第一次缓存并在确认进程结束后，将 CUDA 构建改用当前项目的 `target/cuda-validation/`；CPU 验证继续使用 `target/`。两者都是同一源码检出的构建产物，不是项目副本或 worktree，临时文件和 CUDA 缓存仍在 E 盘。

## 来源验证的实际发现

### 模型入口台账

| 入口 | 当前实际调用方 | 后续接入位置 |
| --- | --- | --- |
| 桌面搜索和仓库变体 | localInference 页面 → acp/local-inference.ts → ACP handler → management → hf_models | ML-08 至 ML-10 统一来源与核验 |
| 桌面下载 | ACP handler → management::download_model → resolve_local_model_selection / resolve_local_model_spec | ML-16、ML-17、ML-19 统一策略与安装计划 |
| CLI 下载 | cli.rs::handle_local_models_command 直接调用 resolve_local_model_spec 和 register_resolved_model | ML-40 改为同一生命周期应用服务，避免绕过管理层 |
| 推荐项及已有模型的视觉补全 | management::ensure_featured_models_current 直接调用下载器补 mmproj | ML-10、ML-13、ML-19 去除合成记录并纳入组件计划 |
| 历史 MLX 选择 | management 显式 backend_id 接受 mlx，hf_models::resolve_mlx_model 调用 HF Hub 下载 | ML-16 拒绝不合规新安装；ML-22 保留历史文件所有权 |
| 本地语音听写 | acp/server/dictation.rs → whisper 模型表 → 通用下载器 | ML-29 迁移真实来源并接入模型注册和任务 |
| 外部 HF 推理 Provider | providers/huggingface.rs、declarative_providers、provider_registry 使用远程推理路由 | 属于外部服务连接，不是本地权重下载；ML-37 保持显式边界 |
| 安全设置中的远程分类模型 | SecurityToggle 的远程推理 endpoint 配置 | 不冒充本地安装或 READY，不删除已有远程功能 |

通用下载器没有模型来源业务语义；策略应在受管模型入口强制执行，不能通过直接给通用传输函数增加单一站点限制破坏其他资源调用。

[ModelScope 探测记录](./model-lifecycle-evidence/2026-11-01-modelscope-discovery.json)保存 11 类真实候选的仓库、固定提交、文件数量、GGUF 示例、来源哈希及文件清单摘要。组件兼容性统一标为 unverified，Runtime 验证统一标为 not-run。

[来源文档事实](./model-lifecycle-evidence/2026-11-01-source-license-facts.json)记录同一批固定版本中 16 份实际文档的 SHA256、大小及模型卡原文声明字段。请求只访问 ModelScope，不跟随文档中的外部链接；跨组件和上游许可保持 UNVERIFIED，不能把卡片单个声明自动套用到全部依赖或商业发行。

[本机 GGUF 元数据观察](./model-lifecycle-evidence/2026-11-01-local-gguf-metadata.json)来自实际文件：小模型架构为 qwen2，已有 35B 模型的架构为 qwen35moe，记录 256 个专家和每次使用 8 个专家；projector 架构为 clip。该观察只覆盖 Header 和元数据，不替代 Tensor 边界检查、组件配对或视觉推理验收，也不能按每次激活 8 个专家估算全部驻留权重。

[锁定 Runtime 接口审计](./model-lifecycle-evidence/2026-11-01-runtime-interface-audit.json)记录实际依赖源码与哈希。当前引擎明确忽略 NextN/MTP 张量，既有 draft model 路径不能认定为 MTP；Embedding、rank pooling、音频输入存在底层接口，但缺少对应业务适配和真实模型验收。DFlash 在限定源码范围内没有找到实现，不据此虚标支持或宣称所有上游版本均不支持。

同一审计新增了 rank 绑定边界：当前 wrapper 按 n_embd 构造输出 slice，原生 rank 接口返回 n_cls_out。ML-27/ML-28 必须使用准确输出长度的原生上下文封装或修正后的固定绑定，不能先构造长度错误的 slice 再只取第一个数。

[上游候选记录](./model-lifecycle-evidence/2026-11-01-runtime-upstream-candidate.json)固定 2026-11-01 观察到的 llama.cpp 提交 `dde879bc4f954b809892fd0d660515f42536394a`，保存 DFlash、推测解码、C API 和 server 文档的实际来源及 SHA256。该上游已包含专用 DFlash 模型和 MTP/DFlash 推测解码路径；旧绑定中的限定搜索结果不能被解释成上游永久不支持。

[绑定候选审计](./model-lifecycle-evidence/2026-11-01-binding-candidate-audit.json)通过现有 D 盘 Cargo 缓存读取 `llama-cpp-2` / `llama-cpp-sys-2` 的确切 `0.1.160` 包，逐包将归档 SHA256 与官方 registry 元数据核对，并逐个比较抽取源码和归档内文件。候选已按实际 pooling 区分 `n_cls_out` / `n_embd_out`，且提供同模型 `MtpSpeculative` RAII 封装、MTP context 和张量加载参数。其 native 层包含 DFlash，而 Rust `src/*.rs` 范围未找到公开 DFlash wrapper；后续需要受控隔离 helper 的产品适配和生命周期接入。当前产品仍锁定 `0.1.146`，候选尚未构建和真实验收，不能据此开放 MTP/DFlash 或登记 READY。

[Embedding 来源记录](./model-lifecycle-evidence/2026-11-01-embedding-fixture-source.json)保存固定提交 `61ae123505fc6fa7f36d372fae1a30bc384241ea` 的真实 639150592 字节 Q8_0 资产及完整 SHA256。文件只存入 D 盘验证资产目录。[实际 Runtime 探针](./model-lifecycle-evidence/2026-11-01-embedding-runtime-probe.json)再次读取生产 Provider 的固定版本清单，核对运行前后完整模型 SHA256，使用锁定引擎的 CPU/4 线程、Context/Batch/MicroBatch 512、Last pooling 和 EOS 产生实际向量。6 个文档及 3 个查询全部为 1024 维、有限值且 L2 归一化，法国首都、Python 排序和中文光合作用的三个预定义检索均 top1 正确，编码与检索部分实际耗时约 11.51 秒；保存完整向量、分数、模型/二进制/锁文件/探针源码摘要。旧 wrapper 下显式约束输入输出宽度一致，拒绝 Rank 和不同 embedding_length_out。该记录仍不代表 ML-27 的产品安装、批处理 Adapter、GPU 或最终能力验收完成。

[ModelScope 协议记录](./model-lifecycle-evidence/2026-11-01-modelscope-protocol-audit.json)保存官方客户端源码摘要、递归文件列表上限和实际 Root 查询。凭据按请求设置 Bearer 与旧 API session cookie，不写入快照；真实受限账号验证仍未完成。

[远程 GGUF 内容观察](./model-lifecycle-evidence/2026-11-01-remote-gguf-metadata.json)由同一个 Rust 解析器检查固定版本的实际前缀和来源文件长度。第一轮发现图像长张量名、视频五维张量和音频大元数据数组与原先 llama.cpp 限制冲突；增加通用有界 profile 后，九类候选抽取的 60 份文件均完成容器检查。该记录只证明容器观察，不证明完整组件、Runtime 或生成效果。

[真实内容分类记录](./model-lifecycle-evidence/2026-11-01-classification-real.json)再次通过生产 Provider 读取九个固定仓库的文件清单，重新解析缓存的实际 GGUF 前缀，并对照上一轮 Header SHA256。60 份观察均完成分类；六份实际 BGE Reranker 的 scalar head 省略末尾单位维度，按锁定原生引擎的等价 shape 规则修正后可记录 Reranking 候选。候选、模型卡来源声明和实际 Runtime 验证始终分开；目前 Whisper、部分图像模型仍为未知任务，TTS/DFlash 包中的 qwen3 子模型不会仅凭仓库名称被直接认定为 TTS/DFlash。来源声明生产器、完整组件图和真实类别验收仍待继续实施。

[容器 profile 差异记录](./model-lifecycle-evidence/2026-11-01-gguf-container-profiles.json)保留上述实际约束和范围。音频包观察到内嵌 Python 源文件；解析容器不授权解包或执行这些源文件，Adapter 必须核实其数据依赖和执行边界。

[Audio Runtime 候选审计](./model-lifecycle-evidence/2026-11-01-audio-runtime-candidate.json)从实际 ModelScope 模型卡追溯到 `0xShug0/audio.cpp`，固定提交 `c0cd77fa80285f8113883e0de06ffc781cf502f6` 并保存文档与构建定义摘要。该引擎要求自身 GGUF family/package spec，不能把任意 llama.cpp/Whisper GGUF 强行加载；包资源与选项 contract 的发现顺序不同，存在当前目录/父目录的外部 spec 发现路径。候选接入需显式选择受控 spec、关闭独立模型管理器以避免绕过 ModelScope 安装计划，并审计原生资产 loader 后再执行实际 TTS/Audio 输出。当前只完成固定源码观察，没有构建、执行 GGUF 内嵌代码或取得音频能力 PASS。

协议记录还补充了真实 smart HTTP 试验：带 Git 兼容请求头直接读取 ModelScope refs 成功，返回完整 HEAD 与 master SHA。生产模块现已实现有界 packet-line 解析和引用消歧，并通过真实固定清单检查，无需把外部 git 可执行文件作为桌面安装依赖；旧搜索和下载调用方尚待服务接入。早期 421 不再作为该接口永久不可用的依据。

已经确认：分支 revisions API 对探测仓库返回 master 名称，文件 API 中的逐文件 Revision 是该文件相关提交，不可随意选作仓库 HEAD。Git ls-remote 在 ModelScope 仓库地址可取得完整 HEAD，文件接口接受该完整 SHA。

[生产 Provider 版本解析证据](./model-lifecycle-evidence/2026-11-01-modelscope-revision.json)保存新 Rust HTTPS 实现实际取得的 master SHA、advertisement 摘要、相同版本文件快照和 6 项清单数量；此检查没有调用 git.exe，也不代表受限仓库或模型运行通过。

直接使用通用 Python HTTP 客户端读取 Git advertisement 的尝试返回 HTTP 421，记录保留在 evidence 中；Git 客户端读取同一官方仓库引用成功。后续 Provider 必须按真实客户端和协议行为验证版本解析，不能预设一个不存在的 commits HTTP 接口或只取短 SHA。

完整构建还核实了源码目录名与实际 Cargo package 名不同：核心包为 lumina-agent，CLI 包为 lumina-agent-cli。初次以目录名选择 CLI 包的命令失败，已修正后重新启动构建，并同步修正工作包中的集成测试命令。失败的构建不计入通过。

## 下一执行项

继续补齐 ML-01 至 ML-07 的全部交付，不把完整 M0 缩减为本轮基础模块。真实类别核验需要内容解析，所以先实现 ML-11 及 ML-08/09 的必要基础来辅助 M0，并保持两个阶段均未完成。优先复核 GGUF 和 Provider 的新增代码、实际文件及固定版本来源，再完成最新完整构建和有效自测；同时补齐数据库的安装、文件及组件记录。阶段出口满足后再执行 stage commit 和 push，后续六阶段保持原定验收要求。


## 来源文档与原生辅助 Runtime 增量

`metadata.rs` 只读取固定清单内白名单文档，限制精确字节数、UTF-8、来源哈希及本地摘要；配置、模型索引和模型卡中的声明保持文档作用域，不能自动套到同仓全部 GGUF。7 项文档检查和一轮真实 OuteTTS 模型卡读取通过；最新整包 188 项通过、6 项默认跳过。Windows R5 矩阵针对六组/84 项基础检查，8 次真实推理、14 个失败反例、两种运行时各 12 项局部检查及静态检查均通过。最新 CLI 完整 recipe 仍单独待验。

[35B CUDA 混合推理](./model-lifecycle-evidence/2026-11-01-cuda-moe-smoke.json)实际 offload 10/41 层、CUDA model buffer 4953.39 MiB，输出 4 和正 token；模型运行前后完整 SHA256 一致。[五组 self-test 功能审计](./model-lifecycle-evidence/2026-11-01-selftest-moe-functional-audit.json)确认同一模型完成实际工具、报告与最终摘要；原检查器将绝对脚本路径误判的 FAIL 和命令字面不同事实同时保留。这不是新增六组配方或全部阶段的验收通过。

确切 0.1.160 Rust 绑定已移除当前原生工具链使用的 OpenAI 模板封装，暂保留产品的 0.1.146。通过[固定提交原生辅助引擎](./model-lifecycle-evidence/2026-11-01-native-helper-build.json)隔离 ABI，第三方源码和构建缓存仅位于当前项目 target，不新增 Goose 副本或 worktree；构建关闭自动 UI 下载、独立模型连接器、子进程工具及 GPU，并以确切源码提交和二进制摘要标识。

[辅助引擎 Embedding](./model-lifecycle-evidence/2026-11-01-native-helper-embedding.json)实际产生 9 个有限、归一化的 1024 维向量，三个预设语义检索 top1 正确。[BGE Reranker](./model-lifecycle-evidence/2026-11-01-native-helper-reranker.json)读取固定 ModelScope GGUF，三个预设中英文场景各自输出六个有限分数且 top1 正确。该模型在本引擎返回原始分类 logit，不是文档描述的概率；源码明确直接返回 classifier output，适配层必须显式保留分数语义。首次错误概率假设的失败记录保留。这些是 M0 可行性探针，尚未替代产品安装、Runtime 调度和全类型验收。


## MTP、视觉和语音真实可行性增量

[MTP 对照](./model-lifecycle-evidence/2026-11-01-native-mtp-feasibility.json)使用同一固定来源 Qwen3.5-0.8B Q8_0、同一辅助引擎和三组固定 greedy 输入。开关 MTP 后文本和 token 数均一致；原生计数实际生成 143 个 draft token、接受 105 个，不能用启动参数代替这些计数。首次检查器漏收不带 draft 字样的 accepted 指标，其失败独立保留；修正指标过滤后重跑通过。仅记录小样本耗时，不宣称统计加速收益。

[视觉探针](./model-lifecycle-evidence/2026-11-01-native-helper-vision.json)使用同一固定仓库的主 GGUF 和 F16 projector，在 MTP 关闭时实际识别三张不同输入图片的红、绿、蓝颜色。每张图保留输入摘要和实际模型响应；这不是复杂场景、视觉多轮或产品安装验收。

[ASR/TTS 探针](./model-lifecycle-evidence/2026-11-01-audio-asr-tts-feasibility.json)使用固定 ModelScope Qwen3-ASR-0.6B Q8_0 与 PocketTTS English Q8_0，显式选择固定引擎源码的 spec、CPU/4 线程。ASR 对固定 16 kHz 输入的词错误率为 0；TTS 实际生成 24 kHz/单声道/2.4 秒 PCM，RMS 0.08224、无削波，再经独立 ASR 得到与输入相同的句子，词错误率为 0。参考语音是本地已有 Windows 语音 API 生成的测试数据，不能用它冒充 Lumina 的 TTS 输出；未下载仓库的 Safetensors 声音状态。通用 Audio 理解/生成仍需单独验证。

[生产 Provider 复核](./model-lifecycle-evidence/2026-11-01-small-fixture-production-sources.json)通过新增 Rust example，对五个固定候选仓库、513 个实际文件和允许模型卡重新检查，与前期清单一致。新 example 的该 crate 全 targets 严格 Clippy 和 Windows fmt 已通过。[最新 CPU CLI](./model-lifecycle-evidence/2026-11-01-cli-build-metadata-cpu.json)包含来源文档模块，完整构建通过；六组/84 项 self-test 正在使用该二进制实际运行。CUDA 最新源码构建单独运行，未将构建中计作通过。

现有 Candle Whisper 的 loader 要求 model.encoder.* 张量路径，而实际候选 Whisper GGUF 使用 enc.*；新增可指定模型和音频路径的诊断 example 后，第一次构建因新增 dev-dependency 编译占用资源而主动停止，保留缓存，不计通过。后续以限制并发的方式继续验证该兼容路径和旧听写回归。


## 全量技术台账汇总

最新[Runtime 技术台账](./model-lifecycle-runtime-baseline.zh-CN.md)逐类列出确切引擎/绑定/源码提交、实际结果、来源与未完成工作，覆盖全部原始类型和加速组件。当前 CPU/CUDA CLI 均构建通过，最新 CUDA Qwen3-1.7B 实际 offload 29/29 层并生成 4；最新 CPU 完整 recipe 的 84 项基础检查、4 项明确跳过、1 次真实文本、最终摘要和全部冻结哈希核对通过。

通用 Audio 已分别完成 AST 结构化理解和真实音乐生成探针。DFlash 使用专用架构模型和实际 draft-dflash 路径，通过 tokenizer/维度/目标层核对、三组 greedy 对照以及原生 129 draft/107 accepted 计数；不是普通 draft。

Video 实际产出 17 帧且均可解码、互不相同，内容方向与末帧块状伪影检查失败，保持 EXPERIMENTAL_NOT_READY。既有 Candle 原生 Whisper GGUF 加载也真实失败，名称桥接和旧听写回归留在 ML-29。完整台账保留这些失败及其具体修复路线，M4 未因此减少范围。M0 阶段退出审核和差异检查已经完成；完整工作区 Clippy 失败保持未解决，58 个诊断来源文件与规划 HEAD 一致，不能声称全工作区检查通过。


## M0 阶段退出复核

[退出证据](./model-lifecycle-evidence/2026-11-01-m0-exit-review.json)对应 ML-01～07 的技术基线范围。最终回归为 188 项通过、0 失败、6 项明确跳过；Windows 格式检查及该 crate 严格 Clippy 通过，最新 CPU/CUDA 构建和真实 recipe 通过。全工作区严格 Clippy 失败：lumina-agent 697 项、lumina-mcp 1 项；[诊断来源审计](./model-lifecycle-evidence/2026-11-01-workspace-clippy-m0.json)记录 58 个相关源码文件与规划 HEAD 一致，该结论不能替代全工作区通过。此项保留为后续系统交付门槛。

[模型入口台账](./model-lifecycle-evidence/2026-11-01-model-entry-inventory.json)覆盖桌面/ACP、CLI、显式 backend、缓存、推荐主文件和 projector、Whisper、MLX 与原始传输层。台账的每个入口列出实际符号和迁移工作包，当前未声称统一策略已接入。M1 接续处理全量清单路径、搜索与来源策略，M2～M6 继续按原计划完成。

## M1 来源与发现推进

生产 Provider 的远程清单现在使用 `ObservedRepositoryPath` 保留 Git 原名，只有转换到 `RepositoryPath` 的文件才能进入安装 Artifact。反斜杠、盘符/ADS、保留名和控制字符作为远程观察保留并给出不可落盘诊断，不归一化成其他路径；Git 路径越界、重复条目、符号链接和非法哈希仍拒绝。大小写不同的 Git 文件保留各自来源身份；安装物理路径继续按 Artifact ID 的摘要分配。

[实际来源复核](./model-lifecycle-evidence/2026-11-01-m1-production-sources.json)通过：`calcuis/wan-1.3b-gguf` 固定版本完整保留 63 个文件和 6 个不合规预览名，所有原始文件事实逐项一致，3 个实际 GGUF 的完整 SHA256 与新生产清单一致。原本合法的 SD1.5 仓库清单摘要保持兼容。历史整库拒绝及视频质量失败记录均保留，没有改写为已安装或 READY。

新 Catalog API 显式发送并核对页码、页大小、总数和实际行数；成功空页与失败/缺字段分开，重复 ID、错误页码和截断结果拒绝，未知下载量和访问标记保留未知。真实 `Wan GGUF` 查询两页返回全部 85 个不同仓库。五类检索使用同一受限来源；实现边界依据 [ModelScope 官方 SDK](https://github.com/modelscope/modelscope_hub/blob/main/src/modelscope_hub/api.py) 和 [官方 OpenAPI 使用说明](https://github.com/modelscope/modelscope-skills/blob/main/skills/ms-hub/SKILL.md)，并用实际响应复核。

新 `discover_models` 组合精确仓库核验、原始查询与分页记录、去重、精确/相关候选分类、最大 3 并发来源核验及明确的完整/受限/部分失败/不可用状态。该服务的来源 VERIFIED 只表示固定版本清单事实，文件名候选数量不表示 GGUF 内容或 Runtime 支持。服务整包回归曾完成 202 项通过、6 项明确跳过；[五种真实发现服务场景](./model-lifecycle-evidence/2026-11-01-m1-discovery-service.json)全部通过。随后增量另有专项检查，尚未执行最终全阶段回归，未产生 M1 阶段完成提交。ML-08 的真实受限账号访问、远程 GGUF 内容读取、完整组件配对、硬件预算和 ML-16 全入口策略仍须完成。


### M1 远程内容与来源文档

[远程 GGUF 第二轮](./model-lifecycle-evidence/2026-11-01-m1-remote-gguf.json)覆盖 Whisper、SD1.5、Wan VAE、PocketTTS 和专用 DFlash。固定仓库/版本后只经已观察的 ModelScope HTTPS API/CDN 读取有限 Range，逐次核对 Content-Range 和文件总长，凭据只留在 API 原点。五份实际 Header 均与本地完整文件的独立结构解析一致，输入完整 SHA256 前后相同；每份实际读取 128 KiB～16 MiB。旧第一轮事实另存历史，未覆盖失败或未执行结果。

[内容分类 v2](./model-lifecycle-evidence/2026-11-01-m1-classifier-v2.json)使用这五份实际来源与 Header 记录，正例和跨快照/来源哈希变化反例通过。SD1.5 无架构元数据时仍保留 UNKNOWN，张量组仅提供 ImageDiffusion 候选；Whisper 是 ASR 候选；audio.cpp family/spec 仅提供有限任务线索；DFlash 保持独立组件含义。实际来源发现旧内嵌 Audio 配置含其他格式和平台下载描述，作为[不执行的元数据事实](./model-lifecycle-evidence/2026-11-01-m1-embedded-audio-spec.json)记录，不能替代产品来源和资产策略。

[来源文档复核](./model-lifecycle-evidence/2026-11-01-m1-source-documents.json)实际读取 Qwen 固定版本 LICENSE 与 README 并核对全部字节摘要。许可证/NOTICE 只作为来源文件事实，不声明模型任务；允许名称中的二进制控制字节被拒绝。该记录没有作许可条款或商业使用结论。

[本批次状态](./model-lifecycle-evidence/2026-11-01-m1-source-foundation.json)分别列出整包与后续专项范围：分类 14 项、来源/Artifact 10 项、文档 9 项、Range/凭据 4 项通过；文档与来源各有 1 项需要真实环境的默认跳过测试，实际在线例另列证据。当前 crate 全 targets 严格 Clippy 通过；完整工作区 M0 遗留失败仍未解决。M1 的完整依赖、资源预算、统一入口和最终验证尚未关闭。

## M1 观察存储、冻结计划与当前交付边界

M0 已提交并推送到 `testing`，提交为 `7aa6cfbe9fad627faf705a389b24369a3c2a5daa`。M1 的新增源码与证据仍在工作区；截至本节记录时，CPU CLI 正在重建，M1 尚未形成阶段退出提交。M2～M6 的完整范围继续保留，没有以技术探针替代产品接入。

[观察与资源检查点](./model-lifecycle-evidence/2026-11-01-m1-admission-checkpoint.json)记录数据库升级到 v6、实际 BF16 双分片观察的关闭后重开读取、计划与观察绑定，以及资源不足反例。该检查点当时整包为 244 项通过；后续增加动态缓存和冻结计划检查后，最新整包为 246 项通过、0 失败、6 项明确跳过，日志与摘要保存在[最新 Windows 验证记录](./model-lifecycle-evidence/2026-11-01-m1-windows-gate.json)。明确跳过不视为通过。

资源规则已从 v1 推进到 `local-resource-budget-v2`，记录全部权重、F16/F32 KV、混合架构循环状态、并行与回滚、提示缓存、上下文检查点、计算/辅助缓冲和三份磁盘峰值。实际 35B BF16 双分片总权重为 71,065,942,560 字节，当前 CPU 配置因空闲内存不足被拒绝；该记录没有 GPU、部分 offload 或其他模态的内存准入结论。

[原生资源测量](./model-lifecycle-evidence/2026-11-01-m1-native-resource-measurements.json)在同一 0.8B 内容与固定 helper 构建上完成两次真实 CPU 文本推理。默认配置暴露 8 GiB 提示缓存和每槽最多 32 个上下文检查点；显式关闭后，原生报告 KV 24 MiB、循环状态 19.27 MiB、计算缓冲 37.02 MiB、输出缓冲 0.95 MiB。测量仅对应确切工作负载；辅助缓冲的保守余量不冒充通用实测上限。

新计划规则 `modelscope-gguf-plan-v2` 冻结 Runtime 安装档案、Context/Batch、KV 精度、动态缓存上限、硬件快照与资源预算，全部进入计划摘要。读取和入队时重新依据缓存 GGUF 观察核对预算。旧计划缺省 admission 字段不参与序列化，保留旧摘要和恢复语义；旧格式记录不自动取得新计划准入证据。

[真实来源到计划及入队](./model-lifecycle-evidence/2026-11-01-m1-real-plan-acceptance.json)使用固定 ModelScope 提交的 Qwen3.5-0.8B 实际头部、经完整摘要核对的本地样本及 helper 二进制，以及确切工作负载资源档案。新数据库中有 1 条 GGUF 观察、1 个冻结计划、1 条观察绑定和 1 个 queued 任务，正式安装版本为 0。该正例没有声称已完成下载 worker、产品 Runtime Adapter 或能力 READY。

旧入口已移除推荐文件合成回退和自动 projector 下载。GGUF 解析使用固定提交与版本专属缓存目录，下载前检查实际头部与完整分片，下载后重验头部。旧聊天下载入口对未验证 projector 和缺少来源完整校验和的缓存复用保持拒绝；支持无来源哈希的本地计算校验与长期 witness 由新的受管安装路径接续。HF/MLX 及旧 HF 听写新下载入口受来源策略限制，历史资产和用户设置保留。

Windows 验证脚本保留六套原检查并增加四套 M1 检查。R6 在 PowerShell 5.1.26100.9711 和 7.6.9 上通过静态检查、LF/CRLF 矩阵、两组各 12 项单元检查、8 次真实文本推理和 14 个集成反例；两次完整 foundation 各为 10 套、131 项通过、4 项跳过。完整工作区 Clippy 的 M0 遗留失败仍未清零。

用户明确表示暂时没有可用于验收的授权受限仓库，要求继续其余开发。真实账号的受限仓库正例保持 USER_DEFERRED_UNVERIFIED；公开来源实测、401/403 单元处理与凭据作用域验证分别保留，不合并成账号验收通过。

M1 后续仍需收尾重建后的 CLI 实际 recipe、必要的 CUDA 复核、差异审计和提交推送。M2 的正式下载/安装与迁移、M3 的 Runtime/Agent/ACP/SDK/CLI 接入、M4 的全部类型 Adapter、M5 的模型中心 UI 和 M6 的系统验收尚未交付。旧 Candle Whisper 布局问题和 Video 质量失败继续保留。

### M1 已验证增量提交边界

[本次增量验证](./model-lifecycle-evidence/2026-11-01-m1-verified-increment.json)记录最新 CPU 完整构建通过（10m51s）及 35B 父模型实际 recipe 通过：10 套检查、131 项通过、4 项跳过、1 次实际文本生成和报告/输入摘要核对。最新 CUDA 完整构建通过（18m20s），实际 Qwen3-1.7B offload 29/29 层、原生缓冲 1743.77 MiB，生成 4，未发现 CPU 回退。听写入口曾有三处错误 crate 引用，修正后重建通过，失败日志保留。

本次提交仅交付已验证基础增量，M1 不标记为整体完成：具体组件角色/档案解析、GPU 与多模态资源档案、全部受管下载和缓存导入切换仍需接续。M2～M6 的原始范围保留。用户延期的受限仓库账号正例继续待验收。[本阶段完整工作区 Clippy](./model-lifecycle-evidence/2026-11-01-m1-workspace-clippy.json)先被未修改的 MCP OnceLock 导入阻止，未证明下游检查通过，保持全局质量门槛未解决。
