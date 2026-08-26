#!/usr/bin/env bash
set -euo pipefail

# Builds and publishes all packages in the configured Lumina npm scope:
#   @hikerm/lumina-sdk            — ACP TypeScript SDK
#   @hikerm/lumina-binary-*       — platform-specific lumina CLI binaries
#   @hikerm/lumina                — TUI that depends on the above
#
# Linux binaries are built inside Docker containers on their native arch.
# macOS binaries are built natively (requires macOS host with Rust).
# Windows is skipped (no cross-compilation support yet).
#
# Usage:
#   ./ui/scripts/publish.sh          # dry run
#   ./ui/scripts/publish.sh --real   # publish to npmjs.org
#
# Requires:
#   - Docker with buildx (linux/amd64 + linux/arm64)
#   - Rust toolchain with aarch64-apple-darwin and x86_64-apple-darwin targets
#   - pnpm
#   - NPM_PUBLISH_TOKEN env var (or ~/.npm-publish-token file)

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NATIVE_DIR="${REPO_ROOT}/ui/lumina-binary"
SDK_DIR="${REPO_ROOT}/ui/sdk"
TEXT_DIR="${REPO_ROOT}/ui/text"
REGISTRY="https://registry.npmjs.org"
DOCKER_IMAGE="rust:1.92-bookworm"

DRY_RUN=true
if [[ "${1:-}" == "--real" ]]; then
  DRY_RUN=false
  echo "==> REAL publish to ${REGISTRY}"
else
  echo "==> Dry run (pass --real to publish for real)"
fi

# Resolve NPM token
if [[ -z "${NPM_PUBLISH_TOKEN:-}" ]]; then
  TOKEN_FILE="${HOME}/.npm-publish-token"
  if [[ -f "${TOKEN_FILE}" ]]; then
    NPM_PUBLISH_TOKEN="$(cat "${TOKEN_FILE}")"
    export NPM_PUBLISH_TOKEN
    echo "==> Loaded NPM_PUBLISH_TOKEN from ${TOKEN_FILE}"
  else
    echo "ERROR: NPM_PUBLISH_TOKEN not set and ${TOKEN_FILE} not found" >&2
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Step 1: Build macOS binaries natively
# ---------------------------------------------------------------------------
build_macos() {
  local platform="$1" target="$2"
  local pkg_dir="${NATIVE_DIR}/lumina-binary-${platform}/bin"

  echo "==> Building lumina for ${platform} (${target}) natively"
  cargo build --release --target "${target}" --bin lumina --manifest-path "${REPO_ROOT}/Cargo.toml"

  mkdir -p "${pkg_dir}"
  cp "${REPO_ROOT}/target/${target}/release/lumina" "${pkg_dir}/lumina"
  chmod +x "${pkg_dir}/lumina"
  echo "    ✅ ${pkg_dir}/lumina"
}

build_macos darwin-arm64 aarch64-apple-darwin
build_macos darwin-x64   x86_64-apple-darwin

# ---------------------------------------------------------------------------
# Step 2: Build Linux binaries in Docker on native arch
# ---------------------------------------------------------------------------
DOCKER_BUILD_SCRIPT='#!/bin/bash
set -euo pipefail
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  build-essential cmake pkg-config libssl-dev libdbus-1-dev \
  libclang-dev protobuf-compiler libprotobuf-dev ca-certificates \
  libvulkan-dev libvulkan1 glslc >/dev/null 2>&1
echo "==> Compiling lumina (this takes a while)..."
cargo build --release --bin lumina --features vulkan
cp /build/target/release/lumina /output/lumina
echo "==> Done"
'

