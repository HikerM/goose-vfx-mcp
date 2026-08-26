---
title: Quick lumina Tips
sidebar_position: 30
sidebar_label: Quick Tips
description: Best practices for working with lumina
---

### lumina works on your behalf
lumina is an AI agent, which means you can prompt lumina to perform tasks for you like opening applications, running shell commands, automating workflows, writing code, browsing the web, and more.

### Prompt lumina using natural language
You don't need fancy language or special syntax to prompt lumina. Talk with lumina like you would talk to a friend. You can even use slang or say please and thank you; lumina will understand.

### Extend lumina's capabilities to any application
lumina's capabilities are extensible. As an [MCP](https://modelcontextprotocol.io/) client, lumina can connect to your apps and services through [extensions](/extensions), allowing it to work across your entire workflow.

### Choose how much control lumina has
You can customize how much [supervision](/docs/guides/managing-tools/lumina-permissions) lumina needs. Choose between full autonomy, requiring approval before actions, or simply chatting without any actions.

### Choose the right LLM
Your experience with lumina is shaped by your [choice of LLM](/blog/2025/03/31/lumina-benchmark), as it handles all the planning while lumina manages the execution. When choosing an LLM, consider its tool support, specific capabilities, and associated costs.

### Keep sessions short
LLMs have context windows, which are limits on how much conversation history they can retain. Once exceeded, they may forget earlier parts of the conversation. Monitor your token usage and [start new sessions](/docs/guides/sessions/session-management) as needed.

### Use Quick Launcher for faster session starts
Press `Cmd+Option+Shift+G` (macOS) or `Ctrl+Alt+Shift+G` (Windows/Linux) and send a prompt to start a new session instantly.

### Turn off unnecessary extensions or tool
Turning on too many extensions can degrade performance. Enable only essential [extensions and tools](/docs/guides/managing-tools/tool-permissions) to improve tool selection accuracy, save context window space, and stay within provider tool limits.

:::tip Code Mode for Many Extensions
Consider enabling [Code Mode](/docs/guides/managing-tools/code-mode), an alternative approach to tool calling that discovers tools on demand.
:::

### Teach lumina your preferences
Help lumina remember how you like to work by using [`.luminahints` or other context files](/docs/guides/context-engineering/using-luminahints) or [skills](/docs/guides/context-engineering/using-skills) for permanent project preferences and the [Memory extension](/docs/mcp/memory-mcp) for things you want lumina to dynamically recall later. Both can help save valuable context window space while keeping your preferences available.

### Protect sensitive files
lumina is often eager to make changes. You can stop it from changing specific files by creating a [.luminaignore](/docs/guides/context-engineering/using-luminaignore) file. In this file, you can list all the file paths you want it to avoid.

### Version Control
Commit your code changes early and often. This allows you to rollback any unexpected changes.

### Control which extensions lumina can use
Administrators can use an [allowlist](/docs/guides/allowlist) to restrict lumina to approved extensions only. This helps prevent risky installs from unknown MCP servers.

### Set up starter templates
You can turn a successful session into a reusable "[recipe](/docs/guides/recipes/session-recipes)" to share with others or use again later—no need to start from scratch.

### Embrace an experimental mindset
You don’t need to get it right the first time. Iterating on prompts and tools is part of the workflow.

### Customize the sidebar
lumina Desktop lets you [customize the sidebar](/docs/guides/desktop-navigation) to match how you like to work. Adjust its position, appearance, and which items are visible.

### Keep lumina updated
Regularly [update](/docs/guides/updating-lumina) lumina to benefit from the latest features, bug fixes, and performance improvements.

### Use a Dedicated Planner Model
Use [planning mode](/docs/guides/context-engineering/creating-plans) with a dedicated planner model for complex reasoning, while keeping a faster default model for everyday execution.

### Make Recipes Safe to Re-run
Write [recipes](/docs/guides/recipes/session-recipes) that check your current state before acting, so they can be run multiple times without causing any errors or duplication. 

### Add Logging to Recipes
Include informative log messages in your recipes for each major step to make debugging and troubleshooting easier should something fail.
