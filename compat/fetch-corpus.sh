#!/usr/bin/env bash
# Downloads the public comparison corpus into $1 (default: ./corpus):
#   - openapi-typescript 6.7.0's own examples and test fixtures
#   - every Nth OpenAPI 3 spec from APIs-guru/openapi-directory (N = $SAMPLE, default 1)
# The commits are pinned so results are reproducible.
set -euo pipefail
OUT=${1:-corpus}
SAMPLE=${SAMPLE:-1}
GURU_SHA=f04b8d0bcd39c52e1cf3ad7a5fe744709832ae49
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT/guru"
if [ ! -d "$OUT/ot" ]; then
  git clone -q --depth 1 --branch openapi-typescript@6.7.0 https://github.com/openapi-ts/openapi-typescript.git "$OUT/ot"
fi
awk -v n="$SAMPLE" 'NR % n == 1 || n == 1' "$HERE/corpus.txt" |
  xargs -d '\n' -P 16 -I{} sh -c 'f="$1/guru/$(printf %s "$2" | tr -c "A-Za-z0-9._\n-" _)"; u=$(printf %s "$2" | sed "s/ /%20/g"); [ -s "$f" ] || curl -sSfL --retry 3 -o "$f" "https://raw.githubusercontent.com/APIs-guru/openapi-directory/$3/$u"' _ "$OUT" {} "$GURU_SHA"
E="$OUT/ot/packages/openapi-typescript"
{
  ls "$E"/test/fixtures/*.yaml | grep -v '/_'
  echo "$E/examples/github-api.yaml"
  echo "$E/examples/github-api-next.yaml"
  echo "$E/examples/stripe-api.yaml"
  echo "$E/examples/digital-ocean-api/DigitalOcean-public.v2.yaml"
  echo "$E/examples/octokit-ghes-3.6-diff-to-api.json"
  ls "$OUT"/guru/*
} > "$OUT/specs.txt"
wc -l < "$OUT/specs.txt"
