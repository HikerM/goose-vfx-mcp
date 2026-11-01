---
title: lumina M0 Runtime 技术验证台账
sidebar_position: 15
---

# lumina M0 Runtime 技术验证台账

本台账固定 M0 的实际试验、Runtime 选择和后续实现路线。`PASS` 只对应所链接的有限探针；全部产品能力仍需安装、Adapter、服务和完整 Validator 接入。视频这轮内容验收明确失败，既有 Candle 路径的张量命名也存在实际加载失败。

当前唯一源码为 `D:\Project\goose`，基线 `fa7bb0a4d`、规划提交 `4f6265905`、产品 `1.43.0-mcp.14`。新增构建和验证缓存使用 D/E；无新增 Goose 检出或 worktree。

## Runtime 选择

| ID | 确切版本/提交 | 当前验证范围 |
| --- | --- | --- |
| llama-rust146-cpu | `llama-cpp-sys-2=0.1.146` | cpu, sampler, mtmd |
| llama-rust146-cuda | `llama-cpp-sys-2=0.1.146` | cuda, sampler, mtmd |
| llama-helper-dde-cpu | `dde879bc4f954b809892fd0d660515f42536394a` | cpu, mtmd, embedding, rerank, native MTP, native DFlash |
| audio-helper-c0-cpu | `c0cd77fa80285f8113883e0de06ffc781cf502f6` | cpu, pocket_tts, qwen3_asr, ace_step, ast_audioset, stable_audio, manager disabled, explicit pinned spec |
| diffusion-helper-c6-cpu | `c6dfa9def12fd0cb36ac7149ba30a75792bedbf0` | cpu, Image, Video, web UI/download frontend disabled |

旧 Rust 绑定保留其已验证的 OpenAI 模板/原生工具链。候选 0.1.160 移除这些接口，直接升级会破坏成熟调用方，因此新增类型采用固定提交的原生辅助 Runtime；产品化的进程、鉴权、资源、取消和 SDK 仍是 M3/M4/M6 的交付。

## 全类别实际记录

| 类别 | 实测状态 | 实际结果 | 后续完整交付 |
| --- | --- | --- | --- |
| LLM | PASS | [Qwen3-1.7B actual output4,positive token,29/29 GPU layers and1743.77MiB CUDA model buffer](./model-lifecycle-evidence/2026-11-01-cuda-metadata-text-smoke.json) | ML-25/26 complete multi-turn/tool suites and product admission |
| MoE | PASS | [Qwen3.6-35B-A3B executes actual validation tool,6 suites84pass/4ignored,1 real text smoke and correct report summary](./model-lifecycle-evidence/2026-11-01-selftest-metadata-moe-cpu.json) | All256 experts count toward residency; ML-15 budget,ML-25/26 full validation |
| VLM | PASS | [Actual0.8B main+projector identifies3distinct image colors](./model-lifecycle-evidence/2026-11-01-native-helper-vision.json) | ML-13 pairing profiles,ML-25/26 complex images and visual multi-turn |
| Embedding | PASS | [9 actual1024D finite normalized vectors;3 fixed retrieval top1 correct](./model-lifecycle-evidence/2026-11-01-native-helper-embedding.json) | ML-27 complete install,batched input/bounds and Adapter |
| Reranker | PASS | [3 fixed query/document rankings correct;real scalar logit output](./model-lifecycle-evidence/2026-11-01-native-helper-reranker.json) | Old binding width unsafe for Rank; ML-28 typed raw-score semantics and boundaries |
| ASR | PASS | [Qwen3-ASR actual fixed speech transcript,W ER0](./model-lifecycle-evidence/2026-11-01-audio-asr-tts-feasibility.json) | Speech proof used archived3family binary; expanded build inherits no automatic READY; Candle layout gap separately recorded,ML-29 unfinished |
| TTS | PASS | [PocketTTS generates2.4s24kHz mono actual speech;independent ASR exact](./model-lifecycle-evidence/2026-11-01-audio-asr-tts-feasibility.json) | Speech proof used archived3family binary;ML-30 full install/stream/language/quality and spec/assets boundary |
| Audio understanding | PASS | [AST returns native typed scores,Speech top1=0.8546876](./model-lifecycle-evidence/2026-11-01-audio-understanding-feasibility.json) | ML-33 per-task schemas/negative classes and product service |
| Audio generation | PASS | [4s44.1kHz stereo music;independent ASTMusic top1 andPiano top3](./model-lifecycle-evidence/2026-11-01-audio-generation-feasibility.json) | ML-33 genres/seeds/duration/quality and product service |
| Image | PASS | [Actual512x512 red apple on wooden table;PNGdecode andvisual review pass](./model-lifecycle-evidence/2026-11-01-native-image-feasibility.json) | ML-31 full install/seeds/content suite and GPU |
| Video | QUALITY_FAILED | [3GGUF files produce17 distinct decodable256x256 frames;actual movement,wrongdirection andblockartifacts](./model-lifecycle-evidence/2026-11-01-native-video-feasibility.json) | ML-32 quality gate blocks READY;standard resolution,VAE/precision/temporal-state isolation required;ML-08/09 raw preview path issue |
| MTP | PASS | [Native same-model draft-mtp,143draft/105accepted,3 exact greedy output/token comparisons](./model-lifecycle-evidence/2026-11-01-native-mtp-feasibility.json) | ML-34 statistical correctness/performance,cancellation,tools and product profile |
| DFlash | PASS | [Dedicateddflash69tensor draft,tokenizer/width/layers checked,129draft/107accepted,3 exact comparisons](./model-lifecycle-evidence/2026-11-01-native-dflash-feasibility.json) | ML-35 training lineage/quality/performance/fallback/tools; no ordinary draft substitution |
| Specialized extension | PASS | [Native ast_audioset.classification.v1 structured scores](./model-lifecycle-evidence/2026-11-01-audio-understanding-feasibility.json) | ML-36 trusted extension registry and typed service integration |

