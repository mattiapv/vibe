#!/bin/bash
# Install Anthropic's Claude.
set -euxo pipefail

# Set this env var so claude doesn't complain about running as root.
echo "export IS_SANDBOX=1" >> .bashrc

# Keep Claude's credentials and session state in Vibe's persistent guest share.
mkdir -p .claude-config
if [[ ! -e .claude-config/.claude.json ]]; then
  cat > .claude-config/.claude.json <<'CLAUDE_CONFIG'
{
  "firstStartTime": "2026-01-01T00:00:00.000Z"
}
CLAUDE_CONFIG
fi
ln -sfn .claude-config/.claude.json .claude.json

tool='    claude = "latest"'
echo "$tool" >> .config/mise/config.toml

mise install
