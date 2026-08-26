---
sidebar_position: 56
title: MCP Roots
sidebar_label: MCP Roots
description: How lumina shares your working directory with roots-aware MCP extensions
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import { PanelLeft } from 'lucide-react';

MCP Roots lets lumina share your session working directory with roots-aware MCP extensions.

This helps extensions understand which folder to use as the active workspace for the current session.

:::info
[MCP Roots](https://modelcontextprotocol.io/specification/2025-06-18/client/roots) is a feature in the Model Context Protocol. lumina automatically enables it for MCP extensions that support roots.
:::

## How MCP Roots Works

When lumina connects to an MCP extension, it advertises roots support during MCP initialization.

An extension that supports roots can then:

- Request the current root list from lumina
- Treat that root as the active workspace boundary
- React if the root changes during the session

In lumina, the root list currently contains one entry:

- Your current session working directory

If you change the session working directory, lumina updates the root and notifies connected extensions automatically.

## Using MCP Roots

There is no separate "Roots" settings screen in lumina. MCP Roots follows the working directory you are already using for the session.

<Tabs groupId="interface">
  <TabItem value="ui" label="lumina Desktop" default>

  In lumina Desktop, the current working directory appears at the bottom of the chat window.

  1. Click the directory display to choose a different folder
  2. lumina updates the session working directory
  3. Connected roots-aware extensions receive the updated root automatically

  To change directories:

  1. Click the <PanelLeft className="inline" size={16} /> button if needed and open a session
  2. Use the directory control shown at the bottom of the chat window
  3. Pick the folder you want lumina and your extensions to work in

  </TabItem>
  <TabItem value="cli" label="lumina CLI">

  In lumina CLI, the session root follows the directory where you launch lumina.

  - Start lumina from the project folder you want to work in
  - When resuming a session, lumina may prompt you to switch back to that session's original working directory

  This means roots-aware extensions see the same workspace directory that lumina is already using in the CLI.

  </TabItem>
</Tabs>

## What Extensions Use It For

MCP Roots is useful for extensions that need to work with local files or understand your project structure.

Without roots, an extension may need to guess which folder you mean or rely on custom configuration. With roots, lumina can tell the extension which directory is currently in scope for the session.

For example, an extension can use roots to:

- Discover the active project directory
- Scope file operations to your current workspace
- Avoid guessing which repository or folder you meant
- Update its behavior when you switch to a different project

## Current Limitations

lumina currently exposes a single root per session rather than a multi-folder workspace. For most workflows, this maps cleanly to how lumina already works: one active project directory at a time.

If you want an extension to work in a different location, change the session working directory first.

## For Extension Developers

If you're building an MCP extension for lumina, support for roots allows your extension to discover the active workspace directory in a standard way instead of relying on custom configuration.

For protocol details, see the [MCP Roots specification](https://modelcontextprotocol.io/specification/2025-06-18/client/roots).
