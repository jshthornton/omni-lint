#!/usr/bin/env bash
# Build every demo module (one .wasm per crate) and install them into the
# e2e fixtures:
#   demo_graphql_ast.wasm   -> tests/fixtures/*/plugins/
#   rule_no_empty_type.wasm -> tests/fixtures/*/rules/  (single-rule module)
#   demo_gql_rules.wasm     -> tests/fixtures/*/rules/  (rule pack)
#
# Usage: ./build-demos.sh
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --target wasm32-wasip1

OUT=target/wasm32-wasip1/release
FIXTURE=../tests/fixtures/wasm-plugin
BENCH=../tests/fixtures/bench-wasm
mkdir -p "$FIXTURE/plugins" "$FIXTURE/rules" "$BENCH/plugins" "$BENCH/rules"

cp "$OUT/demo_graphql_ast.wasm" "$FIXTURE/plugins/"
cp "$OUT/demo_graphql_ast.wasm" "$BENCH/plugins/"
rm -f "$FIXTURE"/rules/*.wasm "$BENCH"/rules/*.wasm
for r in rule_no_empty_type demo_gql_rules; do
  cp "$OUT/${r}.wasm" "$FIXTURE/rules/"
  cp "$OUT/${r}.wasm" "$BENCH/rules/"
done

echo "installed:"
find "$FIXTURE" -name '*.wasm' | sort