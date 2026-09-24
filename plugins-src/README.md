# plugins-src — guest modules + the authoring SDK

Everything here builds to `wasm32-wasip1` and runs sandboxed in the engine:

| Crate | Kind | What it shows |
|---|---|---|
| `omni-guest/` | SDK (rlib) | allocator, contract I/O, facts/finding types — link this to write a module |
| `demo-graphql-ast/` | language AST module | parse GraphQL SDL → facts, **no rules inside** |
| `rules/no-empty-type/` | one-rule module | the copy-me template; findings as a returned array; options from `omni-lint.toml` |
| `rules/no-undefined-type/` | one-rule module | cross-file fold over `files[].facts` |
| `rules/duplicate-type/` | one-rule module | streaming findings via `env.omni_report` |
| `rules/no-unused-type/` | one-rule module | the `graphql.workspace` cross-file model (`requires` negotiation) |

```sh
./build-demos.sh    # build all + install into tests/fixtures/
```

To write your own rule: copy `rules/no-empty-type/`, edit `omni_rule_meta`
and `omni_rule_run`, build, drop the `.wasm` into your project's `rules/`.
Details in the root README ("Writing a rule").