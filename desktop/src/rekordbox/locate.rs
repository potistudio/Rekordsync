//! Rekordbox データベース（`master.db`）の場所の解決。

use std::path::{Path, PathBuf};

/// この環境変数にファイルパスを指定すると、自動検出より優先して使う。
pub const DB_PATH_ENV: &str = "REKORDSYNC_DB_PATH";

/// Pioneer アプリのユーザーデータフォルダ。
fn pioneer_app_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("Pioneer"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|p| PathBuf::from(p).join("Library/Application Support/Pioneer"))
    } else {
        None
    }
}

/// `rekordboxAgent/storage/options.json` の `options` 配列（`[キー, 値]` の組）から
/// `db-path` を読む。
fn db_path_from_options(app_dir: &Path) -> Option<PathBuf> {
    let path = app_dir.join("rekordboxAgent").join("storage").join("options.json");
    let text = std::fs::read_to_string(path).ok()?;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    data["options"].as_array()?.iter().find_map(|pair| {
        let pair = pair.as_array()?;
        if pair.first()?.as_str()? == "db-path" {
            Some(PathBuf::from(pair.get(1)?.as_str()?))
        } else {
            None
        }
    })
}

/// Rekordbox 6/7 の `master.db` を探す。見つからなければ `None`。
///
/// 1. 環境変数 `REKORDSYNC_DB_PATH`
/// 2. `options.json` の `db-path`（Rekordbox 6/7 が使う設定）
/// 3. 既定の場所 `<Pioneer>/rekordbox/master.db`
pub fn locate_database() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(DB_PATH_ENV) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let app_dir = pioneer_app_dir()?;
    if let Some(p) = db_path_from_options(&app_dir) {
        if p.is_file() {
            return Some(p);
        }
    }
    let p = app_dir.join("rekordbox").join("master.db");
    p.is_file().then_some(p)
}
