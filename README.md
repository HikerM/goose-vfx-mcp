# Lumina

Lumina 是独立维护的本地 AI Agent 桌面产品，面向 Windows、本地模型和 MCP 扩展工作流。产品下载、更新、支持与数据处理必须由实际的 Lumina 发行方负责。

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

## 独立发行声明

Lumina 是基于 Apache-2.0 许可代码开发的独立衍生发行版，不是 Goose、Agentic AI Foundation、Linux Foundation 或 Block, Inc. 的官方产品，也未获得这些组织的背书。`Goose`、相关组织名称及其标识仅在许可证、来源归属和兼容性说明所必需的范围内出现。

上游版权与许可证必须保留。详见 [LICENSE](./LICENSE)、[NOTICE](./NOTICE) 和 [MODIFICATIONS.md](./MODIFICATIONS.md)。商业发布前还必须完成 [COMMERCIAL_RELEASE.md](./COMMERCIAL_RELEASE.md) 中的门禁；仓库代码本身不能替代目标市场的商标检索和法律审核。

## 功能

- MCP 扩展中心、手动扩展配置、工具发现与诊断导出。
- MCP 配置导入/导出，敏感值不会写入导出文件。
- 本地 GGUF 模型、下载完整性校验和 ModelScope 私有模型令牌支持。
- 简体中文桌面体验、便携版更新与 Windows 本地推理优化。
- 独立的 `lumina://` 深层链接、安装标识和用户数据目录。
- 默认关闭的更新通道；只有构建时配置发行方实际拥有的 GitHub 仓库后才会启用在线更新。

## 快速开始

商业下载和自动更新通道尚未启用。发行方创建并验证自己的仓库、代码签名身份和更新资产后，使用 `LUMINA_RELEASE_OWNER`、`LUMINA_RELEASE_REPO` 与相应签名配置生成发行包；未配置时应用以便携模式运行且不会访问伪造或上游更新源。

从源码运行：

```powershell
cd lumina
. .\bin\activate-lumina-rust.ps1
& .\bin\cargo.cmd build
```

已有本地数据必须通过一次性迁移器显式导入：

```powershell
lumina migrate --dry-run
lumina migrate
```

Lumina 主运行时只读写 Lumina 命名空间，不会静默回退到旧产品的数据、协议、环境变量或信任域。旧名称只存在于独立迁移器、历史来源说明和许可证材料中；迁移是本地、显式、幂等且不删除源数据的。签名 MCP/evidence 状态不会直接继承信任，迁移器只将其归档，必须在 Lumina 中重新授权或重建。

## 项目链接

- [商业发布门禁](./COMMERCIAL_RELEASE.md)
- [迁移契约](./LEGACY_MIGRATION.md)
- [上游来源](https://github.com/aaif-goose/goose)

## License

Lumina 中源自上游的代码继续受 Apache License 2.0 约束。Lumina 自有修改的具体授权以相应文件和发行物附带的许可证声明为准；任何附加商业条款都不得削减 Apache-2.0 对上游代码授予的权利。
