This directory is committed empty: drop `.wasm` **rule packs** here, one
bundle per `.wasm`, optionally with a `limits.toml`.

This is the eslint-plugin model: dropping a pack in *extends* the app's
ruleset with every rule the pack declares; each rule is then keyed by its
full id in `omni-lint.toml` to enable/disable/re-grade/configure
individually:

```toml
[rules."acme/no-empty-type"]   # per-rule control inside the pack
enabled = false

[rules."acme/no-bar"]          # a pack rule graded "off": opt in here
enabled = true

[rules."acme/no-foo".options]  # per-rule options
strict = true
```

Copy `plugins-src/rules/demo-gql-rules/` (bundle) or
`plugins-src/rules/no-empty-type/` (a pack of one) as a template — the
`omni-guest` SDK handles allocator, framing, facts and findings for you. The
engine validates each module at load (exports `memory`, `omni_alloc`,
`omni_pack_meta` or `omni_rule_meta`, `omni_rule_run`; rule ids under the
pack namespace; `requires` `<language>.facts` or `<language>.workspace`) and
reports a bad module as a warning and skips it.

See the root README, "Writing rules (the ESLint story)", for the full
contract.