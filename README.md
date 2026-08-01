<div align="center">

# goose-vfx-mcp

_面向本地模型与 MCP 扩展工作流的 Goose 自定义发行版_

<p align="center">
  <a href="https://opensource.org/licenses/Apache-2.0"
    ><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg"></a>
  <a href="https://discord.gg/goose-oss"
    ><img src="https://img.shields.io/discord/1287729918100246654?logo=discord&logoColor=white&label=Join+Us&color=blueviolet" alt="Discord"></a>
  <a href="https://github.com/HikerM/goose-vfx-mcp/actions/workflows/ci.yml"
     ><img src="https://img.shields.io/github/actions/workflow/status/HikerM/goose-vfx-mcp/ci.yml?branch=main" alt="CI"></a>
  <a href="https://insights.linuxfoundation.org/project/goose"><img src="https://insights.linuxfoundation.org/api/badge/health-score?project=goose"></a>
  <a href="https://repology.org/project/goose-cli/versions"><img src="https://repology.org/badge/tiny-repos/goose-cli.svg" alt="Packaging status"></a>
</p>

<a href="https://trendshift.io/repositories/25298?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-25298" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/25298" alt="aaif-goose%2Fgoose | Trendshift" width="250" height="55"/></a>

</div>


`goose-vfx-mcp` 是由 HikerM 维护的 Goose 自定义发行版，重点增强普通用户在 Windows 桌面端使用本地模型和自定义 MCP 扩展的体验。

当前自定义能力包括：

- MCP 扩展中心、手动扩展配置、工具发现与诊断导出。
- MCP 配置导入/导出（敏感值不会写入导出文件）。
- 本地 GGUF 模型下载完整性校验，以及 ModelScope 私有模型令牌支持。
- 简体中文界面和便携版更新提示。

本项目基于 [AAIF Goose](https://github.com/aaif-goose/goose) 开发，并继续采用 Apache-2.0 许可证。上游链接仅用于来源说明；本发行版的下载、问题反馈和更新均以 HikerM 仓库为准。

A native desktop app for macOS, Linux, and Windows. A full CLI for terminal workflows. An API to embed it anywhere. Built in Rust for performance and portability.

goose works with 15+ providers — Anthropic, OpenAI, Google, Ollama, OpenRouter, Azure, Bedrock, and more. Use API keys or your existing Claude, ChatGPT, or Gemini subscriptions via [ACP](https://goose-docs.ai/docs/guides/acp-providers). Connect to 70+ extensions via the [Model Context Protocol](https://modelcontextprotocol.io/) open standard.

goose is part of the [Agentic AI Foundation (AAIF)](https://aaif.io/) at the Linux Foundation.

# 快速开始

**[下载 HikerM 桌面版本](https://github.com/HikerM/goose-vfx-mcp/releases/latest)**。便携版更新前请先关闭 Goose，再用新版本应用文件夹替换旧版本；MCP 配置和本地模型数据存放在应用目录之外。

需要从源码运行时，请克隆本仓库：

```powershell
git clone https://github.com/HikerM/goose-vfx-mcp.git
cd goose-vfx-mcp
. .\bin\activate-goose-rust.ps1
& .\bin\cargo.cmd build
```

# 快速链接
- [版本下载](https://github.com/HikerM/goose-vfx-mcp/releases)
- [版本标签](https://github.com/HikerM/goose-vfx-mcp/tags)
- [问题反馈](https://github.com/HikerM/goose-vfx-mcp/issues)
- [桌面端中文使用说明](./Goose-%E6%A1%8C%E9%9D%A2%E7%AB%AF%E4%BD%BF%E7%94%A8%E8%AF%B4%E6%98%8E.txt)
- [上游 Quickstart 文档](https://goose-docs.ai/docs/quickstart)
- [上游安装文档](https://goose-docs.ai/docs/getting-started/installation)
- [Tutorials](https://goose-docs.ai/docs/category/tutorials)
- [Documentation](https://goose-docs.ai/docs/category/getting-started)
- [本仓库自定义发行说明](./CUSTOM_DISTROS.md)

## Need help?
- [Diagnostics & Reporting](https://goose-docs.ai/docs/troubleshooting/diagnostics-and-reporting)
- [Known Issues](https://goose-docs.ai/docs/troubleshooting/known-issues)

# a little goose humor 🪿

> Why did the developer choose goose as their AI agent?
> 
> Because it always helps them "migrate" their code to production! 🚀

# goose around with us
- [Discord](https://discord.gg/goose-oss)
- [YouTube](https://www.youtube.com/@goose-oss)
- [LinkedIn](https://www.linkedin.com/company/goose-oss)
- [Twitter/X](https://x.com/goose_oss)
