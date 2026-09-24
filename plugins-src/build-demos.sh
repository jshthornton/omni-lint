#!/usr/bin/env bash
# Build every demo module (one .wasm per crate) and install them into the
# e2e fixtures:
#   demo_graphql_ast.wasm  -> tests/fixtures/*/plugins/
#   rule_*.wasm            -> tests/fixtures/wasm-plugin/rules/
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
for r in no_empty_type no_undefined_type duplicate_type no_unused_type; do
  cp "$OUT/rule_${r}.wasm" "$FIXTURE/rules/"
  cp "$OUT/rule_${r}.wasm" "$BENCH/rules/"
done

echo "installed:"
find "$FIXTURE" -name '*.wasm' | sort