//! フロントエンドから呼ばれるバックエンド処理の窓口。
//!
//! `backend_call(method, params)` の形で呼び出し、結果を JSON で返す。
//! 長い処理の進捗は `backend-progress` イベント
//! `{"event": "progress", "id", "message", "current", "total"}` で通知する。
//!
//! DB を触る処理（`list_playlists` / `inspect_package` / `export` / `import`）は
//! 同時に 1 件だけ、ブロッキング用のスレッドで実行する。実行中に別の処理が
//! 呼ばれた場合は `busy` エラーを返す。`status` / `cancel` は即時に処理する。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::config;
use crate::exporter::{collect_playlist_tree, export_playlists};
use crate::importer::{import_package, inspect_package, ImportOptions};
use crate::rekordbox::{locate_database, RekordboxDb};
use crate::task::{Cancelled, Task};

/// フロントエンドへ返すエラー。`code` は `error` / `cancelled` / `busy` / `backend`。
#[derive(Debug, Clone, Serialize)]
pub struct BackendError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceback: Option<String>,
}

impl BackendError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            traceback: None,
        }
    }

    fn from_anyhow(err: anyhow::Error) -> Self {
        if err.is::<Cancelled>() {
            return Self::new("cancelled", "キャンセルしました");
        }
        Self {
            code: "error".into(),
            message: format!("{err:#}"),
            traceback: Some(format!("{err:?}")),
        }
    }
}

type Reply = std::result::Result<Value, BackendError>;

/// 実行中の処理の状態。
#[derive(Default)]
pub struct Backend {
    busy: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    next_id: AtomicU64,
}

/// `busy` を処理の終了時に必ず下ろすためのガード。
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Backend {
    /// メソッドを呼び出し、結果を待つ。
    pub async fn call(&self, app: &AppHandle, method: &str, params: Value) -> Reply {
        match method {
            "status" => return Ok(status()),
            "cancel" => {
                if self.busy.load(Ordering::Acquire) {
                    self.cancel.store(true, Ordering::Release);
                }
                return Ok(Value::Null);
            }
            "shutdown" => {
                self.shutdown();
                return Ok(Value::Null);
            }
            "list_playlists" | "inspect_package" | "export" | "import" => {}
            _ => return Err(BackendError::new("error", format!("未知のメソッドです: {method}"))),
        }

        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(BackendError::new("busy", "別の処理が実行中です。お待ちください。"));
        }
        let guard = BusyGuard(self.busy.clone());
        self.cancel.store(false, Ordering::Release);

        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let emitter = app.clone();
        let task = Task::new(self.cancel.clone(), move |message, current, total| {
            let _ = emitter.emit(
                "backend-progress",
                json!({
                    "event": "progress",
                    "id": id,
                    "message": message,
                    "current": current,
                    "total": total,
                }),
            );
        });
        let method = method.to_string();
        let joined = tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            run_job(&method, params, &task)
        })
        .await;
        match joined {
            Ok(result) => result.map_err(BackendError::from_anyhow),
            Err(e) => Err(BackendError::new("backend", format!("処理が異常終了しました: {e}"))),
        }
    }

    /// アプリ終了時に、実行中の処理へ中断を要求する。
    pub fn shutdown(&self) {
        if self.busy.load(Ordering::Acquire) {
            self.cancel.store(true, Ordering::Release);
        }
    }
}

fn status() -> Value {
    let path = locate_database();
    json!({
        "app_name": config::APP_NAME,
        "version": config::APP_VERSION,
        "db_found": path.is_some(),
        "db_path": path.as_ref().map(|p| p.display().to_string()),
        "db_name": path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()),
        "package_ext": config::PACKAGE_EXT,
        "default_extract_dir": config::default_extract_dir().display().to_string(),
    })
}

/// Rekordbox の `master.db` を開く。
fn open_database() -> Result<RekordboxDb> {
    let path = locate_database().ok_or_else(|| {
        anyhow!("Rekordbox データベースが見つかりません。Rekordbox がインストール・初期化されているか確認してください。")
    })?;
    RekordboxDb::open(&path).map_err(|e| {
        anyhow!("Rekordbox データベースを開けませんでした: {e:#}\nRekordbox を終了してから再試行してください。")
    })
}

#[derive(Deserialize)]
struct InspectParams {
    path: String,
}

#[derive(Deserialize)]
struct ExportParams {
    uuids: Vec<String>,
    dest: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ImportOptionParams {
    extract_dir: Option<String>,
    conflict_policy: Option<String>,
    import_cues: Option<bool>,
    import_tags: Option<bool>,
    import_analysis: Option<bool>,
    import_artwork: Option<bool>,
}

#[derive(Deserialize)]
struct ImportParams {
    path: String,
    uuids: Vec<String>,
    #[serde(default)]
    options: Option<ImportOptionParams>,
}

fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T> {
    serde_json::from_value(params).context("パラメータが不正です")
}

fn run_job(method: &str, params: Value, task: &Task) -> Result<Value> {
    match method {
        "list_playlists" => {
            task.progress("プレイリストを取得中…", 0, 0);
            let db = open_database()?;
            Ok(serde_json::to_value(collect_playlist_tree(&db)?)?)
        }
        "inspect_package" => {
            let p: InspectParams = parse(params)?;
            task.progress("パッケージを解析中…", 0, 0);
            let path = PathBuf::from(&p.path);
            let (playlists, tracks) = inspect_package(&path)?;
            Ok(json!({
                "path": p.path,
                "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "track_count": tracks.len(),
                "playlists": playlists.iter().map(|pl| json!({
                    "uuid": pl.uuid,
                    "name": pl.name,
                    "parent": pl.parent,
                    "is_folder": pl.is_folder(),
                    "track_count": pl.track_uuids.len(),
                })).collect::<Vec<_>>(),
            }))
        }
        "export" => {
            let p: ExportParams = parse(params)?;
            let db = open_database()?;
            let package = export_playlists(&db, &p.uuids, Path::new(&p.dest), task)?;
            Ok(json!({
                "dest": p.dest,
                "playlists": package.playlists.len(),
                "tracks": package.tracks.len(),
            }))
        }
        "import" => {
            let p: ImportParams = parse(params)?;
            let o = p.options.unwrap_or_default();
            let extract_dir = o.extract_dir.as_deref().map(str::trim).unwrap_or("");
            let options = ImportOptions {
                extract_dir: if extract_dir.is_empty() {
                    config::default_extract_dir()
                } else {
                    PathBuf::from(extract_dir)
                },
                conflict_policy: o.conflict_policy.unwrap_or_else(|| config::CONFLICT_RENAME.into()),
                import_cues: o.import_cues.unwrap_or(true),
                import_tags: o.import_tags.unwrap_or(true),
                import_analysis: o.import_analysis.unwrap_or(true),
                import_artwork: o.import_artwork.unwrap_or(true),
            };
            let selected: HashSet<String> = p.uuids.into_iter().collect();
            let mut db = open_database()?;
            let result = import_package(&mut db, Path::new(&p.path), &options, Some(&selected), task)?;
            let mut value = serde_json::to_value(&result)?;
            value["summary"] = Value::String(result.summary());
            Ok(value)
        }
        _ => Err(anyhow!("未知のメソッドです: {method}")),
    }
}
