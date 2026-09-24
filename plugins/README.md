This directory is committed empty: drop `.wasm` **language AST modules**
here, optionally with a `limits.toml`.

Language modules parse a language and publish `<language>.facts` (and
optionally `<language>.workspace`) JSON capabilities. They contain **no
rules** — rules are separate one-rule modules in `../rules/`.

`plugins/limits.toml` (optional):

```toml
# fuel default for all modules in this directory
fuel = 5_000_000_000
```

The engine validates each module at load: it must export `memory`,
`omni_alloc`, `omni_plugin_meta`, and `omni_plugin_parse`; the optional
`omni_plugin_workspace` overrides the host-synthesized cross-file facts. A
module that fails validation is reported as a warning and skipped (no crash).
See the root README for the full contract.