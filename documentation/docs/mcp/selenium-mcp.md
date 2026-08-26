---
title: Selenium Extension
description: Add Selenium MCP Server as a lumina Extension
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import YouTubeShortEmbed from '@site/src/components/YouTubeShortEmbed';
import LuminaDesktopInstaller from '@site/src/components/LuminaDesktopInstaller';
import CLIExtensionInstructions from '@site/src/components/CLIExtensionInstructions';

<YouTubeShortEmbed videoUrl="https://www.youtube.com/embed/PLqPOEeGPLc" />


This tutorial covers how to add the [Selenium MCP Server](https://github.com/angiejones/mcp-selenium) as a lumina extension to automate browser interactions such as navigating web pages and completing forms.

:::tip Quick Install
<Tabs groupId="interface">
  <TabItem value="ui" label="lumina Desktop" default>
  [Launch the installer](lumina://extension?cmd=npx&arg=-y&arg=%40angiejones%2Fmcp-selenium&id=selenium-mcp&name=Selenium%20MCP&description=automates%20browser%20interactions)
  </TabItem>
  <TabItem value="cli" label="lumina CLI">
  **Command**
  ```sh
  npx -y @angiejones/mcp-selenium
  ```
  </TabItem>
</Tabs>
:::

## Configuration

:::info
Note that you'll need [Node.js](https://nodejs.org/) installed on your system to run this command, as it uses `npx`.
:::


<Tabs groupId="interface">
  <TabItem value="ui" label="lumina Desktop" default>
  <LuminaDesktopInstaller
    extensionId="selenium-mcp"
    extensionName="Selenium MCP"
    description="automates browser interactions"
    command="npx"
    args={["-y", "@angiejones/mcp-selenium"]}
  />
</TabItem>
  <TabItem value="cli" label="lumina CLI">
    <CLIExtensionInstructions
      name="Selenium"
      description="Automates browser interactions"
      command="npx -y @angiejones/mcp-selenium"
    />
  </TabItem>
</Tabs>

## Example Usage

Let's use lumina to build a test automation project from scratch! We'll use the Selenium MCP to automate filling out a web form, then have lumina generate a Selenium project with the code so that we can run these tests again when needed.


### lumina Prompt

> Use selenium to go to the heroku formy site and fill out the form page with generic data. then can you turn what you've done into an automation script for me? I would like it in Java. Also use the Page Object Model pattern.


### lumina Output

<iframe class="aspect-ratio" src="https://www.youtube.com/embed/mRV0N8hcgYA?start=28&end=152" title="YouTube video player" frameborder="0" allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share" referrerpolicy="strict-origin-when-cross-origin" allowfullscreen></iframe>