//! `.rsync` パッケージからローカル Rekordbox DB への取り込み・同期。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::types::Value;
use serde::Serialize;

use crate::config;
use crate::models::{PlaylistData, StreamData, TrackData};
use crate::package::PackageReader;
use crate::rekordbox::{anlz, non_empty, ContentRow, CueRow, PlaylistRow, RekordboxDb, Table};
use crate::task::Task;

/// 取り込みの挙動を制御するオプション。
#[derive(Debug, Clone)]
pub struct ImportOptions {
	/// 楽曲を展開するフォルダ。
	pub extract_dir: PathBuf,
	/// `merge` / `rename` / `skip`。
	pub conflict_policy: String,
	pub import_cues: bool,
	pub import_tags: bool,
	/// 解析データ（波形・ビートグリッド）も取り込む。
	pub import_analysis: bool,
	/// アートワーク画像も取り込む。
	pub import_artwork: bool,
}

/// 取り込み結果のサマリ。
#[derive(Debug, Default, Serialize)]
pub struct ImportResult {
	pub tracks_added: usize,
	pub tracks_reused: usize,
	pub tracks_failed: usize,
	pub playlists_created: usize,
	pub playlists_merged: usize,
	pub playlists_skipped: usize,
	pub analysis_imported: usize,
	pub artwork_imported: usize,
	pub warnings: Vec<String>,
}

impl ImportResult {
	pub fn summary(&self) -> String {
		let mut lines = vec![
			format!(
				"楽曲: 追加 {} / 既存再利用 {} / 失敗 {}",
				self.tracks_added, self.tracks_reused, self.tracks_failed
			),
			format!(
				"プレイリスト: 新規 {} / 統合 {} / スキップ {}",
				self.playlists_created, self.playlists_merged, self.playlists_skipped
			),
		];
		if self.analysis_imported > 0 {
			lines.push(format!("解析データ: {} 曲分を取り込み", self.analysis_imported));
		}
		if self.artwork_imported > 0 {
			lines.push(format!("アートワーク: {} 曲分を取り込み", self.artwork_imported));
		}
		if !self.warnings.is_empty() {
			lines.push(format!("警告 {} 件", self.warnings.len()));
		}
		lines.join("\n")
	}
}

/// パッケージを DB に取り込まずに中身（プレイリスト・楽曲一覧）を覗く。
pub fn inspect_package(path: &Path) -> Result<(Vec<PlaylistData>, Vec<TrackData>)> {
	let reader = PackageReader::open(path)?;
	Ok((reader.package.playlists, reader.package.tracks))
}

/// 一時フォルダ。破棄時に中身ごと削除する。
struct TempDir(PathBuf);

impl TempDir {
	fn new(prefix: &str) -> Result<Self> {
		let dir = std::env::temp_dir().join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()));
		std::fs::create_dir_all(&dir)?;
		Ok(Self(dir))
	}
}

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// パッケージを DB に取り込む。`selected` は取り込むプレイリストの UUID（`None` なら全て）。
///
/// 途中でエラーになった場合は DB への変更を取り消す。1 曲単位の失敗は警告に留めて続行する。
pub fn import_package(
	db: &mut RekordboxDb,
	package_path: &Path,
	options: &ImportOptions,
	selected: Option<&HashSet<String>>,
	task: &Task,
) -> Result<ImportResult> {
	db.begin()?;
	let result = import_inner(db, package_path, options, selected, task);
	if result.is_err() {
		db.rollback();
	}
	result
}

fn import_inner(
	db: &mut RekordboxDb,
	package_path: &Path,
	options: &ImportOptions,
	selected: Option<&HashSet<String>>,
	task: &Task,
) -> Result<ImportResult> {
	let mut result = ImportResult::default();
	let mut reader = PackageReader::open(package_path)?;

	let target_playlists: Vec<PlaylistData> = reader
		.package
		.playlists
		.iter()
		.filter(|p| !p.is_folder() && selected.map_or(true, |s| s.contains(&p.uuid)))
		.cloned()
		.collect();

	// 取り込むプレイリストに含まれる楽曲だけを対象にする。
	let needed: HashSet<&str> = target_playlists
		.iter()
		.flat_map(|p| p.track_uuids.iter().map(String::as_str))
		.collect();
	let tracks: Vec<TrackData> = reader
		.package
		.tracks
		.iter()
		.filter(|t| needed.contains(t.uuid.as_str()))
		.cloned()
		.collect();

	// --- 楽曲を展開して DB へ確保し、UUID -> content ID をマップ ---------
	let mut uuid_to_content: HashMap<String, String> = HashMap::new();
	let total = tracks.len();
	for (i, track) in tracks.iter().enumerate() {
		task.progress(&format!("楽曲を取り込み中: {}", track.file_name), i + 1, total);
		if let Err(e) = import_track(db, &mut reader, track, options, &mut uuid_to_content, &mut result) {
			// 1 曲の失敗で全体を止めない。
			result.tracks_failed += 1;
			result.warnings.push(format!("{}: {e}", track.file_name));
		}
	}

	// --- プレイリストを作成／統合 --------------------------------------
	for pl in &target_playlists {
		task.progress(&format!("プレイリストを反映中: {}", pl.name), 0, 0);
		apply_playlist(db, pl, &uuid_to_content, options, &mut result)?;
	}

	db.commit()?;
	task.progress("取り込み完了", total, total);
	Ok(result)
}

