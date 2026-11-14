//! Rekordbox 6/7 の `master.db`（SQLCipher で暗号化された SQLite）の読み書き。
//!
//! pyrekordbox `Rekordbox6Database` のうち本アプリが使う機能を移植したもの。
//! 書き込みでは、Rekordbox が整合性の判定に使う次の値を pyrekordbox と同じ規則で設定する。
//!
//! - `created_at` / `updated_at`: `YYYY-MM-DD HH:MM:SS.SSS +00:00`（UTC）
//! - `rb_local_usn`: 変更 1 回ごとに 1 ずつ進む番号。行には最後の変更時の番号を入れる
//! - `agentRegistry.localUpdateCount`: 上記の番号の最新値
//! - `masterPlaylists6.xml`: 追加したプレイリストの `<NODE>`

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension, Row};

use super::key::database_key;
use super::playlist_xml::{self, PlaylistNode};

/// 書き込み可能な主要テーブル名。SQL 文へ埋め込むため固定の集合に限る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Table {
	Content,
	Artist,
	Album,
	Genre,
	Cue,
	MyTag,
	SongMyTag,
	Playlist,
	SongPlaylist,
}

impl Table {
	fn name(self) -> &'static str {
		match self {
			Table::Content => "djmdContent",
			Table::Artist => "djmdArtist",
			Table::Album => "djmdAlbum",
			Table::Genre => "djmdGenre",
			Table::Cue => "djmdCue",
			Table::MyTag => "djmdMyTag",
			Table::SongMyTag => "djmdSongMyTag",
			Table::Playlist => "djmdPlaylist",
			Table::SongPlaylist => "djmdSongPlaylist",
		}
	}
}

/// `djmdPlaylist` の 1 行。
#[derive(Debug, Clone)]
pub struct PlaylistRow {
	pub id: String,
	pub uuid: String,
	pub name: String,
	pub attribute: i64,
	pub parent_id: Option<String>,
}

/// `djmdContent` の 1 行（本アプリで使う列と、関連テーブルの名前）。
#[derive(Debug, Clone, Default)]
pub struct ContentRow {
	pub id: String,
	pub uuid: String,
	pub folder_path: Option<String>,
	pub file_name_l: Option<String>,
	pub title: Option<String>,
	pub artist: Option<String>,
	pub album: Option<String>,
	pub genre: Option<String>,
	pub key: Option<String>,
	pub color: Option<String>,
	pub bpm: Option<i64>,
	pub length: Option<i64>,
	pub rating: Option<i64>,
	pub commnt: Option<String>,
	pub release_year: Option<i64>,
	pub track_no: Option<i64>,
	pub file_size: Option<i64>,
	pub analysed: Option<i64>,
	pub sample_rate: Option<i64>,
	pub bit_rate: Option<i64>,
	pub bit_depth: Option<i64>,
	pub analysis_data_path: Option<String>,
	pub analysis_updated: Option<String>,
	pub image_path: Option<String>,
	pub track_info_updated: Option<String>,
	pub file_type: Option<i64>,
	/// ストリーミング楽曲のカタログ参照（JSON 文字列）。ローカルファイルの楽曲では `"null"`。
	pub ext_info: Option<String>,
}

impl ContentRow {
	/// ファイルを持たないストリーミング楽曲の `FolderPath`。ローカルファイルの楽曲では `None`。
	///
	/// Rekordbox はストリーミング楽曲の `FolderPath` に `apple-music:tracks:972852653` のような
	/// URI を保持する。2 文字以上の URI スキームで始まる値を URI とみなし、`C:\...` のような
	/// ドライブ文字付きのパスと区別する。
	pub fn stream_uri(&self) -> Option<&str> {
		let path = non_empty(&self.folder_path)?;
		let (scheme, _) = path.split_once(':')?;
		let is_scheme = scheme.len() >= 2
			&& scheme.starts_with(|c: char| c.is_ascii_alphabetic())
			&& scheme
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
		is_scheme.then_some(path)
	}
}

/// `djmdCue` の 1 行（本アプリで使う列）。
#[derive(Debug, Clone, Default)]
pub struct CueRow {
	pub kind: i64,
	pub in_msec: Option<i64>,
	pub out_msec: Option<i64>,
	pub color: Option<i64>,
	pub color_table_index: Option<i64>,
	pub comment: Option<String>,
	pub active_loop: Option<i64>,
	pub beat_loop_size: Option<i64>,
}

const CONTENT_SELECT: &str = "SELECT c.ID, c.UUID, c.FolderPath, c.FileNameL, c.Title, \
    ar.Name, al.Name, g.Name, k.ScaleName, co.Commnt, \
    c.BPM, c.Length, c.Rating, c.Commnt, c.ReleaseYear, c.TrackNo, c.FileSize, \
    c.Analysed, c.SampleRate, c.BitRate, c.BitDepth, \
    c.AnalysisDataPath, c.AnalysisUpdated, c.ImagePath, c.TrackInfoUpdated, \
    c.FileType, c.ExtInfo \
    FROM djmdContent c \
    LEFT JOIN djmdArtist ar ON ar.ID = c.ArtistID \
    LEFT JOIN djmdAlbum al ON al.ID = c.AlbumID \
    LEFT JOIN djmdGenre g ON g.ID = c.GenreID \
    LEFT JOIN djmdKey k ON k.ID = c.KeyID \
    LEFT JOIN djmdColor co ON co.ID = c.ColorID";

/// SQLite の値を型に寄らず読むための補助。
trait RowExt {
	fn text(&self, idx: usize) -> Result<Option<String>>;
	fn int(&self, idx: usize) -> Result<Option<i64>>;
}

impl RowExt for Row<'_> {
	fn text(&self, idx: usize) -> Result<Option<String>> {
		Ok(match self.get::<_, Value>(idx)? {
			Value::Null => None,
			Value::Text(s) => Some(s),
			Value::Integer(i) => Some(i.to_string()),
			Value::Real(f) => Some(f.to_string()),
			Value::Blob(b) => Some(String::from_utf8_lossy(&b).into_owned()),
		})
	}

	fn int(&self, idx: usize) -> Result<Option<i64>> {
		Ok(match self.get::<_, Value>(idx)? {
			Value::Integer(i) => Some(i),
			Value::Real(f) => Some(f as i64),
			Value::Text(s) => s.trim().parse().ok(),
			_ => None,
		})
	}
}

/// 空文字列を `None` とみなす（Python の真偽判定に合わせる）。
pub fn non_empty(v: &Option<String>) -> Option<&str> {
	v.as_deref().filter(|s| !s.is_empty())
}

/// 0 と `None` を偽とみなす。
fn truthy_int(v: Option<i64>) -> bool {
	v.is_some_and(|n| n != 0)
}

/// DB に書く時刻文字列（UTC）。
fn now_str() -> String {
	chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f +00:00").to_string()
}

/// Python の `str(Path)` と同じく、Windows では区切りを `\` にそろえる。
fn native_path_string(path: &Path) -> String {
	let s = path.to_string_lossy().into_owned();
	if cfg!(windows) {
		s.replace('/', "\\")
	} else {
		s
	}
}

/// `rekordbox` という名前のプロセスが動いているか。
fn rekordbox_running() -> bool {
	use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
	let mut sys = System::new();
	sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
	sys.processes()
		.values()
		.any(|p| Path::new(p.name()).file_stem().is_some_and(|s| s == "rekordbox"))
}

/// 楽曲ファイルの拡張子から `djmdContent.FileType` を求める。
fn file_type_of(path: &Path) -> Result<i64> {
	let ext = path
		.extension()
		.map(|e| e.to_string_lossy().to_ascii_uppercase())
		.unwrap_or_default();
	Ok(match ext.as_str() {
		"MP3" => 1,
		"M4A" => 4,
		"FLAC" => 5,
		"WAV" => 11,
		"AIFF" | "AIF" => 12,
		_ => bail!("対応していないファイル形式です: .{}", ext.to_ascii_lowercase()),
	})
}

/// 開いている `master.db` への接続。
pub struct RekordboxDb {
	conn: Connection,
	db_dir: PathBuf,
	share_dir: PathBuf,
	/// 書き込みトランザクション中か。
	in_tx: bool,
	/// 変更の記録（順序どおり）。コミット時に USN の割り当てに使う。
	changes: Vec<(Table, String)>,
	/// 追加したプレイリスト。コミット後に `masterPlaylists6.xml` へ反映する。
	new_playlists: Vec<PlaylistNode>,
	/// コミット前に Rekordbox の起動を確認するか。
	require_closed: bool,
}

