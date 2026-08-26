import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const trackedFiles = execFileSync(
  "git",
  ["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
  {
    encoding: "utf8",
  },
)
  .split("\0")
  .filter(Boolean)
  .map((path) => path.replaceAll("\\", "/"));

const runtimeFiles = trackedFiles.filter((path) => {
  if (path.startsWith("crates/lumina-legacy-migrator/")) return false;
  if (path.startsWith("crates/lumina/tests/mcp_replays/")) return false;

  return (
    path.startsWith("crates/") ||
    path.startsWith("third_party/pctx_config/src/") ||
    path.startsWith("ui/desktop/src/") ||
    path.startsWith("ui/text/src/") ||
    path.startsWith("ui/sdk/src/") ||
    path.startsWith("bin/") ||
    path === "Cargo.toml" ||
    path === "Cargo.lock" ||
    path === "third_party/pctx_config/Cargo.toml" ||
    path === "ui/desktop/forge.config.ts" ||
    path === "ui/desktop/vite.main.config.mts" ||
    path === "ui/desktop/vite.renderer.config.mts"
  );
});

const forbidden = [
  { label: "legacy product identity", pattern: /goose/i },
  { label: "unapproved remote UI asset", pattern: /cash-f\.squarecdn\.com/i },
  {
    label: "telemetry or monitoring SDK",
    pattern: /\b(?:posthog|langfuse|opentelemetry|sentry)\b/i,
  },
  { label: "fabricated release repository", pattern: /lumina-vfx-mcp/i },
  {
    label: "unowned AAIF package identity",
    pattern: /@aaif\/lumina|aaif-lumina/i,
  },
  { label: "fabricated V8 package identity", pattern: /v8-lumina/i },
  { label: "invalid documentation host", pattern: /lumina-docs\.ai/i },
  {
    label: "upstream organization used as Lumina runtime identity",
    pattern:
      /dev\.block\.lumina|Agentic AI Foundation|ai-oss-tools@block\.xyz/i,
  },
];

const failures = [];

for (const path of runtimeFiles) {
  if (/goose/i.test(path)) {
    failures.push(`${path}: path contains a legacy product identity`);
  }

  let content;
  try {
    content = readFileSync(path, "utf8");
  } catch {
    continue;
  }

  if (content.includes("\0")) continue;

  const lines = content.split(/\r?\n/);
  for (const [index, line] of lines.entries()) {
    for (const rule of forbidden) {
      if (rule.pattern.test(line)) {
        failures.push(`${path}:${index + 1}: ${rule.label}`);
      }
    }
  }
}

if (failures.length > 0) {
  console.error("Lumina runtime independence check failed:");
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `Lumina runtime independence check passed (${runtimeFiles.length} source files).`,
);