fn import_track(
	db: &mut RekordboxDb,
	reader: &mut PackageReader,
	track: &TrackData,
	options: &ImportOptions,
	uuid_to_content: &mut HashMap<String, String>,
	result: &mut ImportResult,
) -> Result<()> {
	let extracted;
	let source = match track.stream.as_ref().filter(|s| !s.uri.is_empty()) {
		Some(stream) => TrackSource::Stream(stream),
		None => {
			let Some(path) = reader.extract_track(track, &options.extract_dir)? else {
				result.tracks_failed += 1;
				result
					.warnings
					.push(format!("ファイル欠落のため取り込めません: {}", track.file_name));
				return Ok(());
			};
			extracted = path;
			TrackSource::File(&extracted)
		}
	};

	let by_location = match source {
		TrackSource::File(path) => db.find_content_by_path(path)?,
		TrackSource::Stream(stream) => db.find_content_by_stream_uri(&stream.uri)?,
	};
	// UUID が一致してもファイルとストリーミングの種別が異なる行は別の楽曲として扱う。
	// 再利用すると、取り込む解析データの PPTH と行の `FolderPath` の種別が食い違うため。
	let is_stream = matches!(source, TrackSource::Stream(_));
	let existing = match by_location {
		Some(c) => Some(c),
		None => db
			.find_content_by_uuid(&track.uuid)?
			.filter(|c| c.stream_uri().is_some() == is_stream),
	};
	let content = match existing {
		Some(c) => {
			result.tracks_reused += 1;
			c
		}
		None => {
			let id = add_track(db, source, track, options)?;
			result.tracks_added += 1;
			db.find_content_by_id(&id)?
				.ok_or_else(|| anyhow::anyhow!("追加した楽曲を読み直せません"))?
		}
	};
	uuid_to_content.insert(track.uuid.clone(), content.id.clone());

	// 解析データ（ANLZ）の取り込み。新規曲、および「まだ解析データを持っていない既存曲」に
	// 適用する。既に解析済みの既存曲は元の解析を尊重してスキップ（上書きしない）。
	if options.import_analysis && !track.analysis.is_empty() && non_empty(&content.analysis_data_path).is_none() {
		let outcome = (|| -> Result<bool> {
			let tmp = TempDir::new("rsync_anlz_")?;
			let files = reader.extract_analysis(track, &tmp.0)?;
			if files.is_empty() {
				return Ok(false);
			}
			import_analysis(db, &content, &files, source, track.analysed)
		})();
		match outcome {
			Ok(true) => result.analysis_imported += 1,
			Ok(false) => {}
			Err(e) => result
				.warnings
				.push(format!("{}: 解析データの取り込み失敗: {e}", track.file_name)),
		}
	}

	// アートワーク画像の取り込み。
	if options.import_artwork && track.artwork.as_deref().is_some_and(|a| !a.is_empty()) {
		if non_empty(&content.image_path).is_none() {
			// まだ画像を持っていない楽曲へ新規に取り込む。
			let outcome = (|| -> Result<bool> {
				let tmp = TempDir::new("rsync_art_")?;
				let Some(art) = reader.extract_artwork(track, &tmp.0)? else {
					return Ok(false);
				};
				import_artwork(db, &content, &art)
			})();
			match outcome {
				Ok(true) => result.artwork_imported += 1,
				Ok(false) => {}
				Err(e) => result
					.warnings
					.push(format!("{}: アートワークの取り込み失敗: {e}", track.file_name)),
			}
		} else if non_empty(&content.track_info_updated).is_none() {
			// 既に画像はあるが Rekordbox の再読込フラグが無い場合はフラグだけ立て直し、
			// 次回起動時にサムネイルを再生成させる。
			db.update_content(&content.id, vec![("TrackInfoUpdated", Value::Text("1".into()))])?;
		}
	}
	Ok(())
}