impl RekordboxDb {
	/// `master.db` を開く。鍵が合わない場合はエラー。
	pub fn open(path: &Path) -> Result<Self> {
		Self::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
	}

	/// 読み取り専用で開く（検証用）。
	#[cfg(test)]
	pub fn open_read_only(path: &Path) -> Result<Self> {
		Self::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
	}

	fn open_with_flags(path: &Path, flags: OpenFlags) -> Result<Self> {
		if !path.is_file() {
			bail!("ファイルがありません: {}", path.display());
		}
		let flags = flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI;
		let conn = Connection::open_with_flags(path, flags)?;
		conn.pragma_update(None, "key", database_key()?)?;
		conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
			.context("データベースの鍵が合いません")?;
		conn.busy_timeout(std::time::Duration::from_secs(5))?;
		let db_dir = path
			.parent()
			.map(Path::to_path_buf)
			.ok_or_else(|| anyhow!("データベースのフォルダが不明です"))?;
		Ok(Self {
			conn,
			share_dir: db_dir.join("share"),
			db_dir,
			in_tx: false,
			changes: Vec::new(),
			new_playlists: Vec::new(),
			require_closed: true,
		})
	}

	/// コミット前の Rekordbox 起動確認を切り替える（検証用）。
	#[cfg(test)]
	pub fn set_require_closed(&mut self, value: bool) {
		self.require_closed = value;
	}

	// -- 読み取り ---------------------------------------------------------

	/// 全プレイリスト／フォルダ行。
	pub fn playlists(&self) -> Result<Vec<PlaylistRow>> {
		let mut stmt = self
			.conn
			.prepare("SELECT ID, UUID, Name, Attribute, ParentID FROM djmdPlaylist")?;
		let mut rows = stmt.query([])?;
		let mut out = Vec::new();
		while let Some(r) = rows.next()? {
			out.push(Self::playlist_row(r)?);
		}
		Ok(out)
	}

	fn playlist_row(r: &Row) -> Result<PlaylistRow> {
		Ok(PlaylistRow {
			id: r.text(0)?.unwrap_or_default(),
			uuid: r.text(1)?.unwrap_or_default(),
			name: r.text(2)?.unwrap_or_default(),
			attribute: r.int(3)?.unwrap_or(0),
			parent_id: r.text(4)?,
		})
	}

	/// 名前が一致する最初のプレイリスト。
	pub fn find_playlist_by_name(&self, name: &str) -> Result<Option<PlaylistRow>> {
		let mut stmt = self
			.conn
			.prepare("SELECT ID, UUID, Name, Attribute, ParentID FROM djmdPlaylist WHERE Name = ?1 LIMIT 1")?;
		let mut rows = stmt.query([name])?;
		rows.next()?.map(Self::playlist_row).transpose()
	}

	/// プレイリスト ID -> 収録曲の UUID（曲順どおり）。
	pub fn playlist_track_uuids(&self) -> Result<HashMap<String, Vec<String>>> {
		let mut stmt = self.conn.prepare(
			"SELECT sp.PlaylistID, c.UUID FROM djmdSongPlaylist sp \
             JOIN djmdContent c ON c.ID = sp.ContentID \
             ORDER BY sp.PlaylistID, COALESCE(sp.TrackNo, 0), sp.rowid",
		)?;
		let mut rows = stmt.query([])?;
		let mut out: HashMap<String, Vec<String>> = HashMap::new();
		while let Some(r) = rows.next()? {
			let (Some(pid), Some(uuid)) = (r.text(0)?, r.text(1)?) else {
				continue;
			};
			if !uuid.is_empty() {
				out.entry(pid).or_default().push(uuid);
			}
		}
		Ok(out)
	}

	/// プレイリストに既に入っている楽曲の `ContentID` 集合。
	pub fn playlist_content_ids(&self, playlist_id: &str) -> Result<HashSet<String>> {
		let mut stmt = self.conn.prepare(
			"SELECT c.ID FROM djmdSongPlaylist sp JOIN djmdContent c ON c.ID = sp.ContentID \
             WHERE sp.PlaylistID = ?1",
		)?;
		let mut rows = stmt.query([playlist_id])?;
		let mut out = HashSet::new();
		while let Some(r) = rows.next()? {
			if let Some(id) = r.text(0)? {
				out.insert(id);
			}
		}
		Ok(out)
	}

