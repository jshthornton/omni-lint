This directory is committed empty: drop `.wasm` plugin modules here,
optionally with a `limits.toml`.

`plugins/limits.toml` (optional):

```toml
# fuel default for all modules in this directory
fuel = 5_000_000_000
```

The engine validates each module at load: it must export `memory`,
`omni_alloc`, `omni_plugin_meta`, `omni_plugin_parse`, and
`omni_plugin_run_workspace`. A module that fails validation is reported as a
warning and skipped (no crash). See the root README for the full contract.