/// 取り込み先 DB での楽曲の実体の場所。
#[derive(Clone, Copy)]
enum TrackSource<'a> {
	/// 展開先フォルダに書き出した楽曲ファイル。
	File(&'a Path),
	/// 配信サービス上の楽曲。ファイルは展開しない。
	Stream(&'a StreamData),
}

/// 楽曲を DB に追加し、メタデータ・キュー・タグを設定する。
fn add_track(db: &mut RekordboxDb, source: TrackSource, track: &TrackData, options: &ImportOptions) -> Result<String> {
	let int = |v: Option<i64>| v.map(Value::Integer);
	let mut extra: Vec<(&'static str, Value)> = Vec::new();
	if let Some(t) = track.title.as_deref().filter(|t| !t.is_empty()) {
		extra.push(("Title", Value::Text(t.to_string())));
	}
	if let Some(c) = track.comment.as_deref().filter(|c| !c.is_empty()) {
		extra.push(("Commnt", Value::Text(c.to_string())));
	}
	// 音声スペックは解析で確定する値。欠落していると Rekordbox が値を埋めるために
	// 自動解析を始めてしまうため、判明していれば設定する。
	for (name, value) in [
		("BPM", int(track.bpm)),
		("Length", int(track.length)),
		("Rating", int(track.rating)),
		("ReleaseYear", int(track.release_year)),
		("TrackNo", int(track.track_no)),
		("SampleRate", int(track.sample_rate)),
		("BitRate", int(track.bit_rate)),
		("BitDepth", int(track.bit_depth)),
	] {
		if let Some(v) = value {
			extra.push((name, v));
		}
	}
	let id = match source {
		TrackSource::File(path) => db.add_content(path, extra)?,
		TrackSource::Stream(stream) => {
			if let Some(ext) = stream.ext_info.as_deref().filter(|e| !e.is_empty()) {
				extra.push(("ExtInfo", Value::Text(ext.to_string())));
			}
			db.add_stream_content(&stream.uri, stream.file_type, extra)?
		}
	};

	// アーティスト／ジャンル／アルバムを名前から解決して紐づける。
	// 失敗は致命ではないので、以降の紐づけを諦めるだけにする（Python 版と同じ）。
	let _ = (|| -> Result<()> {
		for (table, column, name) in [
			(Table::Artist, "ArtistID", &track.artist),
			(Table::Genre, "GenreID", &track.genre),
			(Table::Album, "AlbumID", &track.album),
		] {
			if let Some(name) = name.as_deref().filter(|n| !n.is_empty()) {
				let rel_id = db.get_or_add_named(table, name)?;
				db.update_content(&id, vec![(column, Value::Text(rel_id))])?;
			}
		}
		Ok(())
	})();

	let content = db
		.find_content_by_id(&id)?
		.ok_or_else(|| anyhow::anyhow!("追加した楽曲を読み直せません"))?;
	if options.import_cues {
		for cue in &track.cues {
			let row = CueRow {
				kind: cue.kind,
				in_msec: cue.in_msec,
				out_msec: cue.out_msec,
				color: cue.color,
				color_table_index: cue.color_table_index,
				comment: cue.comment.clone(),
				active_loop: cue.active_loop,
				beat_loop_size: cue.beat_loop_size,
			};
			let _ = db.add_cue(&content, &row);
		}
	}
	if options.import_tags {
		for name in &track.my_tags {
			let _ = db.add_my_tag(&id, name);
		}
	}
	Ok(id)
}

/// `share/PIONEER/<kind>/<UUID先頭3文字>/<UUID残り>` の新しい一意なフォルダを作る。
/// 戻り値は（`/` 区切りの相対パス, 実パス）。
fn new_share_dir(db: &RekordboxDb, kind: &str) -> Result<(String, PathBuf)> {
	let u = uuid::Uuid::new_v4().to_string();
	let rel = format!("PIONEER/{kind}/{}/{}", &u[..3], &u[3..]);
	let abs = db.share_path(&rel);
	std::fs::create_dir_all(&abs)?;
	Ok((rel, abs))
}

/// 解析（ANLZ）ファイルをローカルの share フォルダへ配置し、楽曲に紐づける。
///
/// `DAT` が無い場合は `AnalysisDataPath` を作れないため何もせず `false` を返す。
/// ファイルの楽曲は、ANLZ 内の楽曲パス（PPTH）も新しい楽曲の場所に書き換える。
fn import_analysis(
	db: &mut RekordboxDb,
	content: &ContentRow,
	files: &BTreeMap<String, PathBuf>,
	source: TrackSource,
	analysed: Option<i64>,
) -> Result<bool> {
	if !files.get("DAT").is_some_and(|p| p.is_file()) {
		return Ok(false);
	}
	// Rekordbox の慣習に合わせ、1 つの UUID を先頭 3 文字とその残りで分割したフォルダに置く。
	// 例: ae794e13-... -> USBANLZ/ae7/94e13-115a-4370-90a2-f2794e9d6017/
	let (rel_dir, abs_dir) = new_share_dir(db, "USBANLZ")?;
	for (kind, src) in files {
		let Some(name) = config::anlz_file_name(kind) else {
			continue;
		};
		if src.is_file() {
			std::fs::copy(src, abs_dir.join(name))?;
		}
	}

	let dat_name = config::anlz_file_name("DAT").unwrap();
	db.mark_analysed(content, format!("/{rel_dir}/{dat_name}"), analysed)?;

	// ストリーミング楽曲の PPTH は環境に依存しない URI（例: `?/apple-music:tracks:<ID>`）のため、
	// 書き出し元の値を保持する。
	let TrackSource::File(audio_path) = source else {
		return Ok(true);
	};
	// ANLZ 内の楽曲パス（PPTH）を、取り込み先の実ファイルに合わせて更新する。
	// 失敗しても解析データ自体は有効なので致命ではない。
	let new_audio = audio_path.to_string_lossy().replace('\\', "/");
	for path in anlz::anlz_paths(&abs_dir).values() {
		let _ = std::fs::read(path)
			.map_err(anyhow::Error::from)
			.and_then(|data| anlz::set_ppth(&data, &new_audio))
			.and_then(|data| std::fs::write(path, data).map_err(anyhow::Error::from));
	}
	Ok(true)
}

/// アートワーク画像を share フォルダへ配置し、楽曲に紐づける。
fn import_artwork(db: &mut RekordboxDb, content: &ContentRow, artwork_file: &Path) -> Result<bool> {
	if !artwork_file.is_file() {
		return Ok(false);
	}
	let (rel_dir, abs_dir) = new_share_dir(db, "Artwork")?;
	std::fs::copy(artwork_file, abs_dir.join(config::ARTWORK_FILE_NAME))?;

	let mut cols = vec![(
		"ImagePath",
		Value::Text(format!("/{rel_dir}/{}", config::ARTWORK_FILE_NAME)),
	)];
	// ImagePath / jpg を入れただけでは Rekordbox がサムネイルのキャッシュを作り直さず、
	// 絵が表示されない。"Reload Tag" 実行時と同じく TrackInfoUpdated に "1" を入れておくと、
	// 次回起動時にアートワークのサムネイルが再生成される。
	if non_empty(&content.track_info_updated).is_none() {
		cols.push(("TrackInfoUpdated", Value::Text("1".into())));
	}
	db.update_content(&content.id, cols)?;
	Ok(true)
}

/// `base (2)` のように衝突しない名前を返す。
fn unique_playlist_name(db: &RekordboxDb, base: &str) -> Result<String> {
	if db.find_playlist_by_name(base)?.is_none() {
		return Ok(base.to_string());
	}
	let mut i = 2;
	loop {
		let candidate = format!("{base} ({i})");
		if db.find_playlist_by_name(&candidate)?.is_none() {
			return Ok(candidate);
		}
		i += 1;
	}
}

fn apply_playlist(
	db: &mut RekordboxDb,
	pl: &PlaylistData,
	uuid_to_content: &HashMap<String, String>,
	options: &ImportOptions,
	result: &mut ImportResult,
) -> Result<()> {
	let target: PlaylistRow = match db.find_playlist_by_name(&pl.name)? {
		Some(existing) => match options.conflict_policy.as_str() {
			config::CONFLICT_SKIP => {
				result.playlists_skipped += 1;
				return Ok(());
			}
			config::CONFLICT_RENAME => {
				let name = unique_playlist_name(db, &pl.name)?;
				result.playlists_created += 1;
				db.create_playlist(&name)?
			}
			_ => {
				// "merge"（既定）
				result.playlists_merged += 1;
				existing
			}
		},
		None => {
			result.playlists_created += 1;
			db.create_playlist(&pl.name)?
		}
	};

	let mut existing_ids = db.playlist_content_ids(&target.id)?;
	for uuid in &pl.track_uuids {
		let Some(content_id) = uuid_to_content.get(uuid) else {
			continue;
		};
		if existing_ids.contains(content_id) {
			continue; // 統合時の重複追加を防ぐ
		}
		match db.add_to_playlist(&target, content_id) {
			Ok(()) => {
				existing_ids.insert(content_id.clone());
			}
			Err(e) => result.warnings.push(format!("{} への追加失敗: {e}", pl.name)),
		}
	}
	Ok(())
}
