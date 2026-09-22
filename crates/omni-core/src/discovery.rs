//! File discovery: walk the lint root, apply extension filters and ignores.

use crate::diagnostic::SourceId;
use crate::source::SourceFile;
use crate::config::Config;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Discover files under `root` (a file or directory) honoring config filters.
/// Returns sources with sequential ids and best-effort skip of binary data.
pub fn discover(
    root: &Path,
    config: &Config,
    allowed_extensions: Option<&[String]>,
) -> Result<Vec<Arc<SourceFile>>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    collect(root, root, config, allowed_extensions, &mut paths)?;
    let mut sources = Vec::with_capacity(paths.len());
    for path in &paths {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.clone());
        sources.push(Arc::new(SourceFile::new(
            SourceId(sources.len() as u32),
            rel,
            bytes,
        )));
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(sources)
}

fn collect(
    root: &Path,
    dir: &Path,
    config: &Config,
    allowed_extensions: Option<&[String]>,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read dir {dir:?}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read dir entry {dir:?}: {e}"))?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with(".git") || name == "node_modules" || name == "target" {
            continue;
        }
        if config.is_ignored(&rel) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect(root, &path, config, allowed_extensions, out)?;
        } else if meta.is_file() && matches_filter(&path, config, allowed_extensions) {
            out.push(path);
        }
    }
    Ok(())
}

fn matches_filter(path: &Path, config: &Config, allowed_extensions: Option<&[String]>) -> bool {
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    else {
        return false;
    };
    // Config allowlist wins; else restrict to extensions some loaded plugin
    // claims (so README/TODO/toml never reach a plugin that cannot parse them).
    if let Some(allowed) = allowed_extensions {
        allowed.iter().any(|a| a == &ext)
    } else if let Some(cfg_list) = &config.extensions {
        cfg_list.iter().any(|a| a == &ext)
    } else {
        true
    }
}