## 边界与明确交接

- 当前硬件是约 96 GiB RAM、28 逻辑处理器和 RTX 5070/12227 MiB。实测过的具体配置与原生缓冲区大小有证据；不能据此编造最小硬件或全部 GPU 支持。ML-15 必须输出完整驻留权重、KV、激活、组件与并发预算；未知阻塞准入。
- Audio 的 ASR/TTS 早期证明绑定到归档的三 family 二进制；AST/Music 使用扩展的五 family 二进制。二进制变化不会自动继承 READY，完整 Validator 需要绑定对应 build。
- Video 的三个权重全部是固定 ModelScope GGUF。17 帧均可解码且互不相同，内容方向和末帧伪影检查失败；ML-32 需用标准分辨率、VAE/精度和时序解码对照定位并通过完整门槛。
- 视频仓库存在六个带反斜杠的预览文件名，生产 Provider 当前拒绝整仓；M0 探针使用明确标为 raw 的有界固定清单，不构成生产 RepositoryVerified 或可执行安装计划。ML-08/09 要保留全部原始文件进入摘要，安全安装路径另行验证，不改名或悄悄忽略预览项。
- 已有 Candle loader 实际报 `cannot find tensor model.encoder.conv1.weight`。ML-29 应按确切配置/维度/tokenizer 增加版本化名称适配，保留原始 GGUF 和旧设置，再完成真实听写回归。
- 模型卡许可证只记录固定来源原文字段、文档存在性和哈希。分件、嵌入资源及传递依赖仍需 M6 审核；来源文档不是自动授权结论。

## 投入重估

现有全量范围保持不变。M0 证明可以通过已固定的多引擎路线推进全部类型，也确认了下列必须纳入后续包的工作：

| 增量 | 对应包 | 初步工程人日 |
| --- | --- | --- |
| 原生辅助 Runtime 的协议、进程/环境/资源控制及生命周期 | ML-23/24/37/47 | 8～14 |
| Audio spec/嵌入元数据与原生打包边界、任务 Schema | ML-30/33/36/47 | 6～10 |
| Candle 原生命名桥接与旧听写回归 | ML-29 | 2～4 |
| Video 标准配置、解码/精度和质量定位 | ML-32 | 5～10，取决于对照结果 |
| 原始远程路径与可安装路径分离、完整摘要和诊断 | ML-08/09/16 | 2～4 |

这些是对既有工作包的细化，窗口可能重叠，不作为固定交付日期；不能用本台账替代 M1～M6 的实现和完整验收。
