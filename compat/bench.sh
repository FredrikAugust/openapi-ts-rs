#!/usr/bin/env bash
# Times openapi-typescript 6.7.0 against openapi-ts-rs on openapi-typescript 6.7.0's own example
# specs and prints a Markdown table. Needs hyperfine >= 2.0 (peak memory), node and `npm ci` here.
#   ./bench.sh [path/to/openapi-ts-rs]   (default: ../target/release/openapi-ts-rs)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=$(realpath "${1:-$HERE/../target/release/openapi-ts-rs}")
OT="$HERE/node_modules/openapi-typescript/bin/cli.js"
CORPUS=${CORPUS:-$HERE/corpus}
command -v hyperfine >/dev/null || { echo "hyperfine is required: https://github.com/sharkdp/hyperfine" >&2; exit 1; }
[ -f "$OT" ] || { echo "run npm ci in $HERE first" >&2; exit 1; }
[ -x "$BIN" ] || { echo "$BIN not found; run cargo build --release" >&2; exit 1; }
if [ ! -d "$CORPUS/ot" ]; then
  git -c advice.detachedHead=false clone -q --depth 1 --branch openapi-typescript@6.7.0 https://github.com/openapi-ts/openapi-typescript.git "$CORPUS/ot"
fi
E="$CORPUS/ot/packages/openapi-typescript/examples"
SPECS=(
  github-api.yaml
  stripe-api.yaml
  digital-ocean-api/DigitalOcean-public.v2.yaml
  octokit-ghes-3.6-diff-to-api.json
)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

for spec in "${SPECS[@]}"; do
  echo "benchmarking $spec" >&2
  # Run from the spec's directory so relative $refs resolve the same way for both generators.
  (cd "$(dirname "$E/$spec")" && hyperfine -N --warmup 1 --min-runs 3 --export-json "$OUT/$(basename "$spec").json" \
    -n openapi-typescript "node $OT $(basename "$spec") -o $OUT/a.d.ts" \
    -n openapi-ts-rs "$BIN $(basename "$spec") -o $OUT/b.d.ts" >/dev/null)
done

node - "$OUT" "${SPECS[@]}" <<'JS'
const fs = require("node:fs");
const [dir, ...specs] = process.argv.slice(2);
const cell = (r) => {
  const t = r.summary?.time_wall_clock ?? { mean: r.mean, stddev: r.stddev };
  const digits = t.mean < 0.1 ? 1 : 0;
  const ms = (s) => (s * 1000).toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits });
  const mem = r.summary?.memory_peak_resident?.median;
  return `${ms(t.mean)} ± ${ms(t.stddev)} ms` + (mem ? `, ${Math.round(mem / 2 ** 20)} MiB` : "");
};
console.log("| Spec | openapi-typescript 6.7.0 | openapi-ts-rs | Speedup |");
console.log("|---|---|---|---|");
for (const spec of specs) {
  const { results } = JSON.parse(fs.readFileSync(`${dir}/${spec.split("/").pop()}.json`, "utf8"));
  const [ot, rs] = ["openapi-typescript", "openapi-ts-rs"].map((n) => results.find((r) => (r.name ?? r.command) === n));
  const mean = (r) => r.summary?.time_wall_clock?.mean ?? r.mean;
  console.log(`| \`${spec}\` | ${cell(ot)} | ${cell(rs)} | ${Math.round(mean(ot) / mean(rs))}× |`);
}
JS
