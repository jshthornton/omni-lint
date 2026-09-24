# plugins-src — guest modules + the authoring SDK

Everything here builds to `wasm32-wasip1` and runs sandboxed in the engine:

| Crate | Kind | What it shows |
|---|---|---|
| `omni-guest/` | SDK (rlib) | allocator, contract I/O, facts/finding types — link this to write a module |
| `demo-graphql-ast/` | language AST module | parse GraphQL SDL → facts, **no rules inside** |
| `rules/no-empty-type/` | single-rule module | the copy-me "first rule" template; findings as a returned array; options from `omni-lint.toml` |
| `rules/demo-gql-rules/` | rule **pack** | the eslint-style bundle: one `.wasm` extends the ruleset with a set of rules, each then enable/disable/configurable in config; includes an off-by-default (opt-in) rule and streaming reports |

```sh
./build-demos.sh    # build all + install into tests/fixtures/
```

To write your own rule: copy `rules/no-empty-type/`, edit `omni_rule_meta`
and `omni_rule_run`, build, drop the `.wasm` into your project's `rules/`.
To author a whole collection (the eslint-plugin model): copy
`rules/demo-gql-rules/` and add rules to the pack meta + dispatch. Details in
the root README ("Writing a rule").