# REQUIREMENTS

- 项目：LUMINA-ENTERPRISE
- 目标：形成企业可用且具备项目级智能开发能力的Lumina产品
- 阶段：REQUIREMENT_RECONCILIATION
- 活动需求：6（当前切片 6）

## 当前需求

- **REQ-001 [must]** All supported model providers and custom OpenAI-compatible providers must remain configurable, testable, selectable, and usable after the Lumina separation; external provider IDs and real endpoints must not be mechanically renamed.（验收：Saving a provider preserves other configured providers and secrets.；Connection testing uses the selected provider's real configuration without sending project or conversation content.；A new session and an existing session can both use the selected provider and model.；Vendor-owned model IDs, API paths, OAuth identifiers, and endpoint hostnames remain exactly as required by that vendor.）
- **REQ-002 [must]** Whole-project development workflows and all user sessions must remain persistent, reloadable, and usable across application restart, upgrade, custom installation path, and legacy-data import.（验收：Opening the same project directory returns the same project and preserves its work history.；Project runs remain linked to their sessions after restart.；Existing sessions retain messages, working directory, provider, model, and Lumina mode.；An interrupted upgrade or import leaves the original data intact and can be retried or rolled back.）
- **REQ-003 [must]** The Lumina commercial runtime must not contain or depend on the legacy product protocol, storage namespace, release infrastructure, telemetry endpoints, or trust domains; legacy compatibility must be isolated in an optional one-time importer.（验收：The primary Lumina executable does not link the legacy migrator.；The primary runtime accepts lumina:// links only and preserves MCP and ACP standards compatibility.；Legacy data is verified and copied by a separate importer before being transformed into the Lumina schema and trust domain.；Cold start produces no non-loopback network traffic.；Legal attribution remains in LICENSE, NOTICE, MODIFICATIONS, and SBOM artifacts.）
- **REQ-004 [must]** Lumina 自有的全部用户可见内容必须统一使用简体中文，生产版本不得在翻译缺失时回退为英文。（验收：安装器、首次启动、主页、项目、任务、会话、设置、MCP、扩展、通知和错误提示全部使用简体中文。；删除任意必需的简体中文翻译键会使生产构建失败，而不是显示英文默认文案。；供应商名称、模型标识、协议缩写、文件路径、命令和代码保持真实技术值。；第三方英文错误被映射为中文原因和处理建议，未知错误显示中文错误编号。）
- **REQ-005 [must]** Lumina 必须从以对话为主的桌面应用演进为项目级智能开发平台，支持持久任务、多文件变更、终端验证、Git 隔离、工具调用、中断恢复、审核和回滚。（验收：同一项目目录在重启后保留任务、会话、运行、步骤、检查点、变更和验证历史。；智能体可以在授权范围内完成至少三个关联文件的修改并运行真实验证命令。；用户可以逐文件和逐变更块接受或拒绝修改，并可以回滚整个任务。；本地模型在 GPU 和纯 CPU 环境分别完成规定的项目任务，且不静默切换云端。；浏览器和桌面控制只能在统一权限、审计、停止和恢复能力完成后开放。）
- **REQ-006 [must]** Lumina 企业版本必须具备统一权限、凭据保险库、出站网络控制、审计证据、签名发布、数据恢复和扩展供应链治理。（验收：所有文件、命令、网络、模型、浏览器、桌面和 MCP 操作经过同一个策略决策入口。；密钥不以明文出现在普通配置、日志、数据库字段或界面中。；冷启动和离线模式没有未经授权的非本机连接。；安装包、更新清单和企业扩展均可验证签名，篡改后被拒绝。；升级、迁移和扩展更新失败均可恢复上一可用状态。）

## 冲突与未知

- 当前未记录显式冲突。
- 当前未记录未知项。

## 关键决策

- Checkpoint：AUTO_RECORDED
- 决策模式：automatic_non_blocking
- 已锁定：无

> 决策 Checkpoint 自动记录后非阻塞继续，不设置人工审批暂停点。完整历史和被裁剪需求见 `.ai/requirements/ledger.json`，会话只加载本文件活动切片。
