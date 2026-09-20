#!/usr/bin/env bash
set -euo pipefail

ENV_FILE="$HOME/.claude/.env"
if [[ -f "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    source "$ENV_FILE"
fi

if [[ -n "${ZAI_API_KEY:-}" ]]; then
    curl -s -m 5 -H "Authorization: Bearer $ZAI_API_KEY" "https://api.z.ai/api/monitor/usage/quota/limit"
fi