	fn content_row(r: &Row) -> Result<ContentRow> {
		let name = |i: usize| -> Result<Option<String>> { Ok(r.text(i)?.filter(|s| !s.is_empty())) };
		Ok(ContentRow {
			id: r.text(0)?.unwrap_or_default(),
			uuid: r.text(1)?.unwrap_or_default(),
			folder_path: r.text(2)?,
			file_name_l: r.text(3)?,
			title: r.text(4)?,
			artist: name(5)?,
			album: name(6)?,
			genre: name(7)?,
			key: name(8)?,
			color: name(9)?,
			bpm: r.int(10)?,
			length: r.int(11)?,
			rating: r.int(12)?,
			commnt: r.text(13)?,
			release_year: r.int(14)?,
			track_no: r.int(15)?,
			file_size: r.int(16)?,
			analysed: r.int(17)?,
			sample_rate: r.int(18)?,
			bit_rate: r.int(19)?,
			bit_depth: r.int(20)?,
			analysis_data_path: r.text(21)?,
			analysis_updated: r.text(22)?,
			image_path: r.text(23)?,
			track_info_updated: r.text(24)?,
			file_type: r.int(25)?,
			ext_info: r.text(26)?,
		})
	}

	fn find_content(&self, column: &str, value: &str) -> Result<Option<ContentRow>> {
		let sql = format!("{CONTENT_SELECT} WHERE c.{column} = ?1 LIMIT 1");
		let mut stmt = self.conn.prepare(&sql)?;
		let mut rows = stmt.query([value])?;
		rows.next()?.map(Self::content_row).transpose()
	}

	pub fn find_content_by_uuid(&self, uuid: &str) -> Result<Option<ContentRow>> {
		self.find_content("UUID", uuid)
	}

	pub fn find_content_by_id(&self, id: &str) -> Result<Option<ContentRow>> {
		self.find_content("ID", id)
	}

	/// `FolderPath` が一致する楽曲。
	pub fn find_content_by_path(&self, path: &Path) -> Result<Option<ContentRow>> {
		self.find_content("FolderPath", &native_path_string(path))
	}

	/// `FolderPath` が `uri` と一致するストリーミング楽曲。
	///
	/// `uri` はパスではないため、区切り文字を変換する `find_content_by_path` を経由しない。
	pub fn find_content_by_stream_uri(&self, uri: &str) -> Result<Option<ContentRow>> {
		self.find_content("FolderPath", uri)
	}

	/// 楽曲のキュー（メモリーキュー・ホットキュー・ループ）。
	pub fn cues_of(&self, content_id: &str) -> Result<Vec<CueRow>> {
		let mut stmt = self.conn.prepare(
			"SELECT Kind, InMsec, OutMsec, Color, ColorTableIndex, Comment, ActiveLoop, BeatLoopSize \
             FROM djmdCue WHERE ContentID = ?1 ORDER BY rowid",
		)?;
		let mut rows = stmt.query([content_id])?;
		let mut out = Vec::new();
		while let Some(r) = rows.next()? {
			out.push(CueRow {
				kind: r.int(0)?.unwrap_or(0),
				in_msec: r.int(1)?,
				out_msec: r.int(2)?,
				color: r.int(3)?,
				color_table_index: r.int(4)?,
				comment: r.text(5)?,
				active_loop: r.int(6)?,
				beat_loop_size: r.int(7)?,
			});
		}
		Ok(out)
	}

	/// 楽曲に付いている My Tag の名前。
	pub fn my_tag_names_of(&self, content_id: &str) -> Result<Vec<String>> {
		let mut stmt = self.conn.prepare(
			"SELECT t.Name FROM djmdSongMyTag st JOIN djmdMyTag t ON t.ID = st.MyTagID \
             WHERE st.ContentID = ?1 ORDER BY st.rowid",
		)?;
		let mut rows = stmt.query([content_id])?;
		let mut out = Vec::new();
		while let Some(r) = rows.next()? {
			if let Some(name) = r.text(0)?.filter(|s| !s.is_empty()) {
				out.push(name);
			}
		}
		Ok(out)
	}

	/// `share` 相対パス（先頭の `/` 付き）を実パスにする。
	pub fn share_path(&self, rel: &str) -> PathBuf {
		let rel = rel.trim_start_matches(['/', '\\']).replace('\\', "/");
		let mut p = self.share_dir.clone();
		for part in rel.split('/').filter(|s| !s.is_empty()) {
			p.push(part);
		}
		p
	}

