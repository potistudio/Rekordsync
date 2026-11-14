//! 長い処理の進捗通知とキャンセル。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// ユーザー操作によって処理が中断されたことを表すエラー。
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("キャンセルしました")
    }
}

impl std::error::Error for Cancelled {}

type ProgressFn = Box<dyn Fn(&str, usize, usize) + Send + Sync>;

/// 実行中の処理 1 件分の文脈（進捗の通知先とキャンセル要求）。
pub struct Task {
    progress: ProgressFn,
    cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn new(cancel: Arc<AtomicBool>, progress: impl Fn(&str, usize, usize) + Send + Sync + 'static) -> Self {
        Self {
            progress: Box::new(progress),
            cancel,
        }
    }

    /// 進捗を通知しない、キャンセルもされないタスク（検証用）。
    #[cfg(test)]
    pub fn silent() -> Self {
        Self::new(Arc::default(), |_, _, _| {})
    }

    /// `(message, current, total)` の進捗を通知する。`total == 0` は件数不明を表す。
    pub fn progress(&self, message: &str, current: usize, total: usize) {
        (self.progress)(message, current, total);
    }

    /// キャンセル要求があれば `Cancelled` を返す。
    pub fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(Cancelled.into());
        }
        Ok(())
    }
}
