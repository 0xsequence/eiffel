//! LinuxKit-compatible `files:` layout YAML: parse, validate, resolve to archive members.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIR_MODE: u32 = 0o755;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutFile {
    files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    source: Option<String>,
    contents: Option<String>,
    #[serde(default)]
    directory: bool,
    mode: Option<String>,
}

/// A resolved archive member. Members are emitted in this order.
#[derive(Debug, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    pub mode: u32,
    pub body: Body,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Body {
    Directory,
    /// Copy the bytes of this host file (symlinks followed).
    Source(PathBuf),
    Inline(Vec<u8>),
}

pub fn load(path: &Path) -> Result<Vec<Item>> {
    let yaml = std::fs::read_to_string(path)
        .with_context(|| format!("reading layout {}", path.display()))?;
    parse(&yaml).with_context(|| format!("in layout {}", path.display()))
}

pub fn parse(yaml: &str) -> Result<Vec<Item>> {
    let layout: LayoutFile = serde_yaml::from_str(yaml)?;

    let mut items: Vec<Item> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in layout.files {
        check_path(&entry.path).with_context(|| format!("entry {:?}", entry.path))?;

        let body = match (entry.source, entry.contents, entry.directory) {
            (Some(source), None, false) => Body::Source(PathBuf::from(source)),
            (None, Some(contents), false) => Body::Inline(contents.into_bytes()),
            (None, None, true) => Body::Directory,
            _ => bail!(
                "entry {:?}: exactly one of `source`, `contents` or `directory: true` is required",
                entry.path
            ),
        };

        // The kernel's initramfs unpacker fails with ENOENT on a file whose parent
        // directory entry is missing, so synthesise the ones the layout left out.
        for (slash, _) in entry.path.match_indices('/') {
            let parent = &entry.path[..slash];
            if seen.insert(parent.to_string()) {
                items.push(Item {
                    path: parent.to_string(),
                    mode: DEFAULT_DIR_MODE,
                    body: Body::Directory,
                });
            }
        }

        let default_mode = match body {
            Body::Directory => DEFAULT_DIR_MODE,
            _ => DEFAULT_FILE_MODE,
        };
        let mode = match entry.mode.as_deref() {
            Some(m) => parse_mode(m).with_context(|| format!("entry {:?}", entry.path))?,
            None => default_mode,
        };

        if !seen.insert(entry.path.clone()) {
            bail!(
                "duplicate path {:?} (listed twice, or already created as a parent directory)",
                entry.path
            );
        }
        items.push(Item {
            path: entry.path,
            mode,
            body,
        });
    }

    Ok(items)
}

fn check_path(path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("path is empty");
    }
    if path.starts_with('/') {
        bail!("path must be relative to the archive root (no leading '/')");
    }
    for segment in path.split('/') {
        match segment {
            "" => bail!("path has an empty segment"),
            "." | ".." => bail!("path segment {:?} is not allowed", segment),
            _ => {}
        }
    }
    Ok(())
}

fn parse_mode(mode: &str) -> Result<u32> {
    let parsed = u32::from_str_radix(mode, 8)
        .with_context(|| format!("mode {mode:?} is not an octal string"))?;
    if parsed > 0o7777 {
        bail!("mode {mode:?} is out of range");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(yaml: &str) -> String {
        format!("{:#}", parse(yaml).unwrap_err())
    }

    #[test]
    fn parses_the_linuxkit_subset() {
        let items = parse(
            r#"
files:
  - path: cmd
    contents: "/app/example\n"
  - path: rootfs/app/example
    source: /workspace/example
    mode: "0755"
  - path: rootfs/dev
    directory: true
"#,
        )
        .unwrap();
        assert_eq!(
            items,
            vec![
                Item {
                    path: "cmd".into(),
                    mode: 0o644,
                    body: Body::Inline(b"/app/example\n".to_vec())
                },
                // `rootfs` and `rootfs/app` are synthesised, in order, before the file.
                Item {
                    path: "rootfs".into(),
                    mode: 0o755,
                    body: Body::Directory
                },
                Item {
                    path: "rootfs/app".into(),
                    mode: 0o755,
                    body: Body::Directory
                },
                Item {
                    path: "rootfs/app/example".into(),
                    mode: 0o755,
                    body: Body::Source("/workspace/example".into()),
                },
                Item {
                    path: "rootfs/dev".into(),
                    mode: 0o755,
                    body: Body::Directory
                },
            ]
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n    optional: true\n").contains("optional")
        );
        assert!(
            err("files:\n  - path: cmd\n    source: /a\nonboot:\n  - name: x\n").contains("onboot")
        );
        assert!(err("init:\n  - linuxkit/init:v1\n").contains("init"));
    }

    #[test]
    fn rejects_bad_paths() {
        assert!(err("files:\n  - path: /etc/passwd\n    source: /a\n").contains("leading '/'"));
        assert!(err("files:\n  - path: a/../b\n    source: /a\n").contains(".."));
        assert!(err("files:\n  - path: a//b\n    source: /a\n").contains("empty segment"));
        assert!(err("files:\n  - path: a/b/\n    source: /a\n").contains("empty segment"));
        assert!(err("files:\n  - path: \"\"\n    source: /a\n").contains("path is empty"));
    }

    #[test]
    fn rejects_duplicates() {
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n  - path: cmd\n    source: /b\n")
                .contains("duplicate path")
        );
        // A path already emitted as a synthesised parent counts as a duplicate too.
        assert!(
            err("files:\n  - path: a/b\n    source: /x\n  - path: a\n    directory: true\n")
                .contains("duplicate path")
        );
    }

    #[test]
    fn rejects_ambiguous_or_empty_bodies() {
        assert!(err("files:\n  - path: cmd\n").contains("exactly one of"));
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n    contents: hi\n")
                .contains("exactly one of")
        );
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n    directory: true\n")
                .contains("exactly one of")
        );
    }

    #[test]
    fn rejects_bad_modes() {
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n    mode: \"0o755\"\n").contains("octal")
        );
        assert!(
            err("files:\n  - path: cmd\n    source: /a\n    mode: \"100644\"\n")
                .contains("out of range")
        );
    }
}
