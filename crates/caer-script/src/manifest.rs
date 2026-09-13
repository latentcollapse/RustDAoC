//! Addon discovery and `.toc` manifests (WoW-community muscle memory, Lua 5.1-era).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::HostError;

/// Host API version advertised as TOC `Interface`.
pub const HOST_API_VERSION: u32 = 1;

/// Parsed addon package: metadata + ordered Lua entry files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonManifest {
    pub id: String,
    pub title: String,
    pub version: String,
    pub interface: u32,
    pub saved_variables: Vec<String>,
    pub scripts: Vec<String>,
    pub root: PathBuf,
}

impl AddonManifest {
    pub fn version_major(&self) -> u32 {
        version_major(&self.version)
    }
}

pub fn version_major(version: &str) -> u32 {
    version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Find addon directories under `root` (one `.toc` per subdirectory).
pub fn discover(root: impl AsRef<Path>) -> Result<Vec<AddonManifest>, HostError> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(HostError::Manifest {
            path: root.display().to_string(),
            detail: "addons directory does not exist".into(),
        });
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| HostError::Manifest {
            path: root.display().to_string(),
            detail: e.to_string(),
        })?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        match find_toc(&path) {
            Some(toc) => out.push(parse_toc(&toc)?),
            None => continue,
        }
    }
    Ok(out)
}

pub fn load_manifest(addon_dir: impl AsRef<Path>) -> Result<AddonManifest, HostError> {
    let dir = addon_dir.as_ref();
    let toc = find_toc(dir).ok_or_else(|| HostError::Manifest {
        path: dir.display().to_string(),
        detail: "no .toc file (expected `{id}.toc` with ## Interface / ## Version / script list)"
            .into(),
    })?;
    parse_toc(&toc)
}

fn find_toc(dir: &Path) -> Option<PathBuf> {
    let prefer = dir.join(format!(
        "{}.toc",
        dir.file_name().and_then(|s| s.to_str()).unwrap_or("")
    ));
    if prefer.is_file() {
        return Some(prefer);
    }
    let mut tocs: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toc"))
        .collect();
    tocs.sort();
    tocs.into_iter().next()
}

pub fn parse_toc(path: &Path) -> Result<AddonManifest, HostError> {
    let text = fs::read_to_string(path).map_err(|e| HostError::Manifest {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    parse_toc_text(path, &root, &text)
}

pub fn parse_toc_text(path: &Path, root: &Path, text: &str) -> Result<AddonManifest, HostError> {
    let id = root
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("addon")
        .to_string();
    let mut title = id.clone();
    let mut version = "0.0.0".to_string();
    let mut interface = HOST_API_VERSION;
    let mut saved_variables = Vec::new();
    let mut scripts = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || (line.starts_with('#') && !line.starts_with("##")) {
            continue;
        }
        if let Some(meta) = line.strip_prefix("##") {
            let meta = meta.trim();
            let Some((key, value)) = meta.split_once(':') else {
                return Err(HostError::Manifest {
                    path: format!("{}:{}", path.display(), lineno + 1),
                    detail: format!("TOC tag `{meta}` needs `## Key: value`"),
                });
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "Interface" | "CAER-API" => {
                    interface = value.parse().map_err(|_| HostError::Manifest {
                        path: format!("{}:{}", path.display(), lineno + 1),
                        detail: format!("Interface must be an integer, got `{value}`"),
                    })?;
                }
                "Title" => title = value.to_string(),
                "Version" => version = value.to_string(),
                "SavedVariables" => {
                    saved_variables = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                _ => {}
            }
            continue;
        }
        scripts.push(line.to_string());
    }

    if scripts.is_empty() {
        return Err(HostError::Manifest {
            path: path.display().to_string(),
            detail: "TOC lists no Lua files; add `main.lua` on its own line".into(),
        });
    }
    for s in &scripts {
        if s.contains('\0') || s.contains("..") || Path::new(s).is_absolute() {
            return Err(HostError::Manifest {
                path: path.display().to_string(),
                detail: format!("script `{s}` must be a relative path inside the addon directory"),
            });
        }
    }

    Ok(AddonManifest {
        id,
        title,
        version,
        interface,
        saved_variables,
        scripts,
        root: root.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_requires_script_list() {
        let err = parse_toc_text(
            Path::new("empty.toc"),
            Path::new("EmptyAddon"),
            "## Interface: 1\n## Version: 1.0.0\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("no Lua files"));
    }

    #[test]
    fn toc_rejects_parent_escape() {
        let err = parse_toc_text(
            Path::new("bad.toc"),
            Path::new("Bad"),
            "## Interface: 1\n../secret.lua\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("relative path"));
    }
}