build_linux_docker() {
  local platform="$1" docker_platform="$2"
  local pkg_dir="${NATIVE_DIR}/lumina-binary-${platform}/bin"

  echo "==> Building lumina for ${platform} in Docker (${docker_platform})"

  mkdir -p "${pkg_dir}"

  # Create a temporary directory with only what cargo needs.
  # This avoids sending multi-GB Docker contexts.
  local ctx
  ctx="$(mktemp -d)"
  trap "rm -rf '${ctx}'" RETURN 2>/dev/null || true

  # Copy Rust source
  rsync -a --delete \
    --exclude='target/' \
    --exclude='.git/' \
    --exclude='node_modules/' \
    --exclude='documentation/' \
    --exclude='ui/desktop/' \
    --exclude='ui/lumina-binary/*/bin/' \
    --exclude='evals/' \
    --exclude='.hermit/' \
    --exclude='*.jsonl' \
    --exclude='bench_results/' \
    "${REPO_ROOT}/" "${ctx}/"

  # Write a minimal Dockerfile into the context
  cat > "${ctx}/Dockerfile.npm-build" <<'DEOF'
FROM rust:1.92-bookworm
RUN apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
      build-essential cmake pkg-config libssl-dev libdbus-1-dev \
      libclang-dev protobuf-compiler libprotobuf-dev ca-certificates \
      libvulkan-dev libvulkan1 glslc >/dev/null 2>&1 && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN mkdir -p /output && \
    cargo build --release --bin lumina --features vulkan && \
    cp target/release/lumina /output/lumina
DEOF

  # Build in Docker and extract the binary
  local iid="lumina-npm-build-${platform}-$$"
  docker build \
    --platform "${docker_platform}" \
    -f "${ctx}/Dockerfile.npm-build" \
    -t "${iid}" \
    "${ctx}"

  # Extract binary from the image
  local cid
  cid="$(docker create --platform "${docker_platform}" "${iid}" /bin/true)"
  docker cp "${cid}:/output/lumina" "${pkg_dir}/lumina"
  docker rm "${cid}" >/dev/null
  docker rmi "${iid}" >/dev/null 2>&1 || true

  rm -rf "${ctx}"

  echo "    ✅ ${pkg_dir}/lumina"
}

build_linux_docker linux-x64   linux/amd64
build_linux_docker linux-arm64 linux/arm64

# ---------------------------------------------------------------------------
# Step 3: Verify all binaries
# ---------------------------------------------------------------------------
echo ""
echo "==> Verifying binaries"
for plat in darwin-arm64 darwin-x64 linux-arm64 linux-x64; do
  bin="${NATIVE_DIR}/lumina-binary-${plat}/bin/lumina"
  if [[ ! -f "${bin}" ]]; then
    echo "    ❌ MISSING: ${bin}"
    exit 1
  fi
  size=$(stat -f%z "${bin}" 2>/dev/null || stat -c%s "${bin}" 2>/dev/null)
  size_mb=$(( size / 1048576 ))
  filetype=$(file -b "${bin}" | head -c 60)
  echo "    ✅ ${plat}: ${size_mb} MB — ${filetype}"
done

# ---------------------------------------------------------------------------
# Step 4: Build TypeScript packages
# ---------------------------------------------------------------------------
echo ""
echo "==> Building @hikerm/lumina-sdk"
(cd "${SDK_DIR}" && pnpm run build:ts)

echo "==> Building @hikerm/lumina"
(cd "${TEXT_DIR}" && pnpm run build)

# ---------------------------------------------------------------------------
# Step 5: Publish
# ---------------------------------------------------------------------------
echo ""

PUBLISH_ARGS=(--access public --no-git-checks --registry "${REGISTRY}" --tag latest)
if [[ "${DRY_RUN}" == "true" ]]; then
  PUBLISH_ARGS+=(--dry-run)
fi

# Write a temporary .npmrc for authentication
NPMRC_BAK=""
UI_NPMRC="${REPO_ROOT}/ui/.npmrc"
if [[ -f "${UI_NPMRC}" ]]; then
  NPMRC_BAK="$(cat "${UI_NPMRC}")"
fi

# Append auth token for npmjs.org
if ! grep -q "registry.npmjs.org/:_authToken" "${UI_NPMRC}" 2>/dev/null; then
  echo "//registry.npmjs.org/:_authToken=\${NPM_PUBLISH_TOKEN}" >> "${UI_NPMRC}"
fi

cleanup_npmrc() {
  if [[ -n "${NPMRC_BAK}" ]]; then
    echo "${NPMRC_BAK}" > "${UI_NPMRC}"
  else
    # Remove the line we added
    sed -i.bak '/registry.npmjs.org\/:_authToken/d' "${UI_NPMRC}" && rm -f "${UI_NPMRC}.bak"
  fi
}
trap cleanup_npmrc EXIT

# Publish order matters: dependencies first
echo "==> Publishing @hikerm/lumina-sdk"
(cd "${REPO_ROOT}/ui" && pnpm publish "${PUBLISH_ARGS[@]}" acp)

echo "==> Publishing native binary packages"
for plat in darwin-arm64 darwin-x64 linux-arm64 linux-x64; do
  pkg="lumina-binary/lumina-binary-${plat}"
  echo "    Publishing @hikerm/lumina-binary-${plat}"
  (cd "${REPO_ROOT}/ui" && pnpm publish "${PUBLISH_ARGS[@]}" "${pkg}")
done

echo "==> Publishing @hikerm/lumina"
(cd "${REPO_ROOT}/ui" && pnpm publish "${PUBLISH_ARGS[@]}" text)

echo ""
if [[ "${DRY_RUN}" == "true" ]]; then
  echo "✅ Dry run complete. Pass --real to publish for real."
else
  echo "✅ All packages published to ${REGISTRY}"
fi
