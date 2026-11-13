---
title: lumina 模型生命周期实施进度
sidebar_position: 14
---

# lumina 模型生命周期实施进度

执行目标为完成[七阶段计划](./model-lifecycle-master-plan.zh-CN.md)，每阶段实现和验证后提交并推送到 `testing`。唯一源码检出为 `D:\Project\goose`；不创建其他项目副本或 worktree，构建、模型、缓存、测试数据和临时输出均使用 D 或 E 盘。

四份完整规划文档已作为规划基线提交 `4f6265905` 并推送到 `origin/testing`。该提交只交付完整计划，不表示 M0 或后续实施阶段完成。

M0 技术基线提交为 `7aa6cfbe9fad627faf705a389b24369a3c2a5daa`，已推送到 `origin/testing` 并核对远程 SHA 完全一致；提交后工作区干净。当前继续实施 M1 和 M2，不以技术基线提交作为整体任务结束。

M1 已验证的来源、观察存储和冻结计划基础增量提交为 `8da22cf34f1b89c846d93cbbd45b95b439040e61`，已推送并通过 `git ls-remote` 核对远端 SHA；见[推送记录](./model-lifecycle-evidence/2026-11-01-m1-foundation-push.json)。M1 整体仍在实施。M2 缓存安装基础增量 `24117f59469f34af8e32de54c7c8919880b5f850` 已推送到 `testing`，在同一数据库和受管目录中完成两个 0.8B GGUF 版本的实际安装、内容复用及旧活动版本保留复核。网络安装增量 `57c007736cfa2870298c3483816d16b5762da576` 和持久化安装输入及数据库驱动恢复增量 `63793ca38400d2f3a666217c1180be2b42935503` 均已推送并核对远端 SHA。后续应用服务及新 ACP/CLI 入口增量已实现并验证，详见末节及 Git 提交记录；M2 整体仍未完成。

## 阶段状态

2026-11-13 的 A41 文本验收在两个受管安装上分别执行了 19 次真实请求。Qwen3.5-4B Q4_K_M 单轮 8/10、多轮 3/3，通过单 token 输出限制，重开数据库后为 READY。Qwen3.5-0.8B Q8 单轮 6/10，低于 8/10 的门槛，重开后保留 FAILED。两个结论均绑定各自的安装清单、引擎和测试语料；MoE 和工具调用仍需独立验收。[4B 文本验收](./model-lifecycle-evidence/2026-11-13-m3-qwen35-4b-a41.json) · [0.8B 文本验收](./model-lifecycle-evidence/2026-11-13-m3-qwen35-08b-a41.json)

2026-11-13 的 A43 受管验收使用 Qwen3.5-0.8B Q8 和配套 F16 视觉投影。模型答对 10/10 张合成图，4/4 组换图对照和 3/3 组多轮对话通过，重开数据库后仍能读取同一份 READY 记录。[A43 验收及逐次输出](./model-lifecycle-evidence/2026-11-13-m3-managed-vision-a43.json)

SD1.5 的受管安装通过缓存校验、注册和重启后的任务目录回读。Qwen3-ASR 恢复了原安装任务，校验后重开服务，读回了对应版本和参数描述。两项验收覆盖安装恢复和模型发现，生成或识别质量由各自的任务套件记录。[SD1.5 安装验收](./model-lifecycle-evidence/2026-11-13-m4-sd15-managed-install-v1.json) · [ASR 恢复验收](./model-lifecycle-evidence/2026-11-13-m4-managed-asr-recovery.json)

SD1.5 的 Windows v2 安装在写入 64 MiB 检查点后正常关闭服务，重启后从保留的进度完成安装。受管目录内权重所占空间峰值为 4,265,169,728 字节，计入临时清单后为 4,265,172,632 字节，低于计划的 4,281,946,944 字节。重开 SQLite 后版本与清单摘要一致，图像生成质量仍待单独验收。[安装、续传和空间采样](./model-lifecycle-evidence/2026-11-13-m4-sd15-managed-install-v2.json)

Wan 2.1 1.3B CPU 的三个组件共 5,496,104,704 字节。Windows v2 受管安装在 64 MiB 检查点关闭服务，恢复后完成全部组件校验和版本注册。再次重开服务，任务目录读回同一版本的 `wan2.1/text_to_video` 参数身份。文生视频的执行与质量仍待单独验收。[Wan 多组件安装和重启验收](./model-lifecycle-evidence/2026-11-13-m4-wan-managed-install.json)

| 阶段 | 当前状态 | 已取得证据 | 尚需完成 |
| --- | --- | --- | --- |
| M0 全量契约和技术验证 | VERIFIED，完整工作区 lint 失败保留 | ML-01～07、全类别真实试验、确切引擎/来源/组件/许可证事实、SQLite v4、状态契约、最新 CPU/CUDA 和完整 recipe；见阶段退出复核 | 产品接入属于 M1～M6；视频质量、旧 Whisper 和完整工作区 lint 仍未通过 |
| M1 来源解析和完整依赖 | IN_PROGRESS，固定 CPU 视觉档案已验证 | 实际发现服务、远程 GGUF、已知组件配对、SQLite v6 观察绑定、CPU 资源预算、不可变计划与入队；固定主模型/projector 的资源档案、有界 PNG 输入、真实多模态计数及旧文本 v2 计划兼容通过 | 更完整的组件角色与档案、GPU 和其他多模态预算、全部入口和受管安装交接；受限账号正例由用户延期 |
| M2 可靠安装和数据迁移 | IN_PROGRESS，真实双组件安装与旧 registry 保护已验证 | 应用服务、ACP/CLI、启动恢复、schema v15 共享 blob 准入、分阶段空间预算和三层有界调度；真实主模型/projector 并发安装、旧 registry 原字节备份及旧 CLI 回退通过；本批 273 项测试、核心严格 Clippy、CLI 构建及实际 self-test 通过 | 计划生成阶段的保守空间检查与非传输阶段的增量重新准入、旧入口统一、多分片验收与性能基准、跨域别名和运行引用保护、卸载/回收、迁移后受管关系及完整阶段验收 |
| M3 Runtime 和核心真实验收 | IN_PROGRESS | A41 在 4B Dense 上达到文本和多轮门槛，0.8B Dense 保留失败记录。A43 在 0.8B 与视觉投影组合上通过。受管 helper 已按安装预算复核启动时内存，支持限量排队和取消 | MoE 的 A41、原生和模拟工具的 A42、跨任务竞争的完整 A39 实测、会话与服务接入、旧引擎兼容 |
| M4 全部类型和加速 | IN_PROGRESS | 两个向量模型经受管验证器分别达到 18/20、19/20，能力登记为 READY，重开数据库后读回一致。PocketTTS 已完成受管安装和任务发现。H3 的文生和首帧生两份原始双流 AVI 均通过媒体格式校验，浏览器卡片通过 11 个场景。持久媒体后端含启动恢复共 52 项通过，真实 MCP 回放、35 次 HTTP 检查和部分 Electron 文件操作已实测 | 逐模型受管执行、对话参数修改和重启复用、完整音视频与图像质量、资源与稳定性验收、能力登记及 MTP/DFlash 对照。剩余 Electron 验收按用户要求暂停 |
| M5 完整模型中心 | PLANNED | 完整界面与服务契约已编写 | 实际页面、SDK、任务及能力状态接入和尺寸验收 |
| M6 系统验收和发行 | PLANNED | 原计划需求与验收映射 | 全类别、硬件、旧功能、安装包和回退验收 |

M0 技术基线已完成阶段退出复核，阶段提交和远程 SHA 以实际 Git 记录为准；这不表示产品支持或后续阶段完成。某个局部测试通过不代表对应阶段或全部能力通过。M1 至 M6 的原始范围保持完整。