	// -- 書き込み ---------------------------------------------------------

	/// 書き込みトランザクションを始める。
	pub fn begin(&mut self) -> Result<()> {
		if !self.in_tx {
			self.conn.execute_batch("BEGIN")?;
			self.in_tx = true;
			self.changes.clear();
			self.new_playlists.clear();
		}
		Ok(())
	}

	/// 書き込みを取り消す。
	pub fn rollback(&mut self) {
		if self.in_tx {
			let _ = self.conn.execute_batch("ROLLBACK");
			self.in_tx = false;
		}
		self.changes.clear();
		self.new_playlists.clear();
	}

	/// 変更を確定する。Rekordbox が起動中なら確定せずエラーを返す（トランザクションは継続）。
	pub fn commit(&mut self) -> Result<()> {
		if !self.in_tx {
			return Ok(());
		}
		if self.require_closed && rekordbox_running() {
			bail!("Rekordbox が起動中です。Rekordbox を終了してから再試行してください。");
		}

		// 変更 1 回ごとに USN を 1 進め、行には最後の変更時の番号を入れる。
		let mut usn: i64 = self
			.conn
			.query_row(
				"SELECT int_1 FROM agentRegistry WHERE registry_id = 'localUpdateCount'",
				[],
				|r| r.get::<_, Option<i64>>(0),
			)
			.optional()?
			.flatten()
			.ok_or_else(|| anyhow!("agentRegistry に localUpdateCount がありません"))?;
		let mut last: HashMap<(Table, String), i64> = HashMap::new();
		for (table, id) in &self.changes {
			usn += 1;
			last.insert((*table, id.clone()), usn);
		}
		for ((table, id), row_usn) in &last {
			let sql = format!("UPDATE {} SET rb_local_usn = ?1 WHERE ID = ?2", table.name());
			self.conn.execute(&sql, params![row_usn, id])?;
		}
		if !self.changes.is_empty() {
			self.conn.execute(
				"UPDATE agentRegistry SET int_1 = ?1, updated_at = ?2 WHERE registry_id = 'localUpdateCount'",
				params![usn, now_str()],
			)?;
		}
		self.conn.execute_batch("COMMIT")?;
		self.in_tx = false;
		self.changes.clear();

		let nodes = std::mem::take(&mut self.new_playlists);
		playlist_xml::append_to_file(&self.db_dir, &nodes).context("masterPlaylists6.xml を更新できませんでした")?;
		Ok(())
	}

	fn ensure_tx(&self) -> Result<()> {
		if !self.in_tx {
			bail!("書き込みトランザクションが開始されていません");
		}
		Ok(())
	}

