//! Hub seeds from a directory (step 2.6 of the factory plan): `people.toml`, `components.toml`, `scopes.toml`,
//! `repositories.toml` and `links/*.jsonl` (by file name) are read at startup from `HUB_SEED_DIR` instead of being embedded in the binary.
//! That way an installation carries its data next to the image, not inside it.
//!
//! No directory (the variable is unset or the path does not exist) or not a single seed file in it: with `HUB_SEED_REQUIRED=true`
//! the start fails, otherwise hub starts without seeds. Nothing is retired: the import only adds and updates. A file
//! missing from the directory: that seed is skipped. A file that cannot be read or parsed: the start fails, as it did with a broken
//! embedded file.
//!
//! The image may carry the seeds `services/hub/seed` in `/seed` (`services/Dockerfile`) until step 5.4, where the
//! directory comes from a ConfigMap in the installation values; the installation values set `HUB_SEED_DIR=/seed` and
//! `HUB_SEED_REQUIRED=true` (hub.env).

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Seed file texts; `None`: the file is not in the directory.
#[derive(Default, Debug)]
pub struct Seeds {
    pub people: Option<String>,
    pub components: Option<String>,
    pub scopes: Option<String>,
    pub repositories: Option<String>,
    /// `links/*.jsonl`: file name and text, in ascending name order.
    pub links: Vec<(String, String)>,
}

impl Seeds {
    /// Seeds by `HUB_SEED_DIR` and `HUB_SEED_REQUIRED` (`true` or `1`).
    pub fn from_env() -> anyhow::Result<Self> {
        let dir = std::env::var_os("HUB_SEED_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let required = matches!(
            std::env::var("HUB_SEED_REQUIRED").as_deref(),
            Ok("true" | "1")
        );
        Self::load(dir.as_deref(), required)
    }

    /// The seed directory, or an empty set if there is no directory and it is not required.
    pub fn load(dir: Option<&Path>, required: bool) -> anyhow::Result<Self> {
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            anyhow::ensure!(
                !required,
                "HUB_SEED_REQUIRED=true, but HUB_SEED_DIR ({}) is not a directory",
                dir.map_or("unset".into(), |d| d.display().to_string())
            );
            tracing::warn!(
                dir = dir.map(|d| d.display().to_string()),
                "no seed directory (HUB_SEED_DIR unset or missing): starting without seeds"
            );
            return Ok(Self::default());
        };
        let seeds = Self::read(dir)?;
        anyhow::ensure!(
            !required || !seeds.files().is_empty(),
            "HUB_SEED_REQUIRED=true, but {} has no seed files",
            dir.display()
        );
        tracing::info!(dir = %dir.display(), files = ?seeds.files(), "seed directory");
        Ok(seeds)
    }

    /// Reading an existing directory.
    pub fn read(dir: &Path) -> anyhow::Result<Self> {
        let file = |name: &str| optional(&dir.join(name));
        let mut links = Vec::new();
        let links_dir = dir.join("links");
        if links_dir.is_dir() {
            for entry in std::fs::read_dir(&links_dir)
                .with_context(|| format!("read {}", links_dir.display()))?
            {
                let path = entry
                    .with_context(|| format!("read {}", links_dir.display()))?
                    .path();
                if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .with_context(|| format!("{}: name is not UTF-8", path.display()))?
                        .to_owned();
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("read {}", path.display()))?;
                    links.push((name, text));
                }
            }
        }
        links.sort();
        Ok(Self {
            people: file("people.toml")?,
            components: file("components.toml")?,
            scopes: file("scopes.toml")?,
            repositories: file("repositories.toml")?,
            links,
        })
    }

    /// Names of the files found, for the log.
    pub fn files(&self) -> Vec<String> {
        [
            ("people.toml", &self.people),
            ("components.toml", &self.components),
            ("scopes.toml", &self.scopes),
            ("repositories.toml", &self.repositories),
        ]
        .into_iter()
        .filter(|(_, t)| t.is_some())
        .map(|(n, _)| n.to_owned())
        .chain(self.links.iter().map(|(n, _)| format!("links/{n}")))
        .collect()
    }
}

/// File text; `None` if there is no file. Any other read error fails the start.
fn optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// The seed directories in the tree for the parse tests: the neutral example and, when the tree carries one, the seed of
/// the install the image is built for (`services/hub/seed`, copied to /seed by services/Dockerfile).
#[cfg(test)]
pub fn tree_dirs() -> Vec<PathBuf> {
    let install = Path::new(env!("CARGO_MANIFEST_DIR")).join("seed");
    let mut dirs = vec![example_dir()];
    if install.is_dir() {
        dirs.push(install);
    }
    dirs
}

/// Neutral seed example (`examples/seed` at the repository root).
#[cfg(test)]
pub fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/seed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temporary test directory; removed when the test exits, including on panic.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("hub-seed-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_dir_fails_only_when_required() {
        let tmp = Tmp::new("missing");
        let gone = tmp.0.join("nope");
        let e = Seeds::load(Some(&gone), true).unwrap_err().to_string();
        assert!(e.contains("HUB_SEED_REQUIRED"), "{e}");
        assert!(Seeds::load(None, true).is_err());
        let s = Seeds::load(Some(&gone), false).unwrap();
        assert!(s.files().is_empty());
        assert!(Seeds::load(None, false).unwrap().files().is_empty());
    }

    // An empty directory with a required seed (image built without `seed/`) fails the start instead of starting without data.
    #[test]
    fn empty_dir_fails_only_when_required() {
        let tmp = Tmp::new("empty");
        let e = Seeds::load(Some(&tmp.0), true).unwrap_err().to_string();
        assert!(e.contains("no seed files"), "{e}");
        assert!(Seeds::load(Some(&tmp.0), false).unwrap().files().is_empty());
    }

    #[test]
    fn reads_files_and_links_by_name() {
        let tmp = Tmp::new("read");
        std::fs::write(tmp.0.join("people.toml"), "[[person]]\n").unwrap();
        std::fs::create_dir(tmp.0.join("links")).unwrap();
        std::fs::write(tmp.0.join("links/b.jsonl"), "b").unwrap();
        std::fs::write(tmp.0.join("links/a.jsonl"), "a").unwrap();
        std::fs::write(tmp.0.join("links/notes.txt"), "x").unwrap();
        let s = Seeds::load(Some(&tmp.0), true).unwrap();
        assert_eq!(s.people.as_deref(), Some("[[person]]\n"));
        assert_eq!(s.components, None);
        assert_eq!(
            s.links,
            [
                ("a.jsonl".into(), "a".into()),
                ("b.jsonl".into(), "b".into())
            ]
        );
        assert_eq!(s.files(), ["people.toml", "links/a.jsonl", "links/b.jsonl"]);
    }

    #[test]
    fn unreadable_file_fails() {
        let tmp = Tmp::new("bad");
        // A directory in place of a file: reading fails with something other than NotFound.
        std::fs::create_dir(tmp.0.join("scopes.toml")).unwrap();
        assert!(Seeds::read(&tmp.0).is_err());
    }

    #[test]
    fn tree_seed_dirs_have_every_file() {
        for dir in tree_dirs() {
            let s = Seeds::read(&dir).unwrap();
            assert_eq!(s.files().len(), 5, "{}: {:?}", dir.display(), s.files());
        }
    }
}
