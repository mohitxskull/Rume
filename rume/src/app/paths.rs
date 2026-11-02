//! Project directory layout.

use std::path::{Path, PathBuf};

/// Project directory layout.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub tmp: PathBuf,
    pub public: PathBuf,
}

impl ProjectPaths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.join("config"),
            tmp: root.join("tmp"),
            public: root.join("public"),
            root,
        }
    }

    pub fn from_current_dir() -> Self {
        Self::from_root(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub fn config_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.config.join(path.as_ref())
    }

    pub fn tmp_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.tmp.join(path.as_ref())
    }

    pub fn public_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.public.join(path.as_ref())
    }

    pub fn migrations_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join("migrations").join(path.as_ref())
    }
}