2026-11-13，持久媒体和首批四个模型的参数契约已推送到 [testing 提交 17ccb0ed4](https://github.com/HikerM/goose-vfx-mcp/commit/17ccb0ed406baace4e18864788128800edcb741c)。参数回归 28 项和[含启动恢复的媒体后端回归 52 项](./model-lifecycle-evidence/2026-11-13-m4-media-startup-recovery.json)通过。H3 的受管安装、从聊天发起生成及完整音画质量和稳定性仍待验收。

受管音频安装计划和任务目录的本地回归已覆盖两种配置文件的固定来源、权重与配置配对、SQLite 重开、错误版本和 H3 两种输入模式。该轮核心库 195 项通过，单独的任务资源集成 3 项通过。真实音频安装、执行资源实测以及从会话发起并复用任务需要单独记录结果。

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

以上基础代码列表对应早期技术基线；后续 M2 的缓存、网络 worker、持久化输入和实际恢复交付见下方增量记录。旧下载和注册业务编排尚未全部替换；GGUF 结构校验已接入旧下载完成后的入口，应用启动服务和 ACP/CLI 接入正在验证，运行占用保护及用户 registry 迁移仍需实现。诊断子进程故障检查不作为完整产品 ACP/CLI 安装入口的 A29 验收；样本 GGUF 不构成模型推理证据。

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

M0 技术基线已完成，M1、M2 按完整范围继续实施。空间预留当前仅为保守峰值基础；下一步补齐已落盘字节与共享内容的精确预算、三层并发，以及进度事件、运行引用、卸载和用户旧 registry 迁移。M1 的完整组件角色、GPU/多模态预算及旧入口统一仍需完成。各增量验证后提交推送，完整阶段出口满足后才标记阶段完成；M3 至 M6、48 个工作包和 64 项验收保持原范围。

以下记录按增量保留历史观察、失败及修正过程；当前状态以阶段表和末尾最新增量为准。

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

## M2 缓存安装执行增量

`cache_worker.rs` 将已接受的冻结计划接到逐文件账本和正式安装事务。外部缓存只读打开，不移动或覆盖原文件；本地导入必须具有固定来源完整 SHA256。受管传输目录按 job 摘要分配并持有进程文件锁，租约过期不能产生两个字节写入者；复制和校验期间逐块检查 fencing 与续期。恢复先核对已保存前缀摘要，损坏检查点明确重启；已完成文件重验后继续，暂停保留进度。取消请求由停止写入的 worker 确认，重启后也可收敛待取消任务。

持久化任务新增有界分页和恢复查询，分别返回完整历史或排队、过期租约、待确认取消任务，不自动恢复暂停或已取消任务。完成重放核对 job、计划、安装和版本身份，并重新检查正式目录、只读属性及共享内容摘要，不增加安装或任务事件。

[真实缓存安装](./model-lifecycle-evidence/2026-11-01-m2-real-cache-install-r1.json)使用前阶段已接受的 Qwen3.5-0.8B 冻结计划，原始来源为固定 ModelScope 提交 `3bec95a4bc613f19c4a38afd2cec6383aa2535ad`。833,592,736 字节原文件运行前后完整 SHA256 相同，正式模型及 blob 的独立摘要均一致，发布文件只读；数据库登记 1 个版本，任务为 installed/completed，重复执行幂等，能力证据为 0。该结果只证明实际缓存安装，不表示网络下载、产品入口或 Runtime READY 已交付。

冻结源码后的 crate 全 targets 严格 Clippy 通过；库和集成测试合计 14 套、256 项通过、0 失败、6 项明确跳过，其中新增缓存安装目标为 10 项通过。首轮整包编译期间源码仍有变更，结果包含两项取消用例失败；失败日志保留，未计为通过。最新 CPU CLI 重建和 `model-cache-install` 配方仍待完成。MCP 的 Windows 未使用 OnceLock 导入已按实际使用平台限定，完整工作区重查已暴露既有 Agent lint 和 evidence-db 编译问题，全局质量门槛继续未通过。

### 缓存所有权和真实双版本复核

后续回归用例实际复现旧 `.part` 写入通过硬链接修改外部文件。修复后，传输文件独占打开，在任何截断或续写前核对句柄的普通文件属性及唯一硬链接；blob、staging 和 Manifest 设为只读前执行相同检查，正式版本及完成重放也拒绝共享文件别名。Windows 使用稳定系统句柄 API 获取链接数，未使用 Rust 1.94.1 尚未稳定的 `MetadataExt::number_of_links`。外部 blob 的权限保持不变反例也已通过。

所有权修复后的[最终局部验证](./model-lifecycle-evidence/2026-11-01-m2-cache-validation.json)为 14 套、258 项通过、0 失败、6 项明确跳过，新增缓存安装目标为 12 项通过，crate 全 targets 严格 Clippy 和 Windows 格式检查通过。原始硬链接失败及此前失败日志均保留。修复前 CPU CLI 构建通过但不作为最终验证；修复后最新 CLI 构建通过，实际 `model-cache-install` 配方另行记录。

[第二次真实安装](./model-lifecycle-evidence/2026-11-01-m2-real-cache-install-r2.json)在同一数据库和受管根目录接受新冻结计划，保留旧正式版本并登记第二个版本。结果为 2 个版本、1 个实际 blob、2 条版本引用；两个版本文件具有不同文件身份、单一硬链接并保持只读。原始模型、旧版本文件及旧 Manifest 摘要相同，活动版本未自动切换，能力证据仍为 0。验证入口默认继续拒绝修改既有数据库，只有显式 `--reuse-database` 才进行该诊断；未提供选项的真实调用被拒绝且数据库未改变。该开关只用于诊断 example，不构成产品安装 API。

[第二轮完整工作区检查](./model-lifecycle-evidence/2026-11-01-m2-workspace-clippy-r2.json)被既有 `lumina-verified-sources` golden_vectors 目标的 3 项检查阻止，未证明其余工作区通过；前一轮的 Agent 697 项和 evidence-db 32 项仍是未解决记录。M2 的网络传输、后台恢复服务、卸载、引用运行保护及迁移范围保持完整，尚未达到阶段退出条件。

### 固定 Windows 验证入口和实际配方

首轮 `model-cache-install` 实际配方先查看目录，随后将目录切换和重定向加到 Cargo CMD 命令，受到 shim 拒绝，又自行改用其他入口；[该轮失败记录](./model-lifecycle-evidence/2026-11-01-selftest-m2-cache-install-r1.json)保留，不计为规范流程通过。

新增 `bin/test-lumina-cache-install.ps1` 复用既有 Windows 验证入口的路径、UTF-8、计数解析、哈希和 Cargo 执行规则，以固定参数运行缓存安装目标并创建不可覆盖的事实报告。未修改原有真实文本入口及其十套 foundation 的统计口径。[脚本质量矩阵](./model-lifecycle-evidence/2026-11-02-m2-cache-powershell-gate.json)在 PowerShell 5.1.26100.9711 和 7.6.9 上通过两次静态检查、各 9 项单元检查、LF/CRLF 与默认仓库路径合计 6 次实际执行、10 个失败反例；每次实际执行均为 12 项通过、无失败或跳过。可选 PSScriptAnalyzer 未安装，未计为执行通过。

修正后的配方及入口重建后，[第二轮实际 CLI 配方](./model-lifecycle-evidence/2026-11-01-selftest-m2-cache-install-r2.json)通过。最新 CLI SHA256 为 `390e39e7521a9620e8f708d06c35ea81ca538ab2bf760e42ade7d83d2e021ac5`；35B 父模型实际调用一次固定 PowerShell 入口，工具退出 0，读取 12/0/0 的结果及报告，最终摘要、报告摘要和全部冻结输入前后核对通过。该结果验证本次安装边界增量，不表示网络安装、Runtime READY 或完整 M2 已完成。


## M2 固定来源网络传输与进程终止恢复

新增 `lumina-download-manager::managed` 仅接收固定 ModelScope API 来源，手动限制重定向到已观察的 ModelScope CDN。鉴权头只发送至 API 原点，不随重定向进入 CDN。Range、Content-Range、总大小、强 ETag、身份编码、响应截断和有界流式读取均有检查；错误不包含签名地址或令牌。下载器 17 项测试通过。旧回退测试曾将服务端线程完成时间误作并发下载判据，保留该失败后改为核对一次 Range 探测和一次整文件请求及实际文件字节，另有连续 30 次通过的[复核记录](./model-lifecycle-evidence/2026-11-02-m2-network-fallback-stress.json)。

[实际传输探针](./model-lifecycle-evidence/2026-11-02-m2-managed-transfer-real-r1.json)从固定提交完整读取 833,592,736 字节，SHA256 与来源记录一致；第二个 Range 使用强 ETag 和 If-Range，实际字节与本地样本一致。该探针本身不写入安装文件或产生能力证据。

新增 `network_worker.rs` 从已接受计划生成固定来源 URL，复用缓存安装的文件校验、Manifest、只读 blob、目录提交和登记事务。写入前核对所有权和检查点前缀；每个有界检查点先同步文件再保存账本。等待响应和接收分块期间检查租约、暂停及取消。Range 不可用或 ETag 改变只允许一次明确归零；瞬时失败可保留检查点供显式重试，完整的持久化自动退避预算尚未接入。

[实际故障恢复审计](./model-lifecycle-evidence/2026-11-02-m2-network-install-real-r1-audit.json)使用独立诊断入口，在真实网络下载保存 8,395,823 字节检查点后强制终止 worker。旧租约过期后，第二个进程复验前缀和强 ETag，未触发归零，完成全部文件校验及登记。同一数据库和根目录最终为 3 个版本、1 个物理 blob、3 条引用，旧活动版本、旧版本 Manifest 和文件内容保持一致，各版本文件身份独立且只读，原始模型摘要不变，完成重放没有新增任务或文件事件，能力证据仍为 0。

[原验收器失败](./model-lifecycle-evidence/2026-11-02-m2-network-install-real-r1-verifier-failure.json)保留：它错误地预期最终 generation 为 2，遗漏了既有完成登记事务再次递增 generation 的 fencing 规则。独立审计核对完整事件链，确认原 worker 为 1、恢复执行为 2、完成封禁为 3，未重新执行或覆盖原始网络过程。

该批次生命周期整包回归为 263 项通过、0 失败、6 项明确跳过；缓存及网络 worker 边界目标为 17 项通过。两个相关 crate 全 targets 严格 Clippy 与 Windows 格式检查通过。网络安装输入目前仍由诊断请求提供，尚未具备与任务原子入队的持久化输入、启动恢复调度或产品 ACP/CLI 入口；三层并发、持久化重试、卸载、运行引用保护和旧数据迁移继续按 ML-17～22 实施。M1 和 M3～M6 的剩余范围保持完整。


### 网络安装增量交付验证

[完整增量记录](./model-lifecycle-evidence/2026-11-02-m2-network-validation.json)汇总 263 项生命周期回归、6 项明确跳过、下载器 17 项、严格局部 Clippy、Windows 格式检查、真实网络故障恢复及 CLI 配方证据。完整 CPU CLI 重建通过（16m30s）；修改配方超时说明后再次构建通过（26.63s），二进制 SHA256 为 `4d4588df496711d16928e7189083b98080239a2c75ddd082d98ce459806a0c4c`。本轮未重新执行完整工作区 Clippy，前述全局失败仍是发行门槛。

[首轮实际配方失败](./model-lifecycle-evidence/2026-11-02-m2-network-recipe-timeout.json)保留：冷 Cargo 验证超过工具的 120 秒限制，工具返回超时和空 stdout；随后原生日志虽记录 17 项通过且生成报告，也不能覆盖工具失败。父模型另尝试一次不合规的报告读取，该已失败运行被明确终止。配方随后明确要求 `timeout_secs=600`，任何超时、缺失/截断输出或工具错误均立即停止，不再检查报告或另行调用工具。

[第二轮实际配方](./model-lifecycle-evidence/2026-11-02-selftest-m2-network-worker-r2.json)使用独立应用目录和报告路径通过。35B 父模型实际仅调用一次固定 Windows 入口，工具参数确为 600 秒，返回 17/0/0 和报告内容及摘要；无过滤、无跳过，最终摘要与工具结果一致，全部冻结源码、配方、二进制、注册表及原始父模型前后哈希一致。两个 PowerShell 入口未修改，前述跨运行时质量矩阵继续按其原始范围保留。该交付只关闭网络安装基础增量的验证，不关闭 M1、M2 或后续阶段。


## M2 持久化安装输入与数据库驱动恢复

SQLite schema v7 增加不可变安装输入账本，记录 job、原计划摘要、绑定时间、安装及版本身份、规范化受管根目录和传输方式。固定来源网络任务不保存下载 URL 或凭据；缓存任务只保存与原计划逐项匹配的绝对路径，原计划仍要求已知来源完整摘要。输入、任务、逐文件账本和初始事件通过 `enqueue_installation` 在同一事务提交，输入写入失败会全部回滚；相同幂等键的并发连接返回同一个任务。版本身份被独占预留，改变目录、版本、来源方式或缓存选择均拒绝。

`resume_installation` 从输入账本重建请求；`recover_installations_page` 使用有界游标扫描排队、租约过期和待确认取消任务。历史未绑定任务返回需要显式补录，不推断安装 ID；其他目录、损坏输入和凭据不可用分别报告。暂停、已取消和仍持有有效租约的任务不自动恢复。当前实现为顺序执行的有界恢复页，应用启动屏障、持续后台调度和统一产品入口仍待接入，不能视为完整启动恢复服务。

目的地约束同时覆盖 worker、底层 Manifest 提交、目录准备、提交及完成重放。文件验证见证绑定实际规范化根目录，即使另一目录中的 Manifest 和文件内容都通过校验，也不能用于原任务登记。37 项安装边界检查包括这些拒绝场景、数据库重开恢复、版本占用、事务故障回滚、两个独立连接的幂等竞争、输入篡改、凭据隔离、旧 schema 升级与碰撞回滚；最新整包回归为 283 项通过、0 失败、6 项明确跳过，严格局部 Clippy 和 Windows 格式检查通过。

第一轮输入反例曾发现 Serde 的无字段枚举变体忽略额外字段，原测试日志保留失败。修正为严格空结构变体后，嵌套令牌、URL 和未知字段均明确拒绝；凭据提供器错误不写入敏感异常信息，也不领取任务。两个 PowerShell 入口保持原样，仅扩展配方的安装边界范围。

[实际升级和故障恢复](./model-lifecycle-evidence/2026-11-02-m2-durable-recovery-real-r1.json)先备份原 v6 诊断数据库，再升级到 v7，保留已有 3 个安装版本且不虚构历史输入。新任务原子保存输入，实际下载保存 8,395,819 字节检查点后强制终止；第二进程只接收数据库和受管目录路径，从数据库发现任务并在租约过期后恢复，未重新传入请求文件、job ID、安装 ID 或版本 ID。结果为第 4 个正式版本、1 个 blob、4 条引用，输入和旧活动版本保持一致，能力证据为 0。

底层目的地及见证约束补齐后重建的[最新实际安装](./model-lifecycle-evidence/2026-11-02-m2-durable-recovery-guarded-r2.json)再次完成固定来源的完整网络下载、数据库恢复分发、文件验证、目录提交和登记，并验证完成重放。最终为 5 个版本、1 个物理 blob、5 条引用、2 条新任务输入；此前版本、输入、活动版本和原始模型未改变，各版本文件独立只读，能力证据仍为 0。该补充记录验证最新约束的实际正例，不将先前构建的故障注入记录扩展为新构建的故障注入。


### 安装输入增量交付验证

[本批次完整验证](./model-lifecycle-evidence/2026-11-02-m2-durable-inputs-validation.json)绑定最终源码、迁移脚本、283 项回归与 6 项明确跳过、37 项安装边界、严格局部 Clippy、Windows 格式检查及两轮真实数据库恢复记录。最新完整 CPU CLI 构建通过（12m21s），SHA256 为 `e8f193f6651d361075ca84e7a4112c262f4531787d828c6ddaaf88162f1919ea`。

[实际 CLI 配方](./model-lifecycle-evidence/2026-11-02-selftest-m2-durable-inputs-r1.json)通过：35B 父模型只调用一次固定 Windows 入口，使用 600 秒工具超时，工具退出 0，返回 37/0/0、报告内容和摘要；最终摘要计数及冻结源码、迁移 SQL、配方、二进制、注册表和父模型前后哈希均一致。验收只采信实际执行、计数、报告和输入身份；父模型自由描述中的 WAL 崩溃一致性和旧格式迁移标签不扩大这 37 项的覆盖，进程终止与 schema 升级分别以独立真实记录为准，用户 registry 迁移仍未完成。

两个 PowerShell 入口未修改，原跨运行时矩阵按原范围保留。完整工作区严格 Clippy 未在本增量重跑，既有失败仍未解决；M1 和 M2 不作阶段退出声明，M3～M6 继续保留完整范围。

## M2 应用安装服务与 ACP/CLI 接入

当前新增应用持有的安装服务、初次恢复屏障、后台轮询、协作关闭、服务端原子生成版本，以及 ACP 提交/查询/控制和 CLI 同服务入口。原 spec 下载及其他历史入口尚未全部迁移；HTTP 监听后首个客户端连接前的主动恢复、控制幂等回执、逐文件进度与事件分页仍需补齐，不能作为 ML-40 整体完成。

本轮生命周期回归为 292 项通过、0 失败、6 项明确跳过，安装专项为 46 项通过；SDK 请求契约 2 项、ACP 工厂所选 5 项、CLI 新命令解析 1 项及修正旧调用签名后的更新验证失败用例 1 项均通过。所选 ACP/CLI 测试的其他过滤目标不计为通过。Windows 格式检查、两个局部 crate 的严格 Clippy、不启用本地推理的兼容检查和完整 CPU CLI 构建通过。

[首轮真实 ACP 安装验证](./model-lifecycle-evidence/2026-11-02-m2-application-startup-r1.json)为 FAIL：真实接口已接受新任务并在网络下载过程中暂停，但关闭 stdin 后 stdio 连接未结束，应用没有走到安装服务的关闭步骤。该进程在两次关闭等待超出验证上限后由失败清理终止；不能将暂停成功或文件存在记为正常关闭及启动恢复通过。原始二进制、日志、数据库、失败和清理超时记录保留。

已在应用 stdio 适配层增加正常 EOF 结束信号，并将既有协议测试的直接 abort 改为关闭输入后等待服务正常返回。EOF 回归通过，修正后再次执行的工厂 5 项、Windows 格式、局部严格 Clippy、不启用本地推理的兼容编译和完整 CPU CLI 构建均通过；新二进制 SHA256 为 `177fc2b900acbc3d9601ffd57457398482e81d43b4dab101251afd08c7c5b8e9`。

[R2 实际记录](./model-lifecycle-evidence/2026-11-02-m2-application-startup-r2.json)保留 FAIL：三个 ACP 进程已正常退出且安装完成，但验证器将数据库的 JSON 编码状态列直接与普通字符串比较，未执行后续 CLI 检查。该错误属于验证器比较方式，不覆盖 R1 的真实关闭缺陷。

[独立 R3 实测](./model-lifecycle-evidence/2026-11-02-m2-application-startup-r3.json)使用同一修正后二进制及全新命名空间通过。通过 ACP 真实提交服务端生成的任务和版本，下载中暂停后正常关闭；第二次启动保留暂停，拒绝过期版本控制并接受正确恢复；再次正常关闭后，任务重新排队、租约清空，保留 16,791,636 字节和强 ETag。第三个进程仅以 `lumina acp` 和应用目录启动，从数据库发现任务，未重新提交安装请求，续传未归零，最终完整 833,592,736 字节 SHA256、实际版本文件和安装登记一致。

三轮进程退出码均为 0，安装输入与原始模型不变，历史未绑定任务的完整事件仍与初始记录相同。完成请求幂等重放返回原任务和版本，改变安装目标被拒绝。CLI 查询和等待退出 0；过期版本控制退出 1，并核对具体状态版本冲突错误，三个命令前后任务记录不变。能力证据仍为 0，不将安装完成计为 Runtime 或 READY 验收。

[本批完整验证](./model-lifecycle-evidence/2026-11-02-m2-application-service-validation.json)绑定测试、最终构建门槛、源码身份、真实 ACP 记录及原始失败。[实际 CLI 配方](./model-lifecycle-evidence/2026-11-02-selftest-m2-application-service-r1.json)使用上述重建二进制和 35B 本地父模型通过：实际只调用一次固定 PowerShell 入口，工具超时参数为 600 秒，返回 46/0/0，报告内容与 SHA256、最终计数和全部冻结源码、配方、二进制、注册表及原始父模型前后哈希一致。Rust 边界测试、真实网络恢复和 Runtime 能力的验收范围分别保留，父模型自由描述不扩大覆盖。

完整工作区严格 Clippy 的既有失败仍未解决。该批关闭应用服务接入增量的验证，不关闭 M1、M2 或 M3～M6；原七阶段、48 个工作包和 64 项验收范围保持完整。

## M2 HTTP 启动与持久化控制回执

后续增量新增 schema v8 控制回执，与状态 CAS 和任务事件同事务提交；同一幂等键绑定动作、版本、任务和目录，响应标记是否重放。跨数据库连接竞争只接受一次，同键重发返回首次结果；任务后来已恢复时，旧暂停请求重发不会再次暂停。回执写入失败回滚状态和事件，篡改回执或伪造历史结果拒绝读取。SDK 和 CLI 控制入口要求调用方提供幂等键。

HTTP/HTTPS 启动路径新增工厂共享恢复入口，在首个客户端连接前恢复已有任务；不提前创建会话调度器或 MCP 服务，空模型命名空间保持休眠，损坏模型数据库继续隔离。实际 HTTP 路径已验证，独立 TLS 实测及 HTTP 正常停止信号接入仍需完成。

当前生命周期整包 14 套、300 项通过、0 失败、6 项明确跳过；其中安装目标为 54 项通过，含新增 8 项控制/迁移用例。SDK 请求契约 3 项、工厂所选 7 项、CLI 解析 1 项、两个局部 crate 全 targets 严格 Clippy、格式检查、不启用本地推理的兼容编译及完整 CPU CLI 构建均通过；其他过滤测试不计为通过。

[真实 HTTP 与回执记录](./model-lifecycle-evidence/2026-11-02-m2-http-control-receipts-r1.json)通过：使用已归档 v7 诊断程序生成真实固定版本安装输入，随后新 CLI 的 HTTP 监听在未发送任何客户端请求时升级数据库到 v8、领取任务并完成下载及登记。初次升级没有虚构控制回执，安装输入保持不变。独立 CLI 进程首次终态取消返回 `replayed=false`，下一进程重发同一请求返回相同任务结果与 `replayed=true`；改变动作或用新键提交过期版本分别核对具体冲突错误。最终只有一条回执，任务和事件未因终态控制改变，能力证据仍为 0。

HTTP 进程只在安装已完成后强制终止作探针清理，不计为 HTTP 正常关闭验收；实际网络中的暂停/恢复及正常关闭另有此前 ACP R3 记录，v8 活跃控制和历史重放由本批 Rust 集成测试覆盖。

[完整增量验证](./model-lifecycle-evidence/2026-11-02-m2-http-controls-validation.json)汇总上述门槛及最终源码身份，并独立复验实际版本文件和 Manifest 的 SHA256、blob 与版本文件的只读属性、单一硬链接及不同文件身份，数据库完整性和外键检查均通过。CLI SHA256 为 `ad664334fbd3649cc553c6f1b985046aaea28390fd20db08c32c41e2ca9bb3ad`。

[实际 35B self-test 配方](./model-lifecycle-evidence/2026-11-02-selftest-m2-http-controls-r1.json)通过：只调用一次固定 PowerShell 入口，工具超时参数为 600 秒，返回 54/0/0，事实报告内容和 SHA256、最终计数及全部冻结源码、两份测试模块、迁移 SQL、配方、二进制、注册表和原始父模型前后哈希一致。两个 PowerShell 入口未修改，既有质量矩阵继续按原范围保留；54 项边界测试不替代独立 HTTP 实测或 Runtime 能力验收。

该批只关闭 HTTP 启动和幂等控制增量的验证，不关闭 M1、M2 或 M3～M6。完整工作区严格 Clippy 的既有失败仍未解决，原七阶段目标及剩余工作保持完整。


## M2 逐文件持久化重试预算

schema v9 新增逐文件预算及不可变历史事件，保持既有任务、文件和控制回执的序列化摘要。新文件在同一入队事务建立账本，执行前登记逻辑尝试；暂停、正常关闭或租约过期后的继续执行沿用当前尝试。超时、限流和来源暂时不可用按最多 3 次尝试及 2 秒、4 秒退避自动排队，传输层有界 Retry-After 取较大值，当前上限为 60 秒。后台扫描与直接领取均拒绝提前执行，退避不持有租约。预算耗尽和其他非瞬时错误保留 FAILED，等待用户处理。

文件失败、失败码及阶段、预算、任务事件与排队结果同事务提交；事务中断不会留下只有文件失败或只有次数增长的半成品。手动 Retry 为失败文件建立新周期，保留累计次数和上次失败，同键控制重放只返回原回执，不重复重置。完成文件验证同时登记 VERIFIED 预算状态。旧 schema 升级标明 legacy_untracked，不补造历史尝试次数。ACP/CLI 任务列表返回独立 fileRetries，控制响应继续保持原历史任务结果。

新增 11 项边界用例覆盖数据库重开、预算耗尽、手动重试幂等、租约恢复与暂停、非瞬时错误、事务回滚、独立连接竞争、保留检查点、篡改拒绝、升级与回滚以及应用后台按到期时间恢复。本轮整包为 311 项通过、0 失败、6 项明确跳过；安装专项 65 项、SDK 4 项、工厂所选 7 项和 CLI 解析 1 项通过。首轮专项编译发现新增测试读取已移动字段，修正后重跑通过，原始失败日志保留在完整验证记录中。

[实际网络及独立查询](./model-lifecycle-evidence/2026-11-02-m2-retry-network-r1.json)验证新 CLI 在无客户端请求时将旧诊断输入数据库从 v7 升级到 v9，完成固定来源 833,592,736 字节安装，持久化 1 次已记录尝试和 VERIFIED 状态；独立 CLI 读取同一结果，来源、安装输入和控制回执保持一致。该记录是实际下载成功路径；瞬时失败、退避和耗尽由 Rust 集成测试验证，没有在本次真实 ModelScope 下载中注入对应故障。

[完整增量验证](./model-lifecycle-evidence/2026-11-02-m2-retry-validation.json)记录两个相关 crate 严格 Clippy、格式、不启用本地推理的兼容检查及完整 CPU CLI 构建，并复核实际版本文件、Manifest、只读 blob、独立文件身份和数据库完整性。[实际 35B self-test](./model-lifecycle-evidence/2026-11-02-selftest-m2-retry-r1.json)只调用一次固定 PowerShell 入口，使用 600 秒工具超时，返回 65/0/0；报告与全部冻结输入前后哈希一致。两个 PowerShell 入口未修改，既有跨运行时矩阵按原范围保留。

凭据获取、安装前准入和非文件提交失败尚未全部纳入统一失败详情；三层全局并发、空间预留、运行引用保护、卸载及用户 registry 迁移仍待实施。完整工作区严格 Clippy 的既有失败保持未解决，M1、M2 和 M3～M6 不作阶段退出声明，七阶段、48 个工作包和 64 项验收范围保持完整。


## M2 结构化失败与凭据恢复

schema v10 增加与历史任务事件绑定的不可变失败记录。凭据获取、格式不合法的临时令牌及安装前准入失败，对绑定同一受管目录且版本仍一致的任务记录 BLOCKED；暂停、取消及其他 worker 的有效租约不能被旧结果覆盖。拥有有效租约的非文件执行和提交错误记录 FAILED；文件失败与已有预算、状态和事件同事务提交。升级不补造 v9 及更早的失败历史。未绑定输入、其他目录和损坏输入继续保留恢复诊断，不推断失败归属。

生产 ModelScope 配置桥接现在区分缺省凭据与配置值类型或存储读取错误；后两者以固定诊断返回，不把原始凭据值写入错误。阻塞任务在后续轮询和重启时不再自动领取，处理后需显式 Retry。取消确认直接走取消路径，不要求先读取凭据。ACP/CLI 查询增加 failures，包含来源、阶段、错误码、恢复性、任务版本和 causeId；isCurrent 表示失败快照与返回的任务是否一致，重试或完成后保留历史详情。

首轮专项 75 项通过、1 项失败：重启控制用例未等待初次恢复完成即断言 BLOCKED。加入 wait_ready 后专项 76/0/0；原失败日志保留。新增 11 项覆盖跨轮询和重启、显式重试、暂停/租约竞争、实际提交路径障碍及修复恢复、磁盘准入分类、事务回滚、篡改拒绝、schema 升级与回滚、当前和历史失败查询及取消时跳过凭据。磁盘分类采用受控输入测试，不表示实际磁盘耗尽验收。

首轮构建检查在核心 Clippy 处失败：受管目录判断可简化为 is_none_or。修正后以严格 lint 优先的顺序重跑全部最终检查，原记录保留。整包回归 322 项通过、0 失败、6 项明确跳过，SDK 5 项、工厂所选 7 项、生产凭据配置专项 3 项及 CLI 解析 1 项通过。两个相关 crate 严格 Clippy、Windows 格式、不启用本地推理的兼容检查和完整 CPU CLI 构建通过；完整工作区严格 Clippy 的既有失败保持未解决。

[实际配置失败及恢复](./model-lifecycle-evidence/2026-11-02-m2-failure-network-r1.json)在全新应用目录使用新 CLI：合成的错误类型配置使已接受的真实 ModelScope 安装在领取文件前进入 BLOCKED，数据库从 v7 升级到 v10；重新启动仍保留同一失败与零次文件尝试。修正配置后任务继续保持阻塞，独立 CLI 显式 Retry 才开始完整公开模型下载。最终 833,592,736 字节的大小和 SHA256 一致，旧失败历史保留，查询 isCurrent 为 false，同键重发返回原重试回执。该场景不需要私有账号，也不构成受限账号授权正例。HTTP 进程仅在阻塞或完成后强制结束作探针清理，不作为正常关闭验收。

[完整增量验证](./model-lifecycle-evidence/2026-11-02-m2-failure-validation.json)绑定最终源码与二进制、检查门槛、实际失败恢复、版本/Manifest/blob 物理复验和数据库完整性。[实际 35B self-test](./model-lifecycle-evidence/2026-11-02-selftest-m2-failure-r1.json)只调用一次固定 Windows 入口，600 秒工具超时，返回 76/0/0，报告及冻结输入前后哈希一致。两个 PowerShell 入口未修改，既有跨运行时矩阵按原范围保留。

本批关闭结构化失败与凭据恢复增量，M1、M2 和后续阶段仍未完成。三层全局并发、空间预留、进度游标、旧入口统一、运行引用保护、卸载及用户 registry 迁移继续按原工作包实施；七阶段、48 个工作包和 64 项验收范围保持完整。


## M2 保守峰值空间预留与恢复

schema v11 在任务接受和绑定安装输入的同一事务内保存空间预留及不可变历史。按实际磁盘卷身份协调同一生命周期数据库中的任务，受管目录不同但卷相同也共享预算。当前采用安装计划的完整峰值；暂停继续持有，失败、阻塞和取消在取得传输写锁、确认写入停止及任务版本未变化后释放。安装完成和预留释放同登记事务提交；Retry/Resume 在控制事务中重新准入，同键回执重放不重新申请。

旧数据库升级不触碰文件系统，已有绑定的非终态任务标记 needs_admission，并暂按未知卷保守计入预算；实际恢复前重新核对目录和卷。未绑定的诊断任务不伪造输入或预留。后台释放使用有界分页和游标，忙任务不会使后续可释放任务饥饿。写入前检查其他任务的预留和当前实际剩余空间。查询增加 storageReservations，保留峰值、当前预留和状态，不暴露内部目录。

新增 12 项专项覆盖独立连接竞争、同卷不同目录、重复提交、暂停及写锁释放、失败重试、事务回滚、登记原子性、篡改拒绝、schema v10 升级及回滚、分页和查询。整包 334 项通过、0 失败、6 项明确跳过；安装专项 88/0/0，SDK 6 项、工厂所选 7 项、凭据配置 3 项和 CLI 解析 1 项通过。两个相关 crate 的严格 Clippy、格式、关闭本地推理的兼容编译和完整 CPU CLI 构建通过。完整工作区严格 Clippy 既有失败没有关闭。

[真实产品提交和空间恢复](./model-lifecycle-evidence/2026-11-02-m2-storage-network-r1.json)先用旧诊断程序准备实际测量过的公开模型计划和未绑定任务，再由新产品 CLI install-plan 提交独立安装。初始事件证明接受时已持有峰值预留；合成的错误类型凭据配置使任务阻塞并释放，重启保持不变；修正配置后显式 Retry 重新预留，完成 833,592,736 字节的固定版本下载、校验及登记后释放。同键重试重放不重复预留，旧未绑定任务保持原样，能力证据仍为零。HTTP 仅在阻塞或完成后强制结束作探针清理，不作为正常停止验收。

[完整验证](./model-lifecycle-evidence/2026-11-02-m2-storage-validation.json)复核实际版本、Manifest、只读 blob、独立文件身份和数据库完整性。[实际 35B self-test](./model-lifecycle-evidence/2026-11-02-selftest-m2-storage-r1.json)执行唯一固定入口并返回 88/0/0，报告与冻结输入前后哈希一致。

本批为保守峰值预留基础，不是最终精确空间算法：尚未按已落盘字节减少预算或扣除共享 blob 节省量；不分配操作系统物理块，不协调独立数据库，也不能阻止其他程序写满磁盘。精确增量核算、三层并发及其他 M2 工作继续保留。M1、M2 和 M3 至 M6 不作阶段退出声明，七阶段、48 个工作包、64 项验收范围保持完整。


## M2 HTTP 和 HTTPS 正常关闭

`lumina serve` 接入 Windows Ctrl+C/Ctrl+Break 和 Unix SIGINT/SIGTERM。HTTP 与 HTTPS 收到信号后停止接收连接，给已接收连接最多 10 秒排空时间，随后等待应用关闭。应用继续使用已有的安装检查点和控制状态，关闭中的任务撤销旧租约并重新排队。

[Windows 实测](./model-lifecycle-evidence/2026-11-03-m2-http-shutdown.json)使用同一固定 ModelScope 安装任务，先启动 HTTP，再启动 HTTPS。两个健康请求均返回 200，HTTPS 通过服务端证书校验并协商 TLS 1.3。两次定向 Ctrl+Break 均正常退出，检查点从 8,395,806 增至 16,791,606 字节，前缀哈希一致。检查点历史单调，传输维持一次逻辑尝试，旧租约失效，任务保留空间预留。该场景验收关闭与续传，完整文件安装见上一批真实安装证据。

CLI 构建、格式及 5 项 serve 专项通过。[真实 35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-http-shutdown-r1.json)返回 88/0/0，报告和冻结输入通过哈希复核。首两轮探针的端口选择和代次断言问题保留在验收记录。M2 仍需精确空间核算、三层并发、旧入口统一、进度事件、运行引用保护、卸载和迁移。


## M2 安装事件游标续读

ACP 安装事件查询和 CLI `local-models job-events` 按两个独立游标读取任务状态和文件进度。每页各返回最多 256 条事件，空页保留调用者的游标。客户端可按事件序号续读，文件状态和字节数来源于持久化账本。

[产品验收](./model-lifecycle-evidence/2026-11-03-m2-event-cursors.json)用先前完成的真实 ModelScope 安装账本验证进程重连。两个 ACP 进程跨连接续读 117 条任务事件和 102 条文件事件，逐条匹配 SQLite 序号。CLI 与 ACP 对同一页返回一致，最终文件状态为 verified，已完成与预期字节数均为 833,592,736。CLI 构建通过，[35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-event-cursors-r1.json)执行 89 项安装边界用例，失败和跳过均为 0。桌面总体进度、速度与 ETA 留在 ML-42，M2 精确空间核算、并发和数据迁移仍需完成。


## M2 发布阶段增量空间核算

schema v12 保留 v11 历史预留事件。进入 staging 后，安装器持有共享摘要的写锁，按尚未发布的唯一 blob、独立版本文件和 Manifest 计算空间。文件持久化后扣减对应预算。重试会重新检查磁盘上的文件，并按当前缺失文件更新预留。

真实 GGUF 验证在同一受管库安装两个独立版本，第二次复用第一份 blob。SQLite 记录两个版本引用，磁盘上保留一个 blob 和各自独立的版本文件。测试覆盖 blob 已持久化、预算事件提交失败后的重试。安装专项 90/0/0、相关 crate 严格 Clippy、完整 CLI 构建及实际 35B self-test 均通过。下载阶段和首次准入仍使用保守峰值预算。[真实复用与预算轨迹](./model-lifecycle-evidence/2026-11-03-m2-publication-budget.json) [35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-publication-budget-r1.json)


## M2 下载检查点空间扣减

schema v13 将文件同步后的检查点、逐文件空间抵扣和预留事件写入同一事务。工作进程收到重下原因时先恢复对应预留，再截断旧文件。暂停和正常关闭保留抵扣，进入发布或释放预留时清理逐文件抵扣账本。旧检查点和历史预留沿用原规则，首次准入和释放后重新准入仍使用保守峰值。

真实 ModelScope 下载经 HTTP 和 HTTPS 两次正常关闭与续传，停机后持有预留等于计划峰值减去已同步前缀。随后在隔离的验收目录破坏一个前缀字节，重启检测到损坏，恢复完整预算并记录新的下载检查点，逻辑尝试数仍为一。专项注入文件事件失败，确认检查点和扣减同时回滚。相关 crate 严格 Clippy、完整 CLI 构建和实际 35B self-test 91/0/0 均通过。[下载恢复和预算轨迹](./model-lifecycle-evidence/2026-11-03-m2-transfer-credit.json) [35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-transfer-credit-r1.json)


## M2 重试准入复用保留的下载抵扣

schema v14 让失败或阻塞任务释放容量时保留已经同步的下载抵扣账本。重新准入持有传输写锁，核对同卷、受管路径、单链接文件身份和文件长度。仅抵扣完整保留的账本字节，缺失或截短的文件按全额预算。恢复 worker 继续验证前缀哈希，发现损坏时在截断前恢复预算。新任务和没有可保留抵扣的记录继续按保守峰值准入。

真实 ModelScope 下载在已有持久检查点后注入 SQLite 文件事件故障。失败任务释放预留并保留抵扣，产品 CLI 显式 Retry 按剩余预算重新准入，随后 HTTPS 继续下载。回归覆盖完整文件、截短文件和重复控制请求。相关 crate 严格 Clippy、完整 CLI 构建及实际 35B self-test 92/0/0 均通过。[重试准入验收](./model-lifecycle-evidence/2026-11-03-m2-readmission.json) [35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-readmission-r1.json)


## M2 共享 blob 首次准入

固定摘要计划的安装准入在同一事务中写入任务、输入、空间预留与共享 blob 的临时引用。登记过的只读单链接 blob 在目标存储中存在且长度吻合时，可减少一次 blob 写入预算。同摘要多文件合并 blob 写入预算，下载文件和独立版本文件仍各自计费。发布时复验内容哈希，实际缺失文件重新核算。释放时清理临时引用，完成登记的版本保留正式引用。

真实 GGUF 双版本安装中，第二次准入少预留一份共享 blob。安装边界覆盖差一字节拒绝和刚好满足预算的接受路径。相关 crate 严格 Clippy、完整 CLI 构建及实际 35B self-test 93/0/0 均通过。计划生成阶段继续使用保守峰值，后续卸载和垃圾回收需尊重临时引用。[首次准入验收](./model-lifecycle-evidence/2026-11-03-m2-blob-admission.json) [35B self-test](./model-lifecycle-evidence/2026-11-03-selftest-m2-blob-admission-r1.json)

## M2 受管根目录任务并发（ML-18 增量）

应用服务和分页恢复入口使用同一受管根目录的两个跨进程任务槽。任务槽与每任务调度锁一起持有，跨服务扫描同一排队任务时不会重复执行；同一版本已有的写入锁和数据库 fencing 继续约束发布。发布相同摘要时，等待方保留任务租约并响应暂停、取消和关闭。

[双 CLI 进程真实安装证据](./model-lifecycle-evidence/2026-11-04-m2-task-slots.json)记录两个产品进程、三个实际 GGUF 缓存任务及共享文件锁竞争。两个任务安装期间，另一进程扫描后第三个任务仍保持原排队版本。释放文件锁后，三个版本均校验登记，三个独立版本文件共用一个受管 blob，SQLite 完整性和外键检查通过。

首轮 91 项通过、3 项失败，原因是锁接口没有保留空间回收依赖的 Windows 锁冲突错误。恢复原始错误链后，Windows 格式、94 项安装专项、核心 crate 严格 Clippy 和 CLI 构建通过，[真实 35B self-test](./model-lifecycle-evidence/2026-11-04-selftest-m2-task-slots-r1.json)返回 94/0/0。ML-18 仍需文件与 Range 两层并发、联合预算和实际性能基准。

## M2 文件及 Range 并发（ML-18 增量）

网络 worker 每任务同时处理最多两个文件，每文件最多两个 8 MiB Range；同一受管根目录的四个文件槽和四个 HTTP 槽限制所有 worker 的总量。下载按顺序写入并同步连续前缀，沿用 SQLite v15 账本与空间扣减规则。Range 不可用或 validator 变化会释放该文件未完成请求，然后恢复空间预留、截断并从零使用完整流，最多一次。首个文件失败会停止兄弟 future，保留已同步的前缀。

本批 Windows 格式、核心 crate 严格 Clippy、CLI 构建通过。123 项单元测试、96 项安装集成和 11 项传输协议测试均通过，无失败或忽略；协议目标另有 6 项不在该筛选范围。新跨文件用例通过正常目录、GGUF 观察、资源评估与冻结计划 API 创建合成物理分片，使用真实 SQLite 与同步文件检查点验证失败和空间抵扣。

[真实网络证据](./model-lifecycle-evidence/2026-11-12-m2-network-concurrency.json)记录两个产品 CLI 进程安装两个固定 ModelScope 单文件任务，观测两个同时活动文件和四个同时占用的 HTTP 槽。两个正式版本的大小、完整 SHA256、独立文件及共享 blob 引用均通过检查，逐段前缀与完成后的预留释放一致。两个任务从首个 worker 启动至全部完成约 304 秒，包含下载、校验和发布，不作为下载速度或并发提速对照结论。[真实 35B self-test](./model-lifecycle-evidence/2026-11-12-selftest-m2-network-concurrency-r1.json)返回 96/0/0，工具调用、原生日志、报告读回和冻结身份一致。

实际多组件或多分片模型的完整并发验收、并发参数性能基准、运行引用、卸载、旧 registry 迁移及阶段退出继续待完成。M1 视觉组件资源档案仍需完成输入与编码 token 上界约束，实测边界见下节；旧文本预算和冻结计划兼容性必须保留。

## M1 视觉资源实测与输入上界

[三轮原生测量](./model-lifecycle-evidence/2026-11-12-m1-vision-resource-measurements.json)复用固定 Qwen3.5-0.8B 主模型和 projector、既有 CPU helper，不复制模型权重。Context 4096、Batch/Microbatch 512、单序列、四线程，显式关闭提示缓存、上下文检查点及推测解码。十次实际颜色识别均得到预期结果；测量记录原生分配日志、Windows 进程峰值、实际响应和运行前后内容身份，不作为完整视觉质量或产品 Runtime 验收。

原生 `image-max-tokens=1024` 不能独自形成硬上限：最低和最高图像 token 均设为 1024 时，128×1024 与 1024×128 输入各实际产生 1092 个图像 token；1×1023 输入有完整的 2048 图像 token 原生分批记录。转置极窄图也返回 2080 个总提示 token，但退出前的缓冲日志只留下四批中的两批，记录明确保留此限制。固定引擎源码显示，最低像素上采样后分别向上对齐宽高，没有再次执行最大像素检查。针对 1～1024 整数宽高的 float32 算术转写枚举得到 2048 的候选最大值，该计算仅作为尺寸推导证据。

因此本批状态为 `MEASURED_BOUNDARY_VIOLATION`，尚不产生可运行的视觉资源准入档案。下一步须将输入尺寸、预处理、实际编码 token 约束和资源余量绑定到确切 Runtime 与组件，完成超限拒绝和旧 v2 文本计划兼容测试，再验收主模型与 projector 的受管安装链路。

## M1/M2 视觉双组件与旧注册表保护

已实现固定内容和原生 CPU 构建的视觉资源档案、双组件冻结计划诊断入口，以及图片字节冻结、PNG 有界解压和完整提示容量检查。旧文本 v2 的真实冻结计划用于序列化、摘要和准入回归。原生 Runtime 参数绑定、会话输入接线与能力 READY 仍属于 M3。

旧注册表现在先保存原字节备份和独立不可变导入记录，保留未知字段、原设置和缺失 storage 声明；损坏数据显式报错，跨进程保存使用来源摘要 CAS。历史删除只解除登记；失败 pending 清理使用唯一身份及完整快照 receipt。`local-models legacy-imports` 提供备份和诊断入口。源码审查发现并修复了分片重排时未知字段错配、typed 值相等不足以证明 pending 所有权，以及 PNG 实际解压可超过 IHDR 尺寸三个问题。

首轮编译因 SHA256 返回值不支持 LowerHex 失败，改用项目现有摘要类型后，第二轮 123 项单元、19 项迁移、96 项安装、15 项观察与计划、20 项视觉资源测试通过，共 273 项通过、0 失败。另有 1 个显式忽略的子进程辅助入口，由跨进程父测试实际调用，不计作通过测试。Clippy 的整除写法及示例跨 crate 摘要调用修正后，核心 crate 全目标严格 Clippy、20 项视觉复跑和两个诊断示例构建通过。完整 CLI、PowerShell 双运行时矩阵、真实双组件安装、旧 CLI 回退与实际 self-test 均已完成本批验收，证据如下。

[原生多模态计数验收](./model-lifecycle-evidence/2026-11-12-m1-vision-preflight.json)已通过：实际持久化双组件计划为 RUNNABLE，预算计入两组件共 1,038,579,840 字节权重，主机内存需求 1,922,807,936 字节。使用同一冻结 PNG 和聊天请求，原生 `/v1/chat/completions/input_tokens` 与实际生成返回的完整提示 token 分别为 1056、1124、2080，三个颜色结果正确；增加输出额度至超出 Context 均被 Rust 边界检查拒绝。该诊断复用既有已验证源文件，其隔离安装任务仅入队，未启动 worker；不能将此记录计作真实网络安装或产品 Runtime 接入。

[PowerShell 验证入口矩阵](./model-lifecycle-evidence/2026-11-12-model-assets-powershell-gate.json)在 Windows PowerShell 5.1.26100.9818 和 PowerShell 7.6.9 上通过静态检查，LF/CRLF 四组实际执行均为 39 项通过、0 失败、1 个不计入通过的辅助入口；每个运行时另有 9 个结果解析正反例通过。新增 self-test 阶段 `model-assets` 使用同一固定入口，要求一次真实工具调用并读回报告。

完整 CLI 已成功构建，实际 Cargo 包名为 `lumina-agent-cli`，产物 SHA256 为 `f094ed6f26115aa5c7115ed526fc3b1e4f0ac8614a8bfc8c2da93a6feb8ed925`。既有 `lumina-agent` 605 项警告及 CLI 6 项警告保留，不将本批核心 Clippy 通过表述为完整工作区 lint 清零。

[真实双组件安装](./model-lifecycle-evidence/2026-11-12-m2-paired-vision-install.json)通过产品 CLI 从固定 ModelScope 来源安装 833,592,736 字节主模型及 204,987,104 字节 projector。观测两个文件、四个 HTTP 槽同时活动；两个正式文件大小、完整 SHA256、独立文件及 blob 引用、逐文件连续前缀、预留释放、数据库完整性均通过。安装耗时约 299 秒，包含传输、校验和发布，不作为并发提速对照。能力证据表仍为零行，未将 INSTALLED 标为 READY。

[旧注册表 CLI 回退](./model-lifecycle-evidence/2026-11-12-m2-legacy-registry-cli.json)使用升级前二进制 `f5451e48…` 与本批二进制实际读写隔离注册表。四种 storage 声明（包含字段缺失）均原样导入并保持待验证；重复导入幂等，解除手工路径别名不删除原模型/projector。旧二进制可读取更新后的登记，恢复原字节备份后两代 CLI 均正常读取；既有设置、未知字段、默认配置和会话数据夹具保持。损坏 JSON 的诊断和列表命令均返回失败，原损坏字节与备份一致。该记录只证明旧 JSON 兼容与回退，不声称旧二进制支持新增视觉计划。

[实际 35B self-test](./model-lifecycle-evidence/2026-11-12-selftest-model-assets-r1.json)使用本批构建和更新后的 recipe 完成一次真实工具调用，返回 39 项通过、0 失败、1 个不计入通过的辅助入口。原生测试日志、报告内容及读回 SHA256 一致，模型最终总结准确保留范围边界；运行前后源码、recipe、二进制及相关输入的冻结摘要一致。该结果是上述专项的模型工具闭环验证，不另计为新增 39 项独立测试，也不代表 M3 Runtime READY。

本批完成的是 M1 视觉预算与 M2 迁移保护增量，M1/M2 整体仍为 IN_PROGRESS。下一批 ML-20/21 需同时保护正式版本、安装临时引用、旧别名和实际运行引用。只读核对发现旧别名可通过任意 main/shard/projector 路径指向受管版本或 blob，registry 锁与受管版本锁目前不在同一协调范围；不能先扫描一次旧 JSON 后删除，也不能以进程内已加载列表替代运行 guard。应在实际登记提交和卸载提交处统一协调、重检目标状态，并持运行 guard 到 native 对象实际释放；恢复也必须重新核对引用。此项随受管卸载及运行接入继续实施，本批旧删除仍只解除登记。

## 2026-11-13 受管能力验收更新

- [Qwen3.5-0.8B Q8 的 A41 实测](./model-lifecycle-evidence/2026-11-13-m3-a41-managed-dense.json)完成全部 19 次调用。单轮 6/10，低于预注册门槛 8/10。三组多轮和 1-token 输出限制通过，SQLite 重开后文本能力仍为 FAILED。
- [向量能力登记](./model-lifecycle-evidence/2026-11-13-m4-managed-vector-ready.json)分别为 Qwen3 Embedding 18/20、BGE Reranker 19/20。两份报告均按文件字节复算摘要，与恢复后的 SQLite READY 证据一致。
- [H3 双流与 MP4 导出](./model-lifecycle-evidence/2026-11-13-h3-mp4-exports.json)覆盖两份既有样片。原生 AVI 校验专项 11/11。另行转换的 MP4 经完整解码，均为 256×256、24 fps、22 帧，含 AAC 音轨。原生双流校验已推送至 `cb39f05bd`，正式任务自动发布 MP4 仍待接入。

## M4 向量资源与原生边界测量

[固定向量档案实测](./model-lifecycle-evidence/2026-11-12-m4-vector-resource-protocol.json)使用已有 Qwen3 Embedding 0.6B Q8 与 BGE Reranker v2 M3 Q4_K_M、同一确切 CPU helper，未下载或复制权重。Context/Batch/Microbatch 512、单序列、CPU 四线程、GPU 0、F16 K/V、Flash Attention on；关闭提示缓存、检查点、自动适配和推测解码。每个模型三个双语检索场景均将正确文档排第一，但不替代正式二十场景门槛。

Embedding 的十六条、每条 511 token 批次通过，512 token 原生请求返回 400；Reranker 的十六条、每个配对 512 token 批次通过，513 token 原生请求返回 500。产品适配器必须在推理前拒绝超界配对，不能把服务错误当正常重排结果。现有协议回归也拒绝固定引擎的缺失 embedding 分数哨兵，同时允许普通负 logit。

Windows 内存采样分别取得 719 和 303 个样本，目标间隔 50 ms，实际最大间隔 78 ms；同时记录操作系统累计峰值。两个模型的峰值工作集分别为 628,539,392 和 542,519,296 字节，采样私有内存峰值分别为 208,420,864 和 418,484,224 字节。候选预算分别保留 64 MiB compute、256/512 MiB auxiliary，独立全额计入权重，Embedding 另计 56 MiB KV；候选总额为 1,033,415,168 和 1,042,356,640 字节。该余量来自固定输入边界的保守工程估计，尚未登记为正式资源准入，也不构成任意硬件、输入或并发下的通用内存上限。

113 个原始证据文件的大小与 SHA256 已逐项复核。两个 helper 均已终止并等待退出。后续工作是将精确参数和资源档案接入正常计划、安装、受管 owner 及产品命令，并用已固定的二十个双语场景通过实际 Rust 适配器验收；本轮不产生能力 READY。

## M3 受管运行与真实 CLI 增量

[实现和构建记录](./model-lifecycle-evidence/2026-11-12-m3-runtime-assets-verification.json)覆盖实际文件占用保护、旧 Provider 缓存身份、冻结启动参数、受管 helper、完整消息/工具/PNG 请求预检以及向量协议适配器。修正 Windows 测试连接模式、路径断言和测试类型别名后，最终核心回归为 202 项通过、0 失败、4 项明确跳过；其中三个是由父测试调用的子进程辅助入口，一个需要真实 GGUF 环境，不计为通过。核心 crate 全 targets 严格 Clippy 通过。

完整 CLI 构建通过，用时 15 分 22 秒，SHA256 为 `f70b810ce32ac215cf88999d51bfdeca47086a13f7a0d889ec5eea00ac6646be`；原有核心 605 项及 CLI 6 项警告仍保留。[真实产品调用](./model-lifecycle-evidence/2026-11-12-m3-installed-generation.json)复用已登记 `paired-vision-v1` 安装及原主模型/projector，未复制权重。文本请求返回 `4`，提示/生成 token 为 30/2；真实 PNG 请求返回 `red`，提示/生成 token 为 1058/2，图像摘要与输入一致。超出完整上下文、非法输出额度和损坏 PNG 三个反例均被拒绝；各命令后 helper 退出已确认，模型和二进制摘要未变化。这是固定 CPU 档案的基础链路验证，不替代完整多轮、工具和视觉质量套件。

[旧 Provider 运行引用证明](./model-lifecycle-evidence/2026-11-12-m3-legacy-runtime-assets.json)使用此前已验证的 Qwen3-1.7B 文本资产。实际输出 `4` 后保持同一 ACP 进程和缓存，再发起第二次真实生成；生成期间和空闲缓存期间，Windows 写入/删除访问及独占字节锁均被拒绝，退出后恢复，权重前后完整摘要一致。测试只申请访问权限，没有写入、重命名或删除模型。

旧视觉配对兼容仍未通过：直接把深层受管路径登记到旧 Provider 时，本批与上一版二进制都报无法打开文件；使用 Windows 扩展路径后，旧进程内引擎对该 Qwen3.5 MTP 文件报缺少 `blk.24.ssm_conv1d.weight`。新受管 helper 已成功执行同一双组件安装，但不能因此宣称旧进程内引擎或旧 projector 路径通过。旧文本证明、新受管视觉证明与单元生命周期测试各自保留范围。

[PowerShell 最终矩阵](./model-lifecycle-evidence/2026-11-12-runtime-assets-powershell-gate.json)在 5.1/7 和 LF/CRLF 四组合均通过 69 项专项、0 失败、3 个不计为通过的辅助入口，命名测试集合一致；静态、解析与十三条结果门禁反例也通过。矩阵明确使用最终预编译测试程序；此前 socket fixture 与 harness 作用域失败记录保留。

[实际 35B self-test](./model-lifecycle-evidence/2026-11-12-selftest-runtime-assets-r1.json)随后使用新 CLI 和正式 `runtime-assets` recipe，模型实际调用一次 PowerShell/Cargo 入口，返回 69/0/3；测试名称、报告内容与哈希、最终模型摘要及运行前后冻结输入均核对通过。该记录不另计新增 69 项独立测试，也不产生能力 READY。

本批是已验证增量，M3/M4 整体仍为 IN_PROGRESS。后续将同一受管生命周期用于向量及媒体能力，并完成正式资源准入、统一服务、Validator 和全部阶段验收。

## M4 视频单变量排查

[一次 VAE direct-conv 对照](./model-lifecycle-evidence/2026-11-12-m4-video-direct-conv-diagnostic.json)保留原模型、seed、尺寸和帧数，只切换实际可生效的 CPU VAE 卷积实现路径。运行成功退出，但新 AVI 与原基线 SHA 完全相同，十七帧解码摘要也逐帧相同；实际查看的 0/8/16 帧仍有明显绿色方块，运动方向未满足提示，状态保持 QUALITY_FAILED。此实验没有改变卷积的 F16 精度，不能表示已经排除精度原因。下一步在固定 latent 上独立回放同一 VAE，以减少反复完整采样并区分解码和编码问题；诊断实现与验收仍在继续。

[固定 latent 回放](./model-lifecycle-evidence/2026-11-12-m4-video-latent-replay.json)随后完成：原采样约 476 秒，仅 VAE 回放约 94 秒；两个独立进程的十七张无损 PNG 逐字节相同。方块失真在视频编码前已经存在，AVI/MJPEG 不能是唯一原因；但尚未区分 latent 质量、VAE 数值计算或像素转换，状态保持 QUALITY_FAILED。首次诊断对张量维数的判断错误已保留，修正为实际五维并通过十七个反例及一个五维正例后才执行替代实验。后续复用相同 latent，避免重复完整采样。

## M4 MiniMax H3 完整 GGUF 候选

[生产来源观察](./model-lifecycle-evidence/2026-11-12-h3-production-source-observation.json)固定了 FL2VA pruned Q4 主模型、完整 Qwen3-VL Q4 编码器和完整视频/音频 VAE 四组件，全部来自 ModelScope，权重合计 35,149,418,880 字节。源码约束逐项核对视频 VAE 的 560 个必需张量、音频 VAE 的 917 个张量，未发现需要混用 safetensors 或新增权重桥接的缺口。旧候选缺少的视觉塔、视频编码器及音频 buffer/卷积权重问题不能靠重命名全部修复，新候选已避开这些问题。

使用既有独立示例实际调用生产 ModelScope Provider，四次退出均为零；共读取 524,288 字节前缀，固定来源、头部摘要及全部张量描述与独立解析一致。视频 VAE 的五维张量由通用读取器实际解析通过。完整权重尚未下载，因此来源清单中的全文件 SHA 仍是期望值；安装、资源准入、原生生成、质量、稳定性、GPU 和 Runtime READY 均未验证。H3 文生和首帧图文生视频继续作为独立目标推进，Wan 结果不替代该项验收。

## M4 向量受管调用与 Whisper 新布局

[向量实现和验证](./model-lifecycle-evidence/2026-11-12-m4-vector-runtime-verification.json)将固定 CPU 资源档案、ModelScope 真实观察、冻结计划、受管 helper 和 Embedding/Reranker 适配器接通；新增 `local-models plan-vector/embed/rerank`。计划输出包含安装命令所需的实际摘要。整个分词与推理操作共用 owner 的请求保护，超时或取消后不复用状态不确定的 helper。资源参数完整绑定输入字节/token、批次、CPU 线程、缓存及原生设置，文本和视觉旧计划仍保持原序列化。

[真实安装和相关性验收](./model-lifecycle-evidence/2026-11-12-m4-managed-vectors.json)通过产品 CLI 安装 Qwen3 Embedding 0.6B Q8 与 BGE Reranker v2 M3 Q4_K_M，正式权重共 1,077,527,456 字节；正式文件、blob、磁盘 Manifest 和 SQLite 内容核对一致。冻结二十组中英文场景分别命中 18/20 与 19/20，达到预注册至少十八组门槛；未命中场景保留，没有修改样本或门槛。空输入、超长输入、未知请求字段和错误能力四个 CLI 反例被拒绝且没有启动新 helper。首次验收脚本把 SDK 的 `versionId` 误读为 `version_id`，记录保留；修复后复用已经成功的安装，没有重新下载。能力证据仍为零行，不把 INSTALLED 或本次外部验收写成产品 READY。

本批核心回归为 237 项通过、0 失败、4 项默认跳过；真实受管向量用例随后显式执行并通过。修复新增资源结构触发的大枚举警告后，核心全目标严格 Clippy 通过。PowerShell 5.1/7 与 LF/CRLF 四组合各为 94/0/3，命名测试集合一致。完整 CLI 与 Whisper 示例构建通过，CLI SHA256 为 `d364e2c2df12598373bdcf4e9d81610cd437f95019e4cc3b6c9bb65319aece66`；既有主包 605 项和 CLI 6 项警告保留。以上专项与真实模型结果分开记账，不累加为重复通过数量。

首次实际 35B `runtime-assets` self-test 暴露了取消测试的时序竞态：150ms deadline 在请求完整发送前触发，而夹具固定断言已收到一次请求。[修复记录](./model-lifecycle-evidence/2026-11-12-m4-vector-cancel-regression.json)保留原 FAIL；外部取消改为等待真实首个分词请求到达后才 drop future，内置 deadline 保持 150ms，并检查超时类型、禁止后续批次与 helper 复用。原十五个命名用例全部通过，核心严格 Clippy 和新的 PowerShell 四组合各 94/0/3 通过。仅该测试程序重新编译，其余程序身份与前一矩阵一致，不宣称整批 237 项又执行了一次。

更新 recipe 后再次构建 CLI/Whisper 示例通过，用时 1 分 21 秒，二进制摘要不变。[第二次实际 self-test](./model-lifecycle-evidence/2026-11-12-selftest-m4-runtime-assets-r2.json)在最终代码上由 35B 模型调用正式入口，九个目标合计 94 项通过、0 失败、3 个不计为通过的子进程辅助入口；原生日志、命名集合、报告读回和运行前后文件身份均一致。此记录是 runtime-assets 专项，不替代完整工作区、ASR 质量、前端、所有模型类型或 M3/M4 阶段验收。

[Whisper 新布局实测](./model-lifecycle-evidence/2026-11-12-m4-whisper-native-smoke.json)使用固定 ModelScope Base Q8_0，按真实 GGUF 头部映射 244 个 Candle 张量，在内存转换并校验完整词表；纠正 bundled tokenizer 中被品牌替换污染的词项和 merge，既有磁盘 sidecar 不被改写。新布局采用源模型要求的居中反射、周期 Hann、单边功率及 mel 前处理，超过三十秒的输入按段保留；legacy 前处理仍调用原 Candle 路径。十四项单元全部通过，包含独立 NumPy 数值基线、跨段边界和 legacy 位级一致性。固定 5.155 秒英文合成语音实际转录与参考完全相同，WER 为 0；本次约 89 秒包含 dev CPU 模型加载和转录，不作为性能通过。完整 A46 多语言/长音频/噪声、取消恢复、ModelScope 安装与既有听写设置接入仍需完成。

## M4 聊天内联音频显示

[音频显示验证](./model-lifecycle-evidence/2026-11-12-m4-acp-inline-audio-ui.json)覆盖 ACP 音频内容保留、同一消息去重，以及普通用户/助手消息、恢复消息、历史详情和 MCP 工具输出中的原生播放器。播放器默认不自动播放，支持键盘操作；音频以受限 Blob URL 加载，组件替换、卸载和解码失败时释放。输入限制包含 MIME 白名单、合法 base64 和每段 32 MiB 解码预算，两个既有 CSP 的媒体来源仅增加 blob。含音频的用户消息暂时隐藏仅支持文本的编辑操作，防止重发时丢失音频；文本和图片编辑继续保留。

六个测试文件共 62 项通过，桌面完整类型检查、改动文件 ESLint 和国际化检查通过；新增文案支持中英文，其他语言沿用既有英文回退。实际 Chrome 验证使用真实组件和 ACP 工具转换链路，成功解码已有 5.155 秒 WAV、播放、暂停和折叠卸载，390～2560 像素宽度没有横向溢出；损坏音频显示解码错误。首次默认 Vitest fork worker 停滞后，使用单 threads worker 完成相同用例，未删减测试。

本项证明组件和浏览器播放链路，不代表打包 Electron 与真实后台会话端到端通过，也不证明 Lumina TTS 已生成音频。该批提交时 Rust `acp/server.rs` 的 `build_tool_call_content` 仍过滤工具 Audio 和 ResourceLink；后续转发修复见下一节。MediaResultRef 持久化、本地媒体文件服务、视频渲染、打开/定位/复制路径/另存为及文件重启恢复仍需继续实现。

## M4 真实 MCP 媒体转发与 H3 完整权重

[媒体转发证据](./model-lifecycle-evidence/2026-11-12-m4-mcp-media-forwarding.json)记录服务端 Audio/ResourceLink 转换、受支持的注解和资源元数据保留、混合结果顺序，以及同一转换器在实时通知和历史回放中的行为。四项后端测试全部通过；前端资源字段展示相关三文件共三十项通过，完整类型检查和改动文件 ESLint 通过。资源 URI 以纯文本展示，不冒充尚未接入的打开/下载功能。

完整 CLI 构建通过，用时 31 分 53 秒，SHA256 为 `ba38f985d367c64c8f2ac0cbc0ea59018fcf6ca7ed454c1e95aad87c70e48a36`；主包 605 项和 CLI 6 项既有警告保留。固定 35B 模型真实调用一次 stdio MCP 工具，取得文本、2.4 秒 WAV、资源链接、文本四块结果；随后关闭 ACP 进程、重新加载同一 SQLite 会话，内容、顺序及工具 ID 一致，数据库四条消息且完整性检查为 `ok`。音频是此前 PocketTTS 原生生成的文件，本次没有重新合成。

实际捕获的实时与回放通知经当前 ACP adapter、组件和由当前源码构建的 Tailwind/Vite 样式，在 Chrome 的 480×400、930×700、1366×768、1920×1080、2560×1440 十组场景中均完成键盘播放/暂停，解码时长为 2.4 秒、readyState 为 4，没有自动播放、横向溢出或 JS/CSP 错误；十张截图已审阅。首次使用旧构建 CSS 时缺失间距类的记录保留。这是浏览器组件链路验收，尚不代表打包 Electron、持久媒体引用或文件操作通过。

最终实际 `lumina run --recipe lumina-self-test.yaml` 的 `media-forwarding` 阶段由 35B 模型调用一次验证入口并正确汇总 PASS。此前三轮失败分别为夹具把必需整数字段写成 null、空扩展覆盖了 MCP 配置、1.7B 调度器执行了额外无关工具；记录均保留。修复夹具后加入实际工具列表前置检查，未修改产品行为来迁就测试，也未放宽一次调用的判定。

[H3 完整权重证据](./model-lifecycle-evidence/2026-11-12-h3-full-weight-downloads.json)记录固定 Revision 的主模型、文本/视觉编码器、视频 VAE、音频 VAE，共 35,149,418,880 字节，全部完成整文件 SHA256 比对。编码器的首次网络失败与按分段校验后的续传记录同时保留。权重位于当前项目验证资产目录，未登记产品安装或 READY；CUDA 引擎构建、真实文生/首帧图文生视频、音视频质量和桌面播放仍需继续验证。

## 2026-11-13 H3 原生输出与媒体接线增量

[H3 原生生成记录](./model-lifecycle-evidence/2026-11-12-h3-native-video-smoke.json)已补齐 CUDA 构建以及文生、首帧图片加文字两种实际生成。两例均为 256×256、22 帧、24 FPS、8 步、种子 42，原生视频约 0.917 秒；本机原生执行分别约 215.859 秒和 189.39 秒。视频各帧和音轨完整解码通过，选取的原始帧可见可辨认的苹果及位置变化。固定 FFmpeg 生成保留视频/音轨的 WebM 预览，两个预览再次完整解码通过。该记录不等于完整运动/音频听审、长时稳定性、受管安装或能力 READY，也不证明实际 GPU 峰值满足预算。

[视频卡片浏览器验收](./model-lifecycle-evidence/2026-11-13-h3-media-card-browser.json)直接使用上述真实 WebM、当前 `MediaResultCard`、当前 Tailwind 样式和 CSP。两案例分别覆盖 480×400、930×700、1366×768、1920×1080、2560×1440，另含中文场景，共 11 场景完成非自动播放、原生键盘播放/暂停与进度条跳转；停用会话阻止继续播放，缺失文件状态移除播放器，恢复后不自动播放，中文失效提示正确。首轮小窗口控件落到首屏下方，已将视频最大高度改为 `min(24rem,50vh)` 后重验并审阅截图。中文测试宿主最初没有展开翻译描述、REPL 回调保留旧宿主地址以及暂停事件即时断言的记录均保留；最终功能和截图判定使用修正后的宿主及有界事件等待。该范围仍是浏览器组件，不冒充 Electron 原生协议和文件操作验收。

持久媒体代码已覆盖受管发布、真实格式解码与摘要、稳定会话/工具引用、只在提供给模型的临时对话副本中恢复内联内容、作用域票据、HTTP 分段读取和原生动作的文件句柄生命周期。当前媒体后端专项为 46 项通过，SDK 请求专项 4 项通过；本轮桌面 9 文件 136 项通过。AVI 已加入已验证文件动作的 MIME 接受列表，浏览器不支持的编码仍显示明确状态。新 CLI 和 schema 生成器构建通过，用时 16 分 56 秒；SDK 已按 Rust schema 重新生成并构建，SDK 31 项既有回归和测试类型检查通过；桌面完整类型检查在修正 mock 函数类型与旧语言目标不支持的数组调用后通过。既有构建警告保留，不声明全工作区严格 Clippy 通过。

[真实持久 MCP 会话](./model-lifecycle-evidence/2026-11-13-m4-durable-media-mcp.json)已用新 CLI 和实际 35B 本地模型通过：仅调用一次实际 MCP 工具，得到原生 TTS 生成的 2.4 秒 WAV；实时消息保存为稳定结果引用，五种用途的作用域访问/释放成功，其他会话不能读取该结果。关闭进程后，新的 ACP 连接在加载会话前被拒绝；正式加载后回放相同工具结果，元数据、文件内容和引用保持一致。SQLite 完整性为 `ok`、原会话四条消息、一个 ready 媒体记录，历史不保存内联音频。首轮验证器错误地要求公开传输保留内部错误数据，已依据既有 ACP 脱敏实现改为核对完整公开错误；首轮失败保留，产品行为和负例门槛未放宽。最终独立流程耗时约 262 秒，不属于新 TTS 合成或 HTTP/桌面文件动作验收。

[真实 HTTP 验收](./model-lifecycle-evidence/2026-11-13-m4-durable-media-http.json)在同一持久会话和同一 CLI 上通过，未复制数据库、构造假会话或重新推理。实际 WebSocket 加载会话并签发作用域票据，35 次 HTTP 请求覆盖完整读取、HEAD、Range、鉴权、会话与用途隔离、释放及连接断开；状态分布为 200×7、206×6、416×2、401×2、403×6、404×12。原生动作路径响应持有句柄时，即使票据已释放或连接断开，Windows 无修改的写入/删除权限探测仍被拒绝；取消响应流后权限恢复。四条历史消息、媒体元数据和 WAV 摘要前后不变，SQLite 完整性为 `ok`。这不是系统剪贴板、保存对话框或外部播放器验收。

[真实 Electron 部分验收](./model-lifecycle-evidence/2026-11-13-m4-media-electron-partial.json)使用当前生产 main/preload/renderer 和真实 TLS 后端、原 SQLite 会话，已完成音频解码及原生播放/暂停/定位、206 Range、系统剪贴板复制路径、原生另存为及取消，以及 Explorer 打开并选中文件；保存文件长度和 SHA256 与登记结果一致。打开文件只确认系统调用和 Windows 应用选择器，不算外部播放器解码通过。用户按 Escape 后停止桌面自动操作，并明确要求继续暂停桌面操作、先推进代码；结果封存为 PARTIAL/USER_STOPPED。原生覆盖拒绝、跨窗口/子框架隔离和会话切换/关闭后的实际通道清理仍待验收。停止后仅进行纯终端检查：完整桌面类型检查、此次选定九文件 134 项测试、定向 ESLint/Prettier/i18n 及差异空白检查均通过；受测源码摘要未变。测试应用、未提交的对话框和剪贴板现场保留，没有擅自恢复桌面控制或清理。

[本批持久媒体 self-test](./model-lifecycle-evidence/2026-11-13-selftest-m4-durable-media.json)通过：实际 `lumina run --recipe lumina-self-test.yaml` 的 media-forwarding 阶段，由本地 35B 模型准确调用一次验证命令；内层实际 MCP、结果持久化、作用域访问与进程重启回放全部通过，模型正确汇总 PASS 与 2.4 秒音频。全过程约 512 秒，CLI、recipe、验证器及受测输入摘要前后一致。该证据固定于本批媒体 CLI，不覆盖随后新增的启动恢复接线或参数模块。

[启动恢复与媒体回归](./model-lifecycle-evidence/2026-11-13-m4-media-startup-recovery.json)共 52 项通过，新增六项验证从真正关闭的 SQLite 重新打开后自动完成重命名前后的 prepared 发布、活跃 writing/prepared 句柄保护、两个 storage 并行恢复、首条损坏 journal 隔离和整轮查询失败时普通会话可用。恢复只处理已验证的提交，不回收未验证 writing，也不按年龄推断死任务。主包测试构建用时 31 分 34 秒，测试执行约 1.18 秒；既有 156 项编译警告保留。此结果不覆盖后续有限任务 owner、资源模块、双流 AVI 和完整配置表的新代码。

本节记录持久媒体与首批参数基础增量，M3/M4 整体仍未完成。桌面验收按用户要求暂停；新增启动恢复尚未据此更新原 CLI/桌面验收结论。H3 双流原始 AVI/WebM 尚未进入正式媒体发布 Adapter。完整参数快照及对话多轮改参仍需正式服务接线，不能用已生成的短视频样片替代这些交付。

## 2026-11-13 不同模型的任务参数实现

`lifecycle/task_parameters` 已新增类型化参数适配接口和精确身份注册表，按安装版本、Manifest、组件摘要、Runtime、任务变体和参数 schema 选择；不同类型的 Adapter 保留独立请求、描述和准备结果类型。首批 SD1.5 文生图、H3 文生视频音频和首帧视频音频分别提供字段约束、默认值、完整有效配置、种子、输入引用及参数来源。指定父结果修改时检查会话和完整模型绑定，保留未修改字段，生成新的任务与配置差异。原生 argv 使用独立参数及待 owner 解析的文件位置，用户输入不作为 shell 命令。

[冻结后的参数专项](./model-lifecycle-evidence/2026-11-13-m4-task-parameters.json) 15 项全部通过，包含模型/引擎切换、未知及不支持选项、帧数/时长组合、参数实际传递、快照读回、多轮修改与跨会话拒绝。交叉审查发现非有限浮点经 JSON 变成 null 后被误当成未修改；已在转换前明确拒绝，并验证两个模型的 CFG 及视频时长补丁。首次构建期间实现与新增测试读取时点不同，旧库上的两项新回归失败保留；重新冻结并编译后 15 项通过。全工作区 Windows 格式检查和 `lumina-local-inference --all-targets` 严格 Clippy 通过；依赖既有警告保留，不代表全工作区严格 Clippy 通过。

[ASR/TTS 参数模块](./model-lifecycle-evidence/2026-11-13-m4-audio-task-parameters.json)已实现 PocketTTS English 与 Qwen3-ASR 0.6B 各自的 Request、Descriptor、Snapshot 和参数计划，固定模型、spec 与 Runtime 的摘要；语音参考/音频输入使用同会话结果引用。Pocket 的声音、种子和温度等参数按原生准备阶段绑定，每次要求新的准备过程；不支持的语速、情绪等显式拒绝。Qwen 独立处理上下文、语言、最大 token 与固定分段/整段模式，原生语言列表不等于多语言质量已验收。实际首轮发现 Serde 无字段枚举分支丢弃额外字段，改为空结构体分支保持 JSON 外形并拒绝冲突；原失败与强化断言保留。重新编译后音频 13 项及图像/视频 15 项共 28 项通过，且经其他 agent 核对真实 CLI、spec、准备与运行代码的参数消费路径。音频本批只实现创建请求，父结果补丁尚未实现。

这些结果仍是参数模块验证，不是对话生成能力通过。所有准备结果均要求独立资源准入，不因匹配既有短样片参数而自动 READY。任务服务、受管执行、实际结果持久化关联、Agent/MCP 入口和取消控制仍需接线。不同类型不继承图像/视频字段，也不把某个模型的选项视为同类全部模型均支持。

下一接线沿现有 `installed_version`、`verify_installed_version`、`RuntimeAssetGuard`、同 store runtime permit 与 Windows Job/child 回收原语增量实现有限 CLI owner；现有 llama HTTP helper 启动与健康检查不能直接套到 sd-cli/audio-cli。对应图像/视频/音频资源档案和即时准入必须单独补齐，现有 llama KV 预算不能替代它们。输入媒体由主包 `open_content` 校验并把保护句柄所有权交给整个任务生命周期；原生输出先写专属临时目录，确认进程退出后有界复制到既有 `StagedMedia` 再发布，磁盘预算计算两份峰值，不放宽第二写者保护。完整参数快照需与任务及媒体引用持久关联；ASR 转录是文本结果，不假冒音频。H3 双流 AVI 仍不满足当前单视频流 AVI 校验器，必须补对应格式验证或受管转换后再接入。
