//! 测试专用临时目录（约定与 `server/tests/*` 一致：`temp_dir()` + pid + 名字，
//! 不引入 `tempfile` 这种新依赖）。
//!
//! Drop 时删目录：测试崩在半路也不会留下一堆库文件。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct TmpDir {
    pub path: PathBuf,
}

impl TmpDir {
    pub(crate) fn create(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wxlike-admin-{}-{}-{n}",
            std::process::id(),
            tag.replace(['/', '\\', ':'], "_")
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        Path::new(&self.path).join(name)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
