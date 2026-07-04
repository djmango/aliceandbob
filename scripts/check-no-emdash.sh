#!/usr/bin/env bash
# Fail if U+2014 (em dash) appears anywhere in frontend source.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WEB="$ROOT/web"

paths=("$WEB/src" "$WEB/index.html")
if [[ -d "$WEB/public" ]]; then
  paths+=("$WEB/public")
fi

if matches="$(rg -n $'\xE2\x80\x94' "${paths[@]}" 2>/dev/null || true)" && [[ -n "$matches" ]]; then
  echo "Em dash (U+2014) found in frontend source. Use a hyphen (-) instead:"
  echo "$matches"
  exit 1
fi

echo "ok: no em dashes in frontend"
