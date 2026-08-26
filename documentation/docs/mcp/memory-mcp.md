---
title: Memory Extension
description: Use Memory MCP Server as a lumina Extension
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import YouTubeShortEmbed from '@site/src/components/YouTubeShortEmbed';
import LuminaBuiltinInstaller from '@site/src/components/LuminaBuiltinInstaller';

<YouTubeShortEmbed videoUrl="https://youtube.com/embed/BZ0yrSLXQwk" />

The Memory extension turns lumina into a knowledgeable assistant by allowing you to teach it personalized key information (e.g. commands, code snippets, preferences and configurations) that it can recall and apply later. Whether it’s project-specific (local) or universal (global) knowledge, lumina learns and remembers what matters most to you.

This tutorial covers enabling and using the Memory MCP Server, which is a built-in lumina extension.

## Configuration

<Tabs groupId="interface">
  <TabItem value="ui" label="lumina Desktop" default>
  <LuminaBuiltinInstaller
    extensionName="Memory"
    description="Store and recall personalized information for consistent assistance"
  />
  </TabItem>
  <TabItem value="cli" label="lumina CLI">


  1. Run the `configure` command:
  ```sh
  lumina configure
  ```

  2. Choose to `Toggle Extensions`
  ```sh
  ┌   lumina-configure
  │
  ◇  What would you like to configure?
  │  Toggle Extensions
  │
  ◆  Enable extensions: (use "space" to toggle and "enter" to submit)
  // highlight-start
  │  ● memory
  // highlight-end
  |
  └  Extension settings updated successfully
  ```
  </TabItem>
</Tabs>

## Storage Locations

Memories are stored as files on disk in one of two locations:

| Scope | Path | When to use |
|-------|------|-------------|
| Local (project) | `.lumina/memory/` in your working directory | Project-specific preferences and configs |
| Global (user) | `~/.config/lumina/memory/` | Preferences that apply across all projects |

lumina loads all saved memories at the start of a session and includes them in every prompt sent to the LLM.

## Tool Reference

| Tool | What it does |
|------|-------------|
| `remember_memory(category, data, tags, is_global)` | Store information with a category, optional tags, and scope (local/global) |
| `retrieve_memories(category, is_global)` | Retrieve memories by category. Use `"*"` to retrieve all. |
| `remove_memory_category(category, is_global)` | Remove all memories in a category. Use `"*"` to clear all. |
| `remove_specific_memory(category, memory_content, is_global)` | Remove a single memory by matching its content within a category |

## Why Use Memory?
With the Memory extension, you’re not just storing static notes, you’re teaching lumina how to assist you better. Imagine telling lumina:

> _learn everything about MCP servers and save it to memory._

Later, you can ask:
> _utilizing our MCP server knowledge help me build an MCP server._

lumina will recall everything you’ve saved as long as you instruct it to remember. This makes it easier to have consistent results when working with lumina.

For large or detailed instructions, store them in files and instruct lumina to reference those files:

> _Remember that if I ask for help writing JavaScript, I want you to refer to "/path/to/javascript_notes.txt" and follow the instructions in that file._


## Trigger Words and When to Use Them
lumina also recognizes certain trigger words that signal when to store, retrieve, or remove memory.

| **Trigger Words**   | **When to Use** |
|---------------------|----------------|
| remember            | Store useful info for later use |
| forget           | Remove a stored memory |
| memory           | General memory-related actions |
| save             | Save a command, config, or preference |
| remove memory    | Delete specific stored data |
| clear memory     | Wipe all stored memories |
| search memory    | Find previously stored data |
| find memory      | Locate specific saved information |

## Example Usage

In this example, I’ll show you how to make lumina a knowledgeable development assistant by teaching it about your project’s API standards. With the Memory extension, lumina can store structured information and recall it when needed, helping you stay consistent in your work.

This means you no longer have to repeat yourself. lumina will remember your project’s requirements and automatically apply them to new tasks.

:::tip
If you frequently work with API standards or other structured knowledge, lumina may automatically suggest saving them to memory for you when it detects a pattern in your workflow.
:::

<Tabs groupId="interface">
  <TabItem value="ui" label="lumina Desktop" default>
   1. Open a new session in lumina Desktop
  </TabItem>
  <TabItem value="cli" label="lumina CLI">

  1. Open a terminal and start a new lumina session:

  ```sh
  lumina session
  ```

  </TabItem>
</Tabs>

### Step 1: Teach lumina Your API Standards
    #### lumina Prompt #1
    ```
    lumina, learn our API development standards:

    - All endpoints must use TypeScript interfaces
    - Error responses follow RFC 7807 Problem Details spec
    - Rate limiting headers are required
    - Authentication uses JWT in Authorization header
    - All endpoints need OpenAPI documentation

    Remember this so you can help me follow these practices.
    ```

    #### lumina Output

    ```
    I've learned your API standards. I'll help enforce these requirements when working on API endpoints.

    Storing in:
        - Category: development_standards
        - Tags: #api #typescript #security #documentation
        - Scope: global (these are general standards)

    I can now help with:
        - API endpoint implementation
        - Error handling patterns
        - Authentication flows
        - Documentation requirements
    ```
### Step 2: Use Stored Knowledge to Create a New API Endpoint
    #### lumina Prompt # 2

    ```
    Help me create a new endpoint for user preferences
    ```
    #### lumina Output
    ```
    I see you need a new endpoint. Based on our API development standards that I have stored in memory, I'll ensure it meets all our requirements.

    [.. Checking stored API standards ...]

    I've included all our required elements:

    ✅ TypeScript interfaces
    ✅ RFC 7807 error response format
    ✅ Rate limiting
    ✅ JWT auth header
    ✅ OpenAPI documentation

    Here's your compliant endpoint:

    interface UserPreferences {
        userId: string;
        theme: 'light' | 'dark';
        notifications: boolean;
        timezone: string;
    }

    interface ErrorResponse {
        type: string;
        title: string;
        status: number;
        detail: string;
        instance: string;
    }

    @Route('/api/users/:userId/preferences')
    class UserPreferencesController {
        @Get()
        @RateLimit(100)  // Adding required rate limiting
        @OpenAPI({ ... }) // Including mandatory OpenAPI docs
        async getUserPreferences(
            @Header('Authorization') token: string,  // JWT auth as per our standard
            @Param('userId') userId: string
        ): Promise<UserPreferences | ErrorResponse> {
            // Implementation following our error handling pattern
        }
    }

    Would you like me to implement the full endpoint logic?
    ```