	/// 1 行追加する。`StatsFull` の既定列（状態フラグ・作成/更新時刻）は自動で埋める。
	fn insert(&mut self, table: Table, id: &str, cols: Vec<(&'static str, Value)>) -> Result<()> {
		self.ensure_tx()?;
		let now = now_str();
		let mut names: Vec<&str> = vec!["ID"];
		let mut values: Vec<Value> = vec![Value::Text(id.to_string())];
		for (name, value) in cols {
			names.push(name);
			values.push(value);
		}
		for (name, value) in [
			("rb_data_status", Value::Integer(0)),
			("rb_local_data_status", Value::Integer(0)),
			("rb_local_deleted", Value::Integer(0)),
			("rb_local_synced", Value::Integer(0)),
			("created_at", Value::Text(now.clone())),
			("updated_at", Value::Text(now)),
		] {
			if !names.contains(&name) {
				names.push(name);
				values.push(value);
			}
		}
		let placeholders = (1..=names.len())
			.map(|i| format!("?{i}"))
			.collect::<Vec<_>>()
			.join(", ");
		let sql = format!(
			"INSERT INTO {} ({}) VALUES ({placeholders})",
			table.name(),
			names.join(", ")
		);
		self.conn.execute(&sql, params_from_iter(values))?;
		self.changes.push((table, id.to_string()));
		Ok(())
	}

	/// 列を更新する。pyrekordbox と同じく、列 1 つにつき変更 1 回として記録する。
	fn update(&mut self, table: Table, id: &str, cols: Vec<(&'static str, Value)>) -> Result<()> {
		self.ensure_tx()?;
		if cols.is_empty() {
			return Ok(());
		}
		let mut sets = Vec::new();
		let mut values = Vec::new();
		for (i, (name, value)) in cols.iter().enumerate() {
			sets.push(format!("{name} = ?{}", i + 1));
			values.push(value.clone());
		}
		values.push(Value::Text(now_str()));
		sets.push(format!("updated_at = ?{}", values.len()));
		values.push(Value::Text(id.to_string()));
		let sql = format!(
			"UPDATE {} SET {} WHERE ID = ?{}",
			table.name(),
			sets.join(", "),
			values.len()
		);
		self.conn.execute(&sql, params_from_iter(values))?;
		for _ in &cols {
			self.changes.push((table, id.to_string()));
		}
		Ok(())
	}

	/// 未使用の数値 ID（28 bit の乱数、100 以上）を作る。
	fn generate_unused_id(&self, table: Table, column: &str) -> Result<String> {
		let sql = format!("SELECT 1 FROM {} WHERE {column} = ?1 LIMIT 1", table.name());
		let mut stmt = self.conn.prepare(&sql)?;
		for _ in 0..1_000_000 {
			let id = rand::random::<u32>() >> 4;
			if id < 100 {
				continue;
			}
			let id = id.to_string();
			if !stmt.exists([&id])? {
				return Ok(id);
			}
		}
		bail!("未使用の ID を作れませんでした")
	}

	fn uuid4() -> String {
		uuid::Uuid::new_v4().to_string()
	}

	/// 楽曲を追加する（pyrekordbox `add_content` 相当）。`extra` は追加で設定する列。
	pub fn add_content(&mut self, path: &Path, extra: Vec<(&'static str, Value)>) -> Result<String> {
		let path_string = native_path_string(path);
		let file_type = file_type_of(path)?;
		let file_name = path
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_default();
		let file_size = std::fs::metadata(path)
			.with_context(|| format!("楽曲ファイルを読めません: {}", path.display()))?
			.len() as i64;
		let file_id = self.generate_unused_id(Table::Content, "rb_file_id")?;
		self.insert_content(path_string, file_name, file_size, file_type, file_id, extra)
	}

	/// ファイルを持たないストリーミング楽曲を追加する。
	///
	/// Rekordbox が登録するストリーミング楽曲の行と同じく、`FolderPath` と `FileNameL` に
	/// `uri` を、`FileSize` と `rb_file_id` に 0 を設定する。`FileType` と `ExtInfo` は
	/// 再生時のサービス判定とカタログ参照に使われるため、元の行の値を `extra` で渡す。
	pub fn add_stream_content(
		&mut self,
		uri: &str,
		file_type: i64,
		extra: Vec<(&'static str, Value)>,
	) -> Result<String> {
		self.insert_content(uri.to_string(), uri.to_string(), 0, file_type, "0".into(), extra)
	}

	fn insert_content(
		&mut self,
		folder_path: String,
		file_name: String,
		file_size: i64,
		file_type: i64,
		file_id: String,
		extra: Vec<(&'static str, Value)>,
	) -> Result<String> {
		let exists: bool = self.conn.query_row(
			"SELECT EXISTS(SELECT 1 FROM djmdContent WHERE FolderPath = ?1)",
			[&folder_path],
			|r| r.get(0),
		)?;
		if exists {
			bail!("同じパスの楽曲が既に登録されています: {folder_path}");
		}

		let id = self.generate_unused_id(Table::Content, "ID")?;
		let content_link: Option<i64> = self
			.conn
			.query_row("SELECT rb_local_usn FROM djmdMenuItems WHERE Name = 'TRACK'", [], |r| {
				r.get(0)
			})
			.context("djmdMenuItems に TRACK がありません")?;
		let (device_id, master_db_id): (Option<String>, Option<String>) = self
			.conn
			.query_row("SELECT ID, MasterDBID FROM djmdDevice LIMIT 1", [], |r| {
				Ok((
					r.get::<_, Value>(0).map(value_to_text)?,
					r.get::<_, Value>(1).map(value_to_text)?,
				))
			})
			.context("djmdDevice が空です")?;
		let today = chrono::Local::now().format("%Y-%m-%d").to_string();

		let opt = |v: Option<String>| v.map_or(Value::Null, Value::Text);
		let mut cols: Vec<(&'static str, Value)> = vec![
			("UUID", Value::Text(Self::uuid4())),
			("ContentLink", content_link.map_or(Value::Null, Value::Integer)),
			("DateCreated", Value::Text(today.clone())),
			("DeviceID", opt(device_id)),
			("FileNameL", Value::Text(file_name)),
			("FileSize", Value::Integer(file_size)),
			("FileType", Value::Integer(file_type)),
			("FolderPath", Value::Text(folder_path)),
			("HotCueAutoLoad", Value::Text("on".into())),
			("MasterDBID", opt(master_db_id)),
			("MasterSongID", Value::Text(id.clone())),
			("StockDate", Value::Text(today)),
			("rb_file_id", Value::Text(file_id)),
		];
		for (name, value) in extra {
			if let Some(slot) = cols.iter_mut().find(|(n, _)| *n == name) {
				slot.1 = value;
			} else {
				cols.push((name, value));
			}
		}
		self.insert(Table::Content, &id, cols)?;
		Ok(id)
	}

	/// 楽曲の列を更新する。
	pub fn update_content(&mut self, id: &str, cols: Vec<(&'static str, Value)>) -> Result<()> {
		self.update(Table::Content, id, cols)
	}

	/// 名前が一致する行の ID を返し、無ければ追加する（アーティスト／ジャンル／アルバム）。
	pub fn get_or_add_named(&mut self, table: Table, name: &str) -> Result<String> {
		if !matches!(table, Table::Artist | Table::Genre | Table::Album) {
			bail!("{} は名前で追加できません", table.name());
		}
		let sql = format!("SELECT ID FROM {} WHERE Name = ?1 LIMIT 1", table.name());
		let found = self
			.conn
			.query_row(&sql, [name], |r| r.get::<_, Value>(0))
			.optional()?
			.and_then(value_to_text);
		if let Some(id) = found {
			return Ok(id);
		}
		let id = self.generate_unused_id(table, "ID")?;
		self.insert(
			table,
			&id,
			vec![
				("Name", Value::Text(name.to_string())),
				("UUID", Value::Text(Self::uuid4())),
			],
		)?;
		Ok(id)
	}

	/// キューを 1 件追加する。
	pub fn add_cue(&mut self, content: &ContentRow, cue: &CueRow) -> Result<()> {
		let id = self.generate_unused_id(Table::Cue, "ID")?;
		let int = |v: Option<i64>| v.map_or(Value::Null, Value::Integer);
		self.insert(
			Table::Cue,
			&id,
			vec![
				("ContentID", Value::Text(content.id.clone())),
				("InMsec", int(cue.in_msec)),
				("OutMsec", int(cue.out_msec)),
				("Kind", Value::Integer(cue.kind)),
				("Color", int(cue.color)),
				("ColorTableIndex", int(cue.color_table_index)),
				("Comment", cue.comment.clone().map_or(Value::Null, Value::Text)),
				("ActiveLoop", int(cue.active_loop)),
				("BeatLoopSize", int(cue.beat_loop_size)),
				("ContentUUID", Value::Text(content.uuid.clone())),
			],
		)
	}

	/// My Tag を名前で探し（無ければ作り）、楽曲に付ける。
	pub fn add_my_tag(&mut self, content_id: &str, name: &str) -> Result<()> {
		let found = self
			.conn
			.query_row("SELECT ID FROM djmdMyTag WHERE Name = ?1 LIMIT 1", [name], |r| {
				r.get::<_, Value>(0)
			})
			.optional()?
			.and_then(value_to_text);
		let tag_id = match found {
			Some(id) => id,
			None => {
				let id = self.generate_unused_id(Table::MyTag, "ID")?;
				self.insert(
					Table::MyTag,
					&id,
					vec![
						("Name", Value::Text(name.to_string())),
						("Attribute", Value::Integer(0)),
					],
				)?;
				id
			}
		};
		let link_id = self.generate_unused_id(Table::SongMyTag, "ID")?;
		self.insert(
			Table::SongMyTag,
			&link_id,
			vec![
				("MyTagID", Value::Text(tag_id)),
				("ContentID", Value::Text(content_id.to_string())),
			],
		)
	}

	/// ルート直下の末尾に通常のプレイリストを作る（pyrekordbox `create_playlist` 相当）。
	pub fn create_playlist(&mut self, name: &str) -> Result<PlaylistRow> {
		let id = self.generate_unused_id(Table::Playlist, "ID")?;
		let uuid = Self::uuid4();
		let parent_id = "root";
		let count: i64 = self.conn.query_row(
			"SELECT count(*) FROM djmdPlaylist WHERE ParentID = ?1",
			[parent_id],
			|r| r.get(0),
		)?;
		// pyrekordbox と同じく、仮の名前で作ってから名前を設定する（USN を Rekordbox に合わせるため）。
		self.insert(
			Table::Playlist,
			&id,
			vec![
				("Seq", Value::Integer(count + 1)),
				("Name", Value::Text("New playlist".into())),
				("Attribute", Value::Integer(0)),
				("ParentID", Value::Text(parent_id.into())),
				("UUID", Value::Text(uuid.clone())),
			],
		)?;
		self.update(Table::Playlist, &id, vec![("Name", Value::Text(name.to_string()))])?;
		self.new_playlists.push(PlaylistNode {
			id: id.clone(),
			parent_id: parent_id.into(),
			attribute: 0,
			timestamp_ms: chrono::Utc::now().timestamp_millis(),
		});
		Ok(PlaylistRow {
			id,
			uuid,
			name: name.to_string(),
			attribute: 0,
			parent_id: Some(parent_id.into()),
		})
	}

	/// プレイリストの末尾に楽曲を追加する（pyrekordbox `add_to_playlist` 相当）。
	pub fn add_to_playlist(&mut self, playlist: &PlaylistRow, content_id: &str) -> Result<()> {
		if playlist.attribute != 0 {
			bail!("通常のプレイリストではありません: {}", playlist.name);
		}
		let count: i64 = self.conn.query_row(
			"SELECT count(*) FROM djmdSongPlaylist WHERE PlaylistID = ?1",
			[&playlist.id],
			|r| r.get(0),
		)?;
		let id = Self::uuid4();
		self.insert(
			Table::SongPlaylist,
			&id,
			vec![
				("PlaylistID", Value::Text(playlist.id.clone())),
				("ContentID", Value::Text(content_id.to_string())),
				("TrackNo", Value::Integer(count + 1)),
				("UUID", Value::Text(Self::uuid4())),
			],
		)
	}

	/// 解析状態の列を、解析済みとして扱われる値にそろえる。
	///
	/// - `Analysed`: 元の値（105/233 等）を引き継ぐ。無ければ 105
	/// - `AnalysisUpdated`: NULL だと再解析の引き金になるため "1"
	pub fn mark_analysed(
		&mut self,
		content: &ContentRow,
		analysis_data_path: String,
		analysed: Option<i64>,
	) -> Result<()> {
		let mut cols = vec![("AnalysisDataPath", Value::Text(analysis_data_path))];
		if let Some(a) = analysed.filter(|&a| a != 0) {
			cols.push(("Analysed", Value::Integer(a)));
		} else if !truthy_int(content.analysed) {
			cols.push(("Analysed", Value::Integer(105)));
		}
		if non_empty(&content.analysis_updated).is_none() {
			cols.push(("AnalysisUpdated", Value::Text("1".into())));
		}
		self.update(Table::Content, &content.id, cols)
	}
}

fn value_to_text(v: Value) -> Option<String> {
	match v {
		Value::Null => None,
		Value::Text(s) => Some(s),
		Value::Integer(i) => Some(i.to_string()),
		Value::Real(f) => Some(f.to_string()),
		Value::Blob(b) => Some(String::from_utf8_lossy(&b).into_owned()),
	}
}

impl Drop for RekordboxDb {
	fn drop(&mut self) {
		self.rollback();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn row(folder_path: &str) -> ContentRow {
		ContentRow {
			folder_path: Some(folder_path.to_string()),
			..Default::default()
		}
	}

	#[test]
	fn stream_uri_detects_uri_scheme() {
		assert_eq!(
			row("apple-music:tracks:972852653").stream_uri(),
			Some("apple-music:tracks:972852653")
		);
		assert_eq!(row(r"C:\Users\a\Music\x.mp3").stream_uri(), None);
		assert_eq!(row("C:/Users/a/Music/x.mp3").stream_uri(), None);
		assert_eq!(row("/Users/a/Music/x:y.mp3").stream_uri(), None);
		assert_eq!(row("").stream_uri(), None);
		assert_eq!(ContentRow::default().stream_uri(), None);
	}
}
