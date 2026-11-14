//! 実機の Rekordbox データベースを使う検証（既定では実行しない）。
//!
//! 実行方法:
//!
//! ```text
//! REKORDSYNC_TEST_REF=<Python 版の出力フォルダ> REKORDSYNC_TEST_WORK=<作業フォルダ> \
//!   cargo test -- --ignored --test-threads=1
//! ```
//!
//! 実機の `master.db` は読み取り専用で開く。書き込みの検証は作業フォルダへ
//! コピーした DB に対して行う。
//!
//! `REKORDSYNC_TEST_REF` には、移植元の Python 版（git 履歴の `src/rekordsync`）で
//! 同じ DB から出力した `tree.json`（`collect_playlist_tree` の結果）と
//! `tracks.json`（`content_to_data` の結果に `_anlz` / `_art` を加えたもの）を置く。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::exporter::{anlz_paths_of, artwork_path_of, collect_playlist_tree, content_to_data, export_playlists};
use crate::importer::{import_package, ImportOptions};
use crate::models::{Package, TrackData};
use crate::rekordbox::{locate_database, RekordboxDb};
use crate::task::Task;

fn env_dir(name: &str) -> PathBuf {
	PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("環境変数 {name} を指定してください")))
}

fn real_db() -> RekordboxDb {
	let path = locate_database().expect("master.db が見つかりません");
	RekordboxDb::open_read_only(&path).expect("master.db を開けません")
}

/// Python 版（pyrekordbox）の出力と、プレイリスト一覧・楽曲情報が一致するか。
#[test]
#[ignore]
fn read_matches_python() {
	let ref_dir = env_dir("REKORDSYNC_TEST_REF");
	let db = real_db();

	let tree = serde_json::to_value(collect_playlist_tree(&db).unwrap()).unwrap();
	let expected: Value = serde_json::from_reader(File::open(ref_dir.join("tree.json")).unwrap()).unwrap();
	let key = |v: &Value| -> HashMap<String, Value> {
		v.as_array()
			.unwrap()
			.iter()
			.map(|r| (r["uuid"].as_str().unwrap().to_string(), r.clone()))
			.collect()
	};
	let (got, want) = (key(&tree), key(&expected));
	assert_eq!(got.len(), want.len(), "プレイリスト数が違います");
	for (uuid, w) in &want {
		assert_eq!(got.get(uuid), Some(w), "プレイリスト {uuid} が違います");
	}
	// 並び順（ツリーの兄弟順に影響する）も同じであること。
	assert_eq!(
		tree.as_array().unwrap().iter().map(|r| &r["uuid"]).collect::<Vec<_>>(),
		expected
			.as_array()
			.unwrap()
			.iter()
			.map(|r| &r["uuid"])
			.collect::<Vec<_>>(),
		"プレイリストの並び順が違います"
	);

	let tracks: Value = serde_json::from_reader(File::open(ref_dir.join("tracks.json")).unwrap()).unwrap();
	for t in tracks.as_array().unwrap() {
		let uuid = t["uuid"].as_str().unwrap();
		let content = db.find_content_by_uuid(uuid).unwrap().expect("楽曲が見つかりません");
		let mut got = serde_json::to_value(content_to_data(&db, &content).unwrap()).unwrap();
		let mut want = t.clone();
		let want = want.as_object_mut().unwrap();
		let want_anlz = want.remove("_anlz").unwrap();
		let want_art = want.remove("_art").unwrap();
		// Python 版は key / color の名前列を取り違えて常に空だったため比較しない。
		let got = got.as_object_mut().unwrap();
		got.remove("key");
		got.remove("color");
		assert_eq!(
			Value::Object(got.clone()),
			Value::Object(want.clone()),
			"楽曲 {uuid} が違います"
		);

		let anlz: serde_json::Map<String, Value> = anlz_paths_of(&db, &content)
			.into_iter()
			.map(|(k, p)| (k, Value::String(p.display().to_string())))
			.collect();
		assert_eq!(Value::Object(anlz), want_anlz, "楽曲 {uuid} の解析ファイルが違います");
		let art = artwork_path_of(&db, &content).map(|p| Value::String(p.display().to_string()));
		assert_eq!(
			art.unwrap_or(Value::Null),
			want_art,
			"楽曲 {uuid} のアートワークが違います"
		);
	}
}

/// パッケージ内の楽曲 UUID を書き換えたコピーを作る（コピー先 DB で「新規の曲」として扱わせるため）。
fn rewrite_uuids(src: &Path, dest: &Path) -> Package {
	rewrite_package(src, dest, |_| {})
}

