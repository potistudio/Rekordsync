//! ローカル Rekordbox DB から `.rsync` パッケージへの書き出し。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::models::{CueData, Package, PlaylistData, StreamData, TrackData};
use crate::package::{write_package, AnalysisPaths};
use crate::rekordbox::{anlz, non_empty, ContentRow, PlaylistRow, RekordboxDb};
use crate::task::Task;

/// GUI のツリー表示用のプレイリスト 1 件分。
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistTreeRow {
	pub uuid: String,
	pub name: String,
	pub parent: Option<String>,
	pub is_folder: bool,
	pub track_count: usize,
}

/// DB のプレイリスト行を `PlaylistData` に変換する（親フォルダ名と収録曲付き）。
fn playlists_to_data(db: &RekordboxDb, rows: &[PlaylistRow]) -> Result<Vec<PlaylistData>> {
	let names: HashMap<&str, &str> = rows.iter().map(|r| (r.id.as_str(), r.name.as_str())).collect();
	let mut songs = db.playlist_track_uuids()?;
	Ok(rows
		.iter()
		.map(|pl| {
			let parent = pl
				.parent_id
				.as_deref()
				.and_then(|pid| names.get(pid))
				.filter(|n| !n.is_empty())
				.map(|n| n.to_string());
			// フォルダ以外なら収録曲を辿る。
			let track_uuids = if pl.attribute != 1 {
				songs.remove(&pl.id).unwrap_or_default()
			} else {
				Vec::new()
			};
			PlaylistData {
				name: pl.name.clone(),
				uuid: pl.uuid.clone(),
				attribute: pl.attribute,
				parent,
				track_uuids,
			}
		})
		.collect())
}

/// GUI のツリー表示用に、プレイリスト一覧を整形して返す。
pub fn collect_playlist_tree(db: &RekordboxDb) -> Result<Vec<PlaylistTreeRow>> {
	let rows = db.playlists()?;
	Ok(playlists_to_data(db, &rows)?
		.into_iter()
		.map(|p| PlaylistTreeRow {
			is_folder: p.is_folder(),
			track_count: p.track_uuids.len(),
			uuid: p.uuid,
			name: p.name,
			parent: p.parent,
		})
		.collect())
}

/// 楽曲行を `TrackData` に変換する（キュー・タグ込み）。
pub fn content_to_data(db: &RekordboxDb, content: &ContentRow) -> Result<TrackData> {
	let folder = content.folder_path.clone().unwrap_or_default();
	let file_name = non_empty(&content.file_name_l).map(str::to_string).unwrap_or_else(|| {
		Path::new(&folder)
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_default()
	});
	let cues = db
		.cues_of(&content.id)?
		.into_iter()
		.map(|c| CueData {
			kind: c.kind,
			in_msec: c.in_msec,
			out_msec: c.out_msec,
			color: c.color,
			color_table_index: c.color_table_index,
			comment: c.comment,
			active_loop: c.active_loop,
			beat_loop_size: c.beat_loop_size,
		})
		.collect();
	Ok(TrackData {
		uuid: content.uuid.clone(),
		file: String::new(), // 書き出し時に write_package が割り当てる
		file_name,
		title: content.title.clone(),
		artist: content.artist.clone(),
		album: content.album.clone(),
		genre: content.genre.clone(),
		key: content.key.clone(),
		bpm: content.bpm,
		length: content.length,
		rating: content.rating,
		color: content.color.clone(),
		comment: content.commnt.clone(),
		release_year: content.release_year,
		track_no: content.track_no,
		file_size: content.file_size,
		analysed: content.analysed,
		sample_rate: content.sample_rate,
		bit_rate: content.bit_rate,
		bit_depth: content.bit_depth,
		cues,
		my_tags: db.my_tag_names_of(&content.id)?,
		analysis: BTreeMap::new(),
		artwork: None,
		stream: content.stream_uri().map(|uri| StreamData {
			uri: uri.to_string(),
			file_type: content.file_type.unwrap_or(0),
			ext_info: non_empty(&content.ext_info).map(str::to_string),
		}),
	})
}

/// 楽曲ファイルの実パス（存在する場合のみ）。
fn folder_path_of(content: &ContentRow) -> Option<PathBuf> {
	let p = PathBuf::from(non_empty(&content.folder_path)?);
	p.is_file().then_some(p)
}

/// 楽曲に紐づく既存の ANLZ 解析ファイルを種別ごとに返す。
pub fn anlz_paths_of(db: &RekordboxDb, content: &ContentRow) -> BTreeMap<String, PathBuf> {
	let Some(dat) = non_empty(&content.analysis_data_path) else {
		return BTreeMap::new();
	};
	let dat_path = db.share_path(dat);
	let Some(dir) = dat_path.parent() else {
		return BTreeMap::new();
	};
	let mut paths = anlz::anlz_paths(dir);
	paths.retain(|_, p| p.is_file());
	paths
}

/// 楽曲に紐づく既存のアートワーク画像の実パス。
///
/// Rekordbox は `DjmdContent.ImagePath` に `/PIONEER/Artwork/.../artwork.jpg` のような
/// share フォルダ相対パスを保持している。
pub fn artwork_path_of(db: &RekordboxDb, content: &ContentRow) -> Option<PathBuf> {
	let p = db.share_path(non_empty(&content.image_path)?);
	p.is_file().then_some(p)
}

/// 指定したプレイリスト群をパッケージに書き出す。フォルダを含めても良い。
pub fn export_playlists(db: &RekordboxDb, playlist_uuids: &[String], dest: &Path, task: &Task) -> Result<Package> {
	let selected: HashSet<&str> = playlist_uuids.iter().map(String::as_str).collect();
	task.progress("プレイリストを収集中", 0, 0);
	// 親フォルダ名は選択外の行からも引くため、全行を変換してから絞り込む。
	let all_rows = db.playlists()?;
	let playlist_data: Vec<PlaylistData> = playlists_to_data(db, &all_rows)?
		.into_iter()
		.filter(|p| selected.contains(p.uuid.as_str()))
		.collect();

	// 収録曲を UUID で集約（重複排除）。
	let mut wanted: Vec<&str> = Vec::new();
	let mut seen: HashSet<&str> = HashSet::new();
	for pl in &playlist_data {
		for uuid in &pl.track_uuids {
			if seen.insert(uuid) {
				wanted.push(uuid);
			}
		}
	}

	// 楽曲の実体とメタデータを集める。
	let mut tracks = Vec::new();
	let mut source_paths = HashMap::new();
	let mut analysis_paths: AnalysisPaths = HashMap::new();
	let mut artwork_paths = HashMap::new();
	let total = wanted.len();
	for (i, uuid) in wanted.iter().enumerate() {
		task.check_cancelled()?;
		let Some(content) = db.find_content_by_uuid(uuid)? else {
			continue;
		};
		task.progress("楽曲メタデータを収集中", i + 1, total);
		let track = content_to_data(db, &content)?;
		if let Some(p) = folder_path_of(&content) {
			source_paths.insert(uuid.to_string(), p);
		}
		let anlz = anlz_paths_of(db, &content);
		if !anlz.is_empty() {
			analysis_paths.insert(uuid.to_string(), anlz);
		}
		if let Some(p) = artwork_path_of(db, &content) {
			artwork_paths.insert(uuid.to_string(), p);
		}
		tracks.push(track);
	}

	let mut package = Package::new(tracks, playlist_data);
	write_package(dest, &mut package, &source_paths, &analysis_paths, &artwork_paths, task)?;
	Ok(package)
}
