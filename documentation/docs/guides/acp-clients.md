---
sidebar_position: 105
title: Using lumina in ACP Clients
sidebar_label: lumina in ACP Clients
---

Client applications that support the [Agent Client Protocol (ACP)](https://agentclientprotocol.com/) can connect natively to lumina. This integration allows you to seamlessly interact with lumina directly from the client.

:::warning Experimental Feature
ACP is an emerging specification that enables clients to communicate with AI agents like lumina. This feature has limited adoption and may evolve as the protocol develops.
:::

## How It Works
After you configure lumina as an agent in the ACP client, you gain access to lumina's core agent functionality, including its extensions and tools. lumina also automatically loads any [configured MCP servers](#using-mcp-servers-from-acp-clients) from your ACP client alongside its own extensions, making their tools available without additional configuration.

The client manages the lumina lifecycle automatically, including:

- **Initialization**: The client runs the `lumina acp` command to initialize the connection
- **Communication**: The client communicates with lumina over stdio using JSON-RPC
- **Multiple Sessions**: The client manages multiple concurrent conversations, each with isolated state
- **Model and Mode Switching**: The client can switch models and modes mid-session without restarting
- **File Operations**: The client handles file reads and writes, so lumina sees changes not yet saved to disk and edits show as native diffs
- **Terminal**: The client runs commands in its own terminal, so output appears alongside the conversation

:::info Session Persistence
ACP sessions are saved to lumina's session history where you can access and manage them using lumina. Access to session history in ACP clients might vary.
:::

:::tip Reference Implementation
The [lumina for VS Code](/docs/experimental/vs-code-extension) extension uses ACP to communicate with lumina. See the [vscode-lumina](https://github.com/HikerM/vscode-lumina) repository for implementation details.
:::

## Setup in ACP Clients
Any editor or IDE that supports ACP can connect to lumina as an agent server. Check the [official ACP clients list](https://agentclientprotocol.com/overview/clients) for available clients with links to their documentation.

### Example: Zed Editor Setup

ACP was originally developed by [Zed](https://zed.dev/). Zed offers two ways to add lumina, and you can use either one.

#### Option 1: Install from the ACP Registry (recommended)

lumina is published in the [ACP Registry](https://agentclientprotocol.com/registry), and Zed 1.5.0 and later has built-in registry support, so it can download and run lumina for you, with no manual configuration and no pre-installed CLI required.

1. Open Zed
2. Open Agent Settings
3. Click `Add Agent`, then choose `Install from Registry`
4. Select `lumina`

A registry-installed lumina runs the same `lumina acp` server and reads your existing lumina configuration, so your providers, models, and extensions carry over. Zed keeps the installed version up to date for you.

#### Option 2: Configure lumina as a Custom Agent

Use a custom agent if you want to run your own lumina binary (for example, a local development build) or pass environment overrides.

##### Prerequisites

Ensure you have both Zed and lumina CLI installed:

- **Zed**: Download from [zed.dev](https://zed.dev/)
- **lumina CLI**: Follow the [installation guide](/docs/getting-started/installation)

  - Verify lumina is installed: `lumina --version`

  - Temporarily run `lumina acp` to test that ACP support is working:

    ```bash
    lumina acp
    ```

    Press `Ctrl+C` to exit the test.

##### Add lumina to Your Zed Settings

1. Open Zed
2. Open Agent Settings, click `Add Agent`, then choose `Add Custom Agent`. Zed scaffolds an `agent_servers` entry and opens your settings file
3. Edit the entry so it runs lumina:

```json
{
  "agent_servers": {
    "lumina": {
      "type": "custom",
      "command": "lumina",
      "args": ["acp"]
    }
  },
}
```

You should now be able to interact with lumina directly in Zed. Your ACP sessions use the same extensions that are enabled in your lumina configuration, and your tools (Developer, Computer Controller, etc.) work the same way as in regular lumina sessions.

#### Start Using lumina in Zed

After adding lumina with either option above:

1. **Open the Agent Panel**: Click the sparkles agent icon in Zed's status bar
2. **Create New Thread**: Click the `+` button to show thread options
3. **Select lumina**: Choose `New lumina` to start a new conversation with lumina
4. **Start Chatting**: Interact with lumina directly from the agent panel

#### Advanced Configuration

##### Overriding Provider and Model

By default, lumina will use the provider and model defined in your [configuration file](/docs/guides/config-files). You can override this for specific ACP configurations using the `LUMINA_PROVIDER` and `LUMINA_MODEL` environment variables.

The following Zed settings example configures two lumina agent instances. This is useful for:
- Comparing model performance on the same task
- Using cost-effective models for simple tasks and powerful models for complex ones

```json
{
  "agent_servers": {
    "lumina": {
      "type": "custom",
      "command": "lumina",
      "args": ["acp"]
    },
    "lumina (GPT-4o)": {
      "type": "custom",
      "command": "lumina",
      "args": ["acp"],
      "env": {
        "LUMINA_PROVIDER": "openai",
        "LUMINA_MODEL": "gpt-4o"
      }
    }
  },
}
```

## Using MCP Servers from ACP Clients

MCP servers configured in the ACP client's `context_servers` are automatically available to lumina. This allows you to use those MCP servers when using both native client features and the lumina agent integration.

**Example (Zed):**

```json
{
  "context_servers": {
    "filesystem": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "/path/to/allowed/dir"
      ]
    }
  },
  "agent_servers": {
    "lumina": {
      "type": "custom",
      "command": "lumina",
      "args": ["acp"]
    }
  },
}
```

To find out what tools are available, just ask lumina while it's running in the client.

:::info
All MCP servers in `context_servers` are automatically available to lumina, provided that they use stdio (command-based) or HTTP transports. lumina doesn't support servers that use the deprecated SSE transport.

If a server in `context_servers` has the same name as a lumina extension, lumina uses its own [configuration](/docs/guides/config-files).
:::

## TUI Client

For terminal-based workflows, lumina provides a TUI (Terminal User Interface) client that communicates with lumina via ACP. This is useful for developers who prefer working entirely in the terminal or need a lightweight alternative to the desktop app.

### Features

- **Full terminal-based chat interface** - Interactive conversation UI rendered directly in your terminal
- **Real-time streaming responses** - See lumina's responses as they're generated
- **Tool call visualization** - View tool executions with status indicators, inputs, and outputs
- **Permission dialogs** - Approve or reject tool permissions inline
- **Keyboard navigation** - Navigate conversation history and scroll through responses
- **Markdown rendering** - Formatted output for code blocks, lists, and other markdown elements
- **Message queuing** - Queue messages while lumina is processing

### Installation

```bash
cd ui/text
npm install
```

### Running the TUI

**Option 1: Auto-launch server (recommended)**

The TUI will automatically start the lumina acp server if you have it installed:

```bash
npm start
```

**Option 2: Connect to a custom server**

For servers that support the draft standard ACP over Streamable HTTP https://github.com/agentclientprotocol/agent-client-protocol/pull/721

```bash
npm start -- --server http://HOST:PORT

# example server
LUMINA_SERVER__SECRET_KEY='a-long-random-secret' cargo run -p lumina-cli --bin lumina -- serve
```

### Server Authentication

Set the `LUMINA_SERVER__SECRET_KEY` environment variable to authenticate the ACP endpoint. `lumina serve` refuses to start without this secret unless you explicitly pass `--dangerously-unauthenticated`:

```bash
LUMINA_SERVER__SECRET_KEY='a-long-random-secret' lumina serve
```

Clients authenticate by sending the token in the `X-Secret-Key` header, or as a `?token=` query parameter for WebSocket connections (the browser WebSocket API can't set custom headers). Requests without a matching token receive `401 Unauthorized`, including WebSocket handshakes.

ACP WebSocket Origin validation allows loopback web origins by default. For `lumina serve`, ACP CORS follows the same policy. If you pass any `--allowed-origin` values, that explicit list replaces the default loopback origins, so include every origin the client needs:

```bash
LUMINA_SERVER__SECRET_KEY='a-long-random-secret' lumina serve \
  --allowed-origin 'http://localhost:5173' \
  --allowed-origin 'app://localhost' \
  --allowed-origin 'https://app.example'
```

For local development only, `lumina serve --dangerously-unauthenticated` starts without a secret and logs a warning. Do not use this mode with shell-capable builtins enabled unless the server is isolated from untrusted browser traffic.

### Single Prompt Mode

Send a single prompt and exit (useful for scripting):

```bash
npm start -- --text "What files are in this directory?"
```

### Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Enter` | Send message |
| `↑` / `↓` | Scroll current response |
| `Shift+↑` / `Shift+↓` | Navigate conversation history |
| `Tab` | Expand/collapse tool call details |
| `Ctrl+C` or `Esc` | Exit (or cancel permission dialog) |

### Permission Dialog

When lumina requests permission to use a tool, a dialog appears with these options:

| Key | Action |
|-----|--------|
| `y` | Allow once |
| `a` | Always allow |
| `n` | Reject once |
| `N` | Always reject |
| `↑` / `↓` | Navigate options |
| `Enter` | Confirm selection |
| `Esc` | Cancel |

## Additional Resources

import ContentCardCarousel from '@site/src/components/ContentCardCarousel';
import chooseYourIde from '@site/blog/2025-10-24-intro-to-agent-client-protocol-acp/choose-your-ide.png';

<ContentCardCarousel
  items={[
    {
      type: 'video',
      title: 'Intro to Agent Client Protocol (ACP) | Vibe Code with lumina',
      description: 'Watch how ACP lets you seamlessly integrate lumina into your code editor to streamline fragmented workflows.',
      thumbnailUrl: 'https://img.youtube.com/vi/Hvu5KDTb6JE/maxresdefault.jpg',
      linkUrl: 'https://www.youtube.com/watch?v=Hvu5KDTb6JE',
      date: '2025-10-16',
      duration: '50:23'
    },
   {
      type: 'blog',
      title: 'Intro to Agent Client Protocol (ACP): The Standard for AI Agent-Editor Integration',
      description: 'Learn how to integrate AI agents like lumina directly into your code editor via ACP, eliminating window-switching and vendor lock-in.',
      thumbnailUrl: chooseYourIde,
      linkUrl: '/blog/2025/10/24/intro-to-agent-client-protocol-acp',
      date: '2025-10-24',
      duration: '7 min read'
    }
  ]}
/>
