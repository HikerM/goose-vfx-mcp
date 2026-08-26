import type { ReactNode } from "react";
import Link from "@docusaurus/Link";
import useDocusaurusContext from "@docusaurus/useDocusaurusContext";
import Layout from "@theme/Layout";

import styles from "./index.module.css";
import { LuminaLogo } from "../components/LuminaLogo";

function HeroSection() {
  return (
    <header className={styles.hero}>
      <div className={styles.heroInner}>
        <div className={styles.heroBadge}>
          Open Source · Apache 2.0 · Rust
        </div>
        <div className={styles.heroLogo}>
          <LuminaLogo />
        </div>
        <p className={styles.heroSubtitle}>
          Your native open source AI agent. Desktop app, CLI, and API — for code,
          workflows, and everything in between.
        </p>
        <div className={styles.heroActions}>
          <Link
            className="button button--primary button--lg"
            to="docs/getting-started/installation"
          >
            Install Lumina
          </Link>
          <Link
            className={`button button--outline button--lg ${styles.secondaryButton}`}
            to="docs/quickstart"
          >
            Quickstart
          </Link>
        </div>
      </div>
    </header>
  );
}

type FeatureCardProps = {
  title: string;
  description: ReactNode;
  icon: string;
};

function FeatureCard({ title, description, icon }: FeatureCardProps) {
  return (
    <div className={styles.featureCard}>
      <div className={styles.featureIcon}>{icon}</div>
      <h3 className={styles.featureTitle}>{title}</h3>
      <div className={styles.featureDescription}>{description}</div>
    </div>
  );
}

type SmallCardProps = {
  title: string;
  description: ReactNode;
  icon: string;
};

function SmallCard({ title, description, icon }: SmallCardProps) {
  return (
    <div className={styles.smallCard}>
      <div className={styles.smallCardIcon}>{icon}</div>
      <h3 className={styles.smallCardTitle}>{title}</h3>
      <div className={styles.smallCardDescription}>{description}</div>
    </div>
  );
}

function FeaturesSection() {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.sectionTitle}>What Lumina does</h2>
        <p className={styles.sectionSubtitle}>
          Lumina is a general-purpose AI agent that runs on your machine. Not
          just for code — use it for research, writing, automation, data
          analysis, or anything you need to get done.
        </p>
        <div className={styles.featuresGridTop}>
          <FeatureCard
            icon="🖥️"
            title="Desktop app, CLI, and API"
            description={
              <p>
                A native desktop app for macOS, Linux, and Windows. A full CLI
                for terminal workflows. An API to embed it anywhere. Built
                in Rust for performance and portability.
              </p>
            }
          />
          <FeatureCard
            icon="🔌"
            title="Extensible"
            description={
              <p>
                Connect to extensions for databases, APIs, browsers, GitHub,
                Google Drive, and more — via the{" "}
                <a href="https://modelcontextprotocol.io/" target="_blank" rel="noopener">
                  Model Context Protocol
                </a>{" "}
                open standard. Add community{" "}
                <Link to="/skills">skills</Link>, or{" "}
                <Link to="/docs/tutorials/custom-extensions">build your own</Link>.
              </p>
            }
          />
          <FeatureCard
            icon="🤖"
            title="Any LLM, including your subscriptions"
            description={
              <p>
                Works with providers including Anthropic, OpenAI, Google, Ollama,
                OpenRouter, Azure, Bedrock, and more. Use API keys or your
                existing Claude, ChatGPT, or Gemini subscriptions via{" "}
                <Link to="/docs/guides/acp-providers">ACP</Link>.
              </p>
            }
          />
        </div>
        <div className={styles.featuresGridBottom}>
          <SmallCard
            icon="📋"
            title="Recipes"
            description={
              <p>
                Capture workflows as portable YAML configs. Share with your
                team, run in CI, include instructions, extensions, parameters,
                and{" "}
                <Link to="/docs/guides/recipes/session-recipes">subrecipes</Link>.
              </p>
            }
          />
          <SmallCard
            icon="🧩"
            title="MCP Apps"
            description={
              <p>
                Extensions can render interactive UIs directly inside Lumina
                Desktop — buttons, forms, visualizations. A new way to build{" "}
                <Link to="/docs/tutorials/building-mcp-apps">
                  agent-powered tools
                </Link>.
              </p>
            }
          />
          <SmallCard
            icon="🔀"
            title="Subagents"
            description={
              <p>
                Spawn independent{" "}
                <Link to="/docs/guides/context-engineering/subagents">subagents</Link> to handle
                tasks in parallel — code review, research, file processing —
                keeping the main conversation clean.
              </p>
            }
          />
          <SmallCard
            icon="🔒"
            title="Security"
            description={
              <p>
                Prompt injection detection, tool permission controls, sandbox
                mode, and an{" "}
                <Link to="/docs/guides/security/adversary-mode">
                  adversary reviewer
                </Link>{" "}
                that watches for unsafe actions.
              </p>
            }
          />
        </div>
      </div>
    </section>
  );
}

