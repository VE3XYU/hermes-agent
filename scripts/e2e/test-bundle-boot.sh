#!/usr/bin/env bash
#
# Phase 0 task 0.6: E2E gate — prove a bundle boots on a bare system.
#
# Runs inside debian:stable-slim (docker) with NO python/node/git installed.
# The bundle must be fully self-contained: its own Python, venv, node, etc.
#
# Usage: bash scripts/e2e/test-bundle-boot.sh <bundle-dir>
#    or: bash scripts/e2e/test-bundle-boot.sh <bundle-archive.tar.zst>
#
# Requires: docker (or podman). If neither is available, exits with an error
# (the gate must run under container isolation — a host with python/node
# installed cannot prove the bundle is self-contained).
#
# This script FAILS CLOSED: every check is a hard error.
#   - doctor --preflight failure → exit 1 (no fallback to import check)
#   - missing manifest.json → exit 1 (no warning)
#   - no docker/podman → exit 1 (no local fallback)

set -euo pipefail

BUNDLE_INPUT="${1:-}"
if [ -z "$BUNDLE_INPUT" ]; then
    echo "Usage: bash scripts/e2e/test-bundle-boot.sh <bundle-dir-or-archive>"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Resolve bundle directory (unpack archive if needed)
if [ -d "$BUNDLE_INPUT" ]; then
    BUNDLE_DIR="$BUNDLE_INPUT"
elif [[ "$BUNDLE_INPUT" == *.tar.zst ]]; then
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    echo "==> Unpacking $BUNDLE_INPUT..."
    tar --zstd -xf "$BUNDLE_INPUT" -C "$WORK"
    BUNDLE_DIR="$WORK/bundle"
    if [ ! -d "$BUNDLE_DIR" ]; then
        # Try finding the bundle dir
        BUNDLE_DIR=$(find "$WORK" -name "manifest.json" -type f -exec dirname {} \; | head -1)
    fi
else
    echo "ERROR: $BUNDLE_INPUT is not a directory or .tar.zst archive" >&2
    exit 1
fi

if [ ! -d "$BUNDLE_DIR" ]; then
    echo "ERROR: bundle directory not found" >&2
    exit 1
fi

echo "==> Bundle: $BUNDLE_DIR"

# ─── Require Docker or Podman (the real gate) ─────────────────────────

CONTAINER_CMD=""
if command -v docker &>/dev/null; then
    CONTAINER_CMD="docker"
elif command -v podman &>/dev/null; then
    CONTAINER_CMD="podman"
fi

if [ -z "$CONTAINER_CMD" ]; then
    echo "ERROR: docker/podman not available — cannot run the bare-container boot gate." >&2
    echo "       This gate requires container isolation to prove the bundle is self-contained." >&2
    echo "       A host with python/node installed cannot prove the bundle carries everything." >&2
    exit 1
fi

echo "==> Running in debian:stable-slim via $CONTAINER_CMD (no python/node/git)..."

BUNDLE_ABSOLUTE="$(cd "$BUNDLE_DIR" && pwd)"

# Every check inside the container is a hard error — set -e is active.
$CONTAINER_CMD run --rm -v "$BUNDLE_ABSOLUTE:/b:ro" debian:stable-slim /bin/sh -c '
    set -e
    echo "--- Checking no system python/node/git ---"
    which python3 2>/dev/null && { echo "FAIL: python3 found on host"; exit 1; } || true
    which node 2>/dev/null && { echo "FAIL: node found on host"; exit 1; } || true
    which git 2>/dev/null && { echo "FAIL: git found on host"; exit 1; } || true
    echo "PASS: no system python/node/git"

    echo "--- bin/hermes --version ---"
    /b/bin/hermes --version

    echo "--- doctor --preflight ---"
    HERMES_HOME=/tmp/hh /b/bin/hermes doctor --preflight

    echo "--- manifest verification ---"
    if [ ! -f /b/manifest.json ]; then
        echo "FAIL: manifest.json not found in bundle" >&2
        exit 1
    fi
    /b/runtime/venv/bin/python -c "import json; m=json.loads(open(\"/b/manifest.json\").read()); assert m[\"schema\"]==1; assert len(m.get(\"files\",{}))>0; print(\"MANIFEST_OK\")"

    echo "E2E_PASS"
'
echo "==> Docker E2E gate passed!"
