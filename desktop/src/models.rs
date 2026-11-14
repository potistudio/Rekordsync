//! パッケージで受け渡しするデータモデル。
//!
//! DB の行とは独立した、シリアライズ可能なプレーンデータ。マニフェスト JSON の
//! キー名・出力順は旧 Python 版（`rekordsync.models`）と同一で、相互に読み書きできる。

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::config;

/// 数値として解釈できる JSON 値を `i64` に変換する。`null` と数値以外は `None`。
fn lenient_opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        Some(serde_json::Value::Bool(b)) => Some(b as i64),
        _ => None,
    })
}

/// `lenient_opt_i64` と同じ規則で変換し、解釈できない場合は 0 とする。
fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(lenient_opt_i64(d)?.unwrap_or(0))
}

/// `null` を空文字列として読む。
fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// メモリーキュー / ホットキュー / ループ 1 点分の情報。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CueData {
    /// `DjmdCue.Kind`（0=memory, それ以外=hot cue 等）。
    #[serde(default, deserialize_with = "lenient_i64")]
    pub kind: i64,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub in_msec: Option<i64>,
    /// ループ終端。`None` ならポイントキュー。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub out_msec: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub color: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub color_table_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub active_loop: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub beat_loop_size: Option<i64>,
}

/// 楽曲 1 件分のメタデータと、パッケージ内での格納先。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackData {
    /// 元 DB の `DjmdContent.UUID`（重複判定に使う）。
    #[serde(default, deserialize_with = "lenient_string")]
    pub uuid: String,
    /// パッケージ zip 内での相対パス（`tracks/...`）。欠落時は空文字列。
    #[serde(default, deserialize_with = "lenient_string")]
    pub file: String,
    /// 元のファイル名。
    #[serde(default, deserialize_with = "lenient_string")]
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Rekordbox 形式（実BPM*100）。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub bpm: Option<i64>,
    /// 秒。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub length: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub rating: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub release_year: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub track_no: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub file_size: Option<i64>,
    /// `DjmdContent.Analysed`（解析状態フラグ。例: 105/233）。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub analysed: Option<i64>,
    /// Hz（解析で確定する音声スペック）。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<i64>,
    /// kbps。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<i64>,
    /// bit。
    #[serde(default, deserialize_with = "lenient_opt_i64", skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<CueData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub my_tags: Vec<String>,
    /// ANLZ 解析ファイルの種別 -> パッケージ zip 内の相対パス。
    /// 例: `{"DAT": "analysis/0001/ANLZ0000.DAT", "EXT": "..."}`
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub analysis: BTreeMap<String, String>,
    /// アートワーク画像のパッケージ zip 内の相対パス（無ければ `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
}

/// プレイリスト（またはフォルダ）1 件分。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistData {
    #[serde(default, deserialize_with = "lenient_string")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub uuid: String,
    /// 0=playlist, 1=folder, 4=smart。
    #[serde(default, deserialize_with = "lenient_i64")]
    pub attribute: i64,
    /// 親フォルダ名（無ければ `None`）。
    #[serde(default, skip_serializing_if = "is_none_or_empty")]
    pub parent: Option<String>,
    /// 収録曲を UUID で参照する（曲順どおり）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_uuids: Vec<String>,
}

fn is_none_or_empty(v: &Option<String>) -> bool {
    v.as_deref().map_or(true, str::is_empty)
}

impl PlaylistData {
    pub fn is_folder(&self) -> bool {
        self.attribute == 1
    }
}

/// パッケージ全体のマニフェストに対応するルートモデル。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    #[serde(default, deserialize_with = "lenient_string")]
    pub format: String,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub version: i64,
    #[serde(default, deserialize_with = "lenient_string")]
    pub created_at: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub source_app: String,
    #[serde(default)]
    pub tracks: Vec<TrackData>,
    #[serde(default)]
    pub playlists: Vec<PlaylistData>,
}

impl Package {
    /// 書き出し用の新しいパッケージを作る。`created_at` はローカル時刻（秒精度）。
    pub fn new(tracks: Vec<TrackData>, playlists: Vec<PlaylistData>) -> Self {
        Self {
            format: config::MANIFEST_FORMAT.to_string(),
            version: config::MANIFEST_VERSION,
            created_at: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            source_app: format!("{} {}", config::APP_NAME, config::APP_VERSION),
            tracks,
            playlists,
        }
    }
}