function StandardsSection() {
  return (
    <section className={`${styles.section} ${styles.sectionAlt}`}>
      <div className={styles.container}>
        <h2 className={styles.sectionTitle}>Built on open standards</h2>
        <div className={styles.standardsGrid}>
          <div className={styles.standardCard}>
            <h3>Model Context Protocol</h3>
            <p>
              <a href="https://modelcontextprotocol.io/" target="_blank" rel="noopener">MCP</a>{" "}
              is the open standard for connecting AI agents to tools and data
              sources. Lumina uses MCP for tool, data, and interactive app
              integrations without tying the runtime to a single vendor.
            </p>
            <Link to="/docs/category/mcp-servers">Browse MCP extensions →</Link>
          </div>
          <div className={styles.standardCard}>
            <h3>Agent Client Protocol</h3>
            <p>
              <a href="https://agentclientprotocol.com/" target="_blank" rel="noopener">ACP</a>{" "}
              is a standard for communicating with coding agents. Lumina works as
              an ACP server — connect from Zed, JetBrains, or VS Code — and can
              use ACP agents like Claude Code and Codex as providers.
            </p>
            <Link to="/docs/guides/acp-clients">lumina as ACP server →</Link>
          </div>
          <div className={styles.standardCard}>
            <h3>Independent Lumina protocols</h3>
            <p>
              Lumina has its own application, storage, package, update, and
              extension namespaces. Legacy imports are handled by an isolated
              migration command rather than by hidden runtime fallbacks.
            </p>
            <Link to="/docs/quickstart">Read the Lumina quickstart →</Link>
          </div>
        </div>
      </div>
    </section>
  );
}

function ResourcesSection() {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.sectionTitle}>Project resources</h2>
        <p className={styles.sectionSubtitle}>
          Documentation and extension surfaces designed for maintainable,
          independently versioned Lumina releases.
        </p>
        <div className={styles.communityGrid}>
          <a
            href="https://github.com/HikerM/lumina"
            target="_blank"
            rel="noopener"
            className={styles.communityCard}
          >
            <h3>🐙 GitHub</h3>
            <p>
              Review source, file issues, and contribute code. Lumina is built in the
              open.
            </p>
          </a>
          <Link to="/extensions" className={styles.communityCard}>
            <h3>🧩 Extensions</h3>
            <p>Browse community-built MCP extensions and add your own.</p>
          </Link>
          <Link to="/docs/category/guides" className={styles.communityCard}>
            <h3>📘 Guides</h3>
            <p>Configuration, security, provider, and workflow documentation.</p>
          </Link>
        </div>
      </div>
    </section>
  );
}

function InstallSection() {
  return (
    <section className={`${styles.section} ${styles.sectionAlt}`}>
      <div className={styles.container}>
        <h2 className={styles.sectionTitle}>Get started</h2>
        <div className={styles.installBlock}>
          <div className={styles.installDesktop}>
            <Link
              className="button button--primary button--lg"
              to="docs/getting-started/installation"
            >
              Read installation instructions
            </Link>
            <p className={styles.installPlatforms}>
              Available for macOS, Linux, and Windows
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}

export default function Home(): ReactNode {
  return (
    <Layout description="Your native open source AI agent. Desktop app, CLI, and API — for code, workflows, and everything in between.">
      <HeroSection />
      <main>
        <FeaturesSection />
        <StandardsSection />
        <ResourcesSection />
        <InstallSection />
      </main>
    </Layout>
  );
}