/// `rewrite_uuids` に加え、楽曲ごとに `edit` を適用したコピーを作る。
fn rewrite_package(src: &Path, dest: &Path, edit: impl Fn(&mut TrackData)) -> Package {
	let mut zip = ZipArchive::new(BufReader::new(File::open(src).unwrap())).unwrap();
	let mut package: Package = serde_json::from_reader(zip.by_name(crate::config::MANIFEST_NAME).unwrap()).unwrap();
	let map = |u: &str| format!("rstest-{u}");
	for t in &mut package.tracks {
		t.uuid = map(&t.uuid);
		edit(t);
	}
	for p in &mut package.playlists {
		p.track_uuids = p.track_uuids.iter().map(|u| map(u)).collect();
	}
	let mut zw = ZipWriter::new(File::create(dest).unwrap());
	for i in 0..zip.len() {
		let entry = zip.by_index_raw(i).unwrap();
		if entry.name() != crate::config::MANIFEST_NAME {
			zw.raw_copy_file(entry).unwrap();
		}
	}
	zw.start_file(
		crate::config::MANIFEST_NAME,
		SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
	)
	.unwrap();
	zw.write_all(serde_json::to_string_pretty(&package).unwrap().as_bytes())
		.unwrap();
	zw.finish().unwrap();
	package
}

fn copy_db(dest_dir: &Path) -> PathBuf {
	let src = locate_database().unwrap();
	let _ = std::fs::remove_dir_all(dest_dir);
	std::fs::create_dir_all(dest_dir).unwrap();
	let dest = dest_dir.join("master.db");
	std::fs::copy(&src, &dest).unwrap();
	std::fs::copy(
		src.parent().unwrap().join("masterPlaylists6.xml"),
		dest_dir.join("masterPlaylists6.xml"),
	)
	.unwrap();
	dest
}

/// 書き出し → 取り込み（DB のコピーへ）を通しで動かす。
///
/// 作業フォルダに次を残す（Python 側の検証で使う）:
/// - `export.rsync`: 実機 DB から書き出したパッケージ
/// - `package.rsync`: 楽曲 UUID を書き換えたパッケージ
/// - `rust/`: Rust 版で取り込んだ DB のコピー
/// - `selected.json`: 書き出したプレイリストの UUID
#[test]
#[ignore]
fn export_then_import_into_copy() {
	let work = env_dir("REKORDSYNC_TEST_WORK");
	std::fs::create_dir_all(&work).unwrap();

	// 曲数の少ない（2〜6 曲）通常のプレイリストを、解析データ付きの曲を含むものから 2 つ選ぶ。
	let db = real_db();
	let playlists = collect_playlist_tree(&db).unwrap();
	let selected: Vec<String> = playlists
		.iter()
		.filter(|p| !p.is_folder && (2..=6).contains(&p.track_count))
		.take(2)
		.map(|p| p.uuid.clone())
		.collect();
	assert!(!selected.is_empty(), "検証に使えるプレイリストがありません");
	std::fs::write(work.join("selected.json"), serde_json::to_string(&selected).unwrap()).unwrap();

	let exported = work.join("export.rsync");
	let package = export_playlists(&db, &selected, &exported, &Task::silent()).unwrap();
	drop(db);
	assert_eq!(package.playlists.len(), selected.len());
	eprintln!(
		"書き出し: プレイリスト {} / 楽曲 {} / 解析付き {} / アートワーク付き {}",
		package.playlists.len(),
		package.tracks.len(),
		package.tracks.iter().filter(|t| !t.analysis.is_empty()).count(),
		package.tracks.iter().filter(|t| t.artwork.is_some()).count(),
	);

	let pkg_path = work.join("package.rsync");
	let package = rewrite_uuids(&exported, &pkg_path);

	let db_path = copy_db(&work.join("rust"));
	let mut db = RekordboxDb::open(&db_path).unwrap();
	db.set_require_closed(false);
	let options = ImportOptions {
		extract_dir: work.join("music"),
		conflict_policy: "rename".into(),
		import_cues: true,
		import_tags: true,
		import_analysis: true,
		import_artwork: true,
	};
	let _ = std::fs::remove_dir_all(&options.extract_dir);
	let all: HashSet<String> = package.playlists.iter().map(|p| p.uuid.clone()).collect();

	// 1 回目: 楽曲はすべて新規。同名プレイリストがあるので rename で別名を作る。
	let r1 = import_package(&mut db, &pkg_path, &options, Some(&all), &Task::silent()).unwrap();
	eprintln!("1 回目:\n{}\n{:?}", r1.summary(), r1.warnings);
	let with_files = package.tracks.iter().filter(|t| !t.file.is_empty()).count();
	assert_eq!(r1.tracks_added, with_files);
	assert_eq!(r1.tracks_reused, 0);
	assert_eq!(r1.playlists_created, selected.len());
	assert!(
		r1.warnings.iter().all(|w| w.contains("ファイル欠落")),
		"{:?}",
		r1.warnings
	);

	// 2 回目: 同じパッケージを merge で取り込む。楽曲は再利用、重複追加なし。
	let merge = ImportOptions {
		conflict_policy: "merge".into(),
		..options.clone()
	};
	let r2 = import_package(&mut db, &pkg_path, &merge, Some(&all), &Task::silent()).unwrap();
	eprintln!("2 回目:\n{}", r2.summary());
	assert_eq!(r2.tracks_added, 0);
	assert_eq!(r2.tracks_reused, with_files);
	assert_eq!(r2.playlists_merged, selected.len());
	drop(db);
}

