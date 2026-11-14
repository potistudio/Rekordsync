//! アプリ全体で共有する定数とパス設定。

use std::path::PathBuf;

pub const APP_NAME: &str = "rekordsync";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// --- パッケージ形式の定義 -------------------------------------------------

/// パッケージ（zip）の拡張子。GUI のファイルダイアログで使用する。
pub const PACKAGE_EXT: &str = ".rsync";

/// パッケージ内のマニフェスト（メタデータ JSON）のファイル名。
pub const MANIFEST_NAME: &str = "manifest.json";

/// パッケージ内で楽曲ファイルを格納するディレクトリ。
pub const TRACKS_DIR: &str = "tracks";

/// パッケージ内で解析ファイル（ANLZ: 波形・ビートグリッド等）を格納するディレクトリ。
pub const ANALYSIS_DIR: &str = "analysis";

/// パッケージ内でアートワーク画像を格納するディレクトリ。
pub const ARTWORK_DIR: &str = "artwork";

/// アートワーク画像の正規ファイル名。Rekordbox は share 配下に
/// `PIONEER/Artwork/<3文字>/<UUID残り>/artwork.jpg` の形で保存している。
pub const ARTWORK_FILE_NAME: &str = "artwork.jpg";

/// マニフェストのスキーマバージョン。互換性チェックに使う。
pub const MANIFEST_VERSION: i64 = 1;

/// パッケージのフォーマット識別子。
pub const MANIFEST_FORMAT: &str = "rekordsync";

/// ANLZ 解析ファイルの種別（拡張子）から、格納時に使う正規ファイル名を返す。
///
/// Rekordbox はディレクトリ内のファイルを `ANLZ0000.<拡張子>` の名前で扱う。
/// - `DAT`: ビートグリッド・メモリーキュー・波形（粗）
/// - `EXT`: 詳細波形・ホットキュー等の拡張データ
/// - `2EX`: さらなる拡張データ
pub fn anlz_file_name(kind: &str) -> Option<&'static str> {
    match kind.to_ascii_uppercase().as_str() {
        "DAT" => Some("ANLZ0000.DAT"),
        "EXT" => Some("ANLZ0000.EXT"),
        "2EX" => Some("ANLZ0000.2EX"),
        _ => None,
    }
}

// --- 取り込み時のプレイリスト名衝突ポリシー -------------------------------
// 下記以外（"merge" を含む）は既存プレイリストへ統合する。

/// 別名（連番付き）で新規作成する。
pub const CONFLICT_RENAME: &str = "rename";
/// 取り込まずスキップする。
pub const CONFLICT_SKIP: &str = "skip";

// --- ローカルの既定パス ---------------------------------------------------

/// ユーザーのホームディレクトリ。
pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from).unwrap_or_default()
}

/// 楽曲を展開する既定フォルダ（ユーザーのミュージック配下）。
pub fn default_extract_dir() -> PathBuf {
    home_dir().join("Music").join("rekordsync")
}
