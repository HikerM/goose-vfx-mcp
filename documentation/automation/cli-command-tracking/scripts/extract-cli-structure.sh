#!/bin/bash
# Extract CLI command structure from lumina at a specific version
# Usage: ./extract-cli-structure.sh <version>
# Example: ./extract-cli-structure.sh v1.15.0
#
# For tagged releases (v*), downloads pre-built binary from GitHub releases.
# For HEAD or non-release refs, builds from source.

set -e
set -o pipefail

VERSION=${1:-"HEAD"}
LUMINA_REPO=${LUMINA_REPO:-"$HOME/Development/lumina"}
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Create a temporary directory
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Check if version is a release tag (starts with 'v' followed by numbers)
is_release_tag() {
    [[ "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

# Download pre-built binary for a release version
download_release_binary() {
    local version=$1
    local safe_version=${version//\//-}
    local bin_dir="$TEMP_DIR/bin"
    mkdir -p "$bin_dir"
    
    echo "Downloading lumina $version from GitHub releases..." >&2
    
    # Use the official download script with custom bin dir and specific version
    curl -fsSL "https://github.com/HikerM/lumina/releases/download/stable/download_cli.sh" | \
        CONFIGURE=false LUMINA_BIN_DIR="$bin_dir" LUMINA_VERSION="$version" bash >&2 2>&1 || {
        echo "Error: Failed to download lumina $version" >&2
        return 1
    }
    
    echo "$bin_dir/lumina"
}

# Build lumina from source
build_from_source() {
    local version=$1
    local safe_version=${version//\//-}
    
    if [ ! -d "$LUMINA_REPO" ]; then
        echo "Error: LUMINA_REPO directory not found: $LUMINA_REPO" >&2
        exit 1
    fi
    
    cd "$LUMINA_REPO"
    
    if [ "$version" = "HEAD" ]; then
        echo "Building lumina from HEAD..." >&2
        cargo build --release --quiet >&2 2>&1 || {
            echo "Error: Failed to build lumina from HEAD" >&2
            return 1
        }
        echo "$LUMINA_REPO/target/release/lumina"
    else
        # Verify version exists
        if ! git rev-parse "$version" >/dev/null 2>&1; then
            echo "Error: Version $version not found in git history" >&2
            return 1
        fi
        
        echo "Building lumina from $version..." >&2
        
        # Create a worktree for the version
        local worktree_dir="$TEMP_DIR/lumina-$safe_version"
        git worktree add --quiet "$worktree_dir" "$version" >&2 2>&1 || {
            echo "Error: Failed to create worktree for $version" >&2
            return 1
        }
        
        cd "$worktree_dir"
        cargo build --release --quiet >&2 2>&1 || {
            echo "Error: Failed to build lumina from $version" >&2
            cd "$LUMINA_REPO"
            git worktree remove "$worktree_dir" 2>/dev/null || true
            return 1
        }
        
        # Clean up worktree but keep the binary accessible
        local bin_path="$worktree_dir/target/release/lumina"
        local temp_bin="$TEMP_DIR/lumina-$safe_version-bin"
        cp "$bin_path" "$temp_bin"
        
        cd "$LUMINA_REPO"
        git worktree remove "$worktree_dir" 2>/dev/null || true
        
        echo "$temp_bin"
    fi
}

# Get the lumina binary
if is_release_tag "$VERSION"; then
    LUMINA_BIN=$(download_release_binary "$VERSION")
else
    LUMINA_BIN=$(build_from_source "$VERSION")
fi

if [ -z "$LUMINA_BIN" ] || [ ! -x "$LUMINA_BIN" ]; then
    echo "Error: Lumina binary not found or not executable" >&2
    exit 1
fi

echo "Using binary: $LUMINA_BIN" >&2
echo "Binary version: $($LUMINA_BIN --version 2>&1)" >&2

# Run the Python extraction script
python3 "$SCRIPT_DIR/extract-cli-structure.py" "$LUMINA_BIN" "$VERSION"