/// ストリーミング楽曲（Apple Music 等）を含むプレイリストを書き出し → 取り込み（DB のコピーへ）する。
///
/// 作業フォルダの `stream/` に、書き出したパッケージと取り込み先 DB のコピーを残す。
#[test]
#[ignore]
fn stream_tracks_export_then_import_into_copy() {
	let work = env_dir("REKORDSYNC_TEST_WORK").join("stream");
	std::fs::create_dir_all(&work).unwrap();

	// ストリーミング楽曲を収録した通常のプレイリストのうち、曲数が最少のものを選ぶ。
	let db = real_db();
	let songs = db.playlist_track_uuids().unwrap();
	let is_stream = |uuid: &String| {
		db.find_content_by_uuid(uuid)
			.unwrap()
			.is_some_and(|c| c.stream_uri().is_some())
	};
	let selected = db
		.playlists()
		.unwrap()
		.into_iter()
		.filter(|p| p.attribute == 0)
		.filter_map(|p| songs.get(&p.id).map(|s| (p, s)))
		.filter(|(_, s)| s.iter().any(is_stream))
		.min_by_key(|(_, s)| s.len())
		.map(|(p, _)| p.uuid)
		.expect("ストリーミング楽曲を含むプレイリストがありません");

	let exported = work.join("export.rsync");
	let package = export_playlists(&db, &[selected.clone()], &exported, &Task::silent()).unwrap();
	let streams: Vec<&TrackData> = package.tracks.iter().filter(|t| t.stream.is_some()).collect();
	assert!(!streams.is_empty());
	for t in &streams {
		let s = t.stream.as_ref().unwrap();
		let src = db.find_content_by_uuid(&t.uuid).unwrap().unwrap();
		assert_eq!(Some(s.uri.as_str()), src.folder_path.as_deref());
		assert_eq!(Some(s.file_type), src.file_type);
		assert_eq!(s.ext_info, src.ext_info);
		assert!(t.file.is_empty(), "ストリーミング楽曲にファイルが格納されています");
	}
	drop(db);

	let db_path = copy_db(&work.join("db"));
	let mut db = RekordboxDb::open(&db_path).unwrap();
	db.set_require_closed(false);
	let options = ImportOptions {
		extract_dir: work.join("music"),
		conflict_policy: "rename".into(),
		import_cues: true,
		import_tags: true,
		import_analysis: true,
		import_artwork: true,
	};
	let _ = std::fs::remove_dir_all(&options.extract_dir);
	let all: HashSet<String> = HashSet::from([selected]);

	// 1 回目: UUID だけを書き換える。取り込み先に同じ URI の楽曲があるため再利用する。
	let reuse_pkg = work.join("reuse.rsync");
	rewrite_uuids(&exported, &reuse_pkg);
	let r1 = import_package(&mut db, &reuse_pkg, &options, Some(&all), &Task::silent()).unwrap();
	eprintln!("再利用:\n{}\n{:?}", r1.summary(), r1.warnings);
	assert!(r1.tracks_reused >= streams.len());
	assert!(
		r1.warnings.iter().all(|w| w.contains("ファイル欠落")),
		"{:?}",
		r1.warnings
	);

	// 2 回目: URI も書き換え、取り込み先に存在しないストリーミング楽曲として新規に追加させる。
	let new_pkg = work.join("new.rsync");
	let package = rewrite_package(&exported, &new_pkg, |t| {
		if let Some(s) = t.stream.as_mut() {
			s.uri = format!("{}-rstest", s.uri);
		}
	});
	let r2 = import_package(&mut db, &new_pkg, &options, Some(&all), &Task::silent()).unwrap();
	eprintln!("新規:\n{}\n{:?}", r2.summary(), r2.warnings);
	assert!(r2.tracks_added >= streams.len());
	assert!(
		r2.warnings.iter().all(|w| w.contains("ファイル欠落")),
		"{:?}",
		r2.warnings
	);
	let created = db
		.find_playlist_by_name(&format!("{} (3)", package.playlists[0].name))
		.unwrap()
		.expect("取り込んだプレイリストがありません");
	let members = db.playlist_content_ids(&created.id).unwrap();
	for t in package.tracks.iter().filter(|t| t.stream.is_some()) {
		let s = t.stream.as_ref().unwrap();
		let row = db
			.find_content_by_stream_uri(&s.uri)
			.unwrap()
			.expect("ストリーミング楽曲が追加されていません");
		assert_eq!(row.file_type, Some(s.file_type));
		assert_eq!(row.ext_info, s.ext_info);
		assert_eq!(row.stream_uri(), Some(s.uri.as_str()));
		assert!(members.contains(&row.id), "プレイリストに追加されていません: {}", s.uri);
		assert_eq!(db.cues_of(&row.id).unwrap().len(), t.cues.len());
		if t.analysis.contains_key("DAT") {
			let dat = db.share_path(row.analysis_data_path.as_deref().unwrap());
			let ppth = crate::rekordbox::anlz::read_ppth(&std::fs::read(dat).unwrap()).unwrap();
			let original = s.uri.trim_end_matches("-rstest");
			assert!(ppth.ends_with(original), "PPTH が書き出し元と違います: {ppth}");
		}
	}
	drop(db);
}
