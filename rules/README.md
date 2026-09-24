This directory is committed empty: drop `.wasm` **rule modules** here — one
rule per module — optionally with a `limits.toml`.

Every module joins the ruleset à la carte, like an entry in eslint's `rules`
map: implement `omni_rule_meta` + `omni_rule_run`, build for
`wasm32-wasip1`, drop the file in, and tune it in `omni-lint.toml`:

```toml
[rules."acme/my-rule"]
severity = "warning"
[rules."acme/my-rule".options]
strict = true
```

Copy `plugins-src/rules/no-empty-type/` as a template (the `omni-guest` SDK
handles allocator, framing, facts and findings for you). The engine validates
each module at load (exports `memory`, `omni_alloc`, `omni_rule_meta`,
`omni_rule_run`; id `namespace/rule-name`; `requires` `<language>.facts` or
`<language>.workspace`) and reports a bad module as a warning and skips it.

See the root README, "Writing a rule", for the full contract.