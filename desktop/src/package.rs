//! パッケージ（`.rsync` = zip）の読み書き。
//!
//! レイアウト:
//!
//! ```text
//! manifest.json              ... メタデータ（Package）
//! tracks/<idx>/<file>        ... 楽曲の生ファイル（無加工コピー）
//! analysis/<idx>/ANLZ0000.*  ... 解析ファイル（波形・ビートグリッド等）
//! artwork/<idx>/artwork.jpg  ... アートワーク画像
//! ```
//!
//! 楽曲はファイル名衝突を避けるため `tracks/<連番>/` のサブフォルダに格納する。
//! 解析ファイルは同じ連番の `analysis/<連番>/` に、アートワークは
//! `artwork/<連番>/` に格納する。

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::config;
use crate::models::{Package, TrackData};
use crate::task::Task;

/// 楽曲 UUID -> 種別ごとの解析ファイルの実パス。
pub type AnalysisPaths = HashMap<String, BTreeMap<String, PathBuf>>;

/// zip 内の名前から、展開に使うファイル名だけを取り出す（`../` 等のパス成分は捨てる）。
fn safe_file_name(name: &str) -> Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    if base.is_empty() || base == "." || base == ".." {
        bail!("不正なファイル名です: {name:?}");
    }
    Ok(base.to_string())
}

fn add_file<W: Write + io::Seek>(zw: &mut ZipWriter<W>, src: &Path, arc_name: &str) -> Result<()> {
    let mut file = File::open(src).with_context(|| format!("{} を開けません", src.display()))?;
    let size = file.metadata()?.len();
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(size >= u32::MAX as u64);
    zw.start_file(arc_name, opts)?;
    io::copy(&mut file, zw)?;
    Ok(())
}

/// `package` と楽曲ファイルを zip に書き出す。
///
/// `TrackData.file` / `analysis` / `artwork` には、格納した zip 内パスが入る。
/// 楽曲ファイルが見つからない場合もメタデータは残し、`file` を空にする（欠落マーク）。
/// 失敗やキャンセル時は、書きかけのパッケージファイルを削除する。
pub fn write_package(
    dest: &Path,
    package: &mut Package,
    source_paths: &HashMap<String, PathBuf>,
    analysis_paths: &AnalysisPaths,
    artwork_paths: &HashMap<String, PathBuf>,
    task: &Task,
) -> Result<()> {
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let result = write_package_inner(dest, package, source_paths, analysis_paths, artwork_paths, task);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result?;
    let total = package.tracks.len();
    task.progress("書き出し完了", total, total);
    Ok(())
}

fn write_package_inner(
    dest: &Path,
    package: &mut Package,
    source_paths: &HashMap<String, PathBuf>,
    analysis_paths: &AnalysisPaths,
    artwork_paths: &HashMap<String, PathBuf>,
    task: &Task,
) -> Result<()> {
    let file = File::create(dest).with_context(|| format!("{} を作成できません", dest.display()))?;
    let mut zw = ZipWriter::new(BufWriter::new(file));
    let total = package.tracks.len();

    for (i, track) in package.tracks.iter_mut().enumerate() {
        task.check_cancelled()?;
        let idx = i + 1;
        let arc_name = format!("{}/{idx:04}/{}", config::TRACKS_DIR, track.file_name);
        task.progress(&format!("楽曲をパッケージ中: {}", track.file_name), idx, total);
        match source_paths.get(&track.uuid).filter(|p| p.is_file()) {
            Some(src) => {
                add_file(&mut zw, src, &arc_name)?;
                track.file = arc_name;
            }
            None => track.file = String::new(),
        }

        // 解析ファイル（ANLZ）を同じ連番のフォルダに格納する。
        track.analysis.clear();
        for (kind, apath) in analysis_paths.get(&track.uuid).into_iter().flatten() {
            let Some(name) = config::anlz_file_name(kind) else { continue };
            if !apath.is_file() {
                continue;
            }
            let arc = format!("{}/{idx:04}/{name}", config::ANALYSIS_DIR);
            add_file(&mut zw, apath, &arc)?;
            track.analysis.insert(kind.to_ascii_uppercase(), arc);
        }

        // アートワーク画像を同じ連番のフォルダに格納する。
        track.artwork = None;
        if let Some(art) = artwork_paths.get(&track.uuid).filter(|p| p.is_file()) {
            let arc = format!("{}/{idx:04}/{}", config::ARTWORK_DIR, config::ARTWORK_FILE_NAME);
            add_file(&mut zw, art, &arc)?;
            track.artwork = Some(arc);
        }
    }

    let manifest = serde_json::to_string_pretty(package)?;
    zw.start_file(
        config::MANIFEST_NAME,
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )?;
    zw.write_all(manifest.as_bytes())?;
    zw.finish()?.flush()?;
    Ok(())
}

/// zip パッケージからマニフェストと楽曲ファイルを取り出す。
pub struct PackageReader {
    zip: ZipArchive<BufReader<File>>,
    pub package: Package,
}

impl PackageReader {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!("パッケージが見つかりません: {}", path.display());
        }
        let file = File::open(path)?;
        let mut zip = ZipArchive::new(BufReader::new(file))
            .map_err(|_| anyhow!("壊れたパッケージです: {}", path.display()))?;

        let mut raw = String::new();
        zip.by_name(config::MANIFEST_NAME)
            .map_err(|_| anyhow!("manifest.json が見つかりません"))?
            .read_to_string(&mut raw)?;
        let data: serde_json::Value = serde_json::from_str(&raw).context("manifest.json を解釈できません")?;
        let fmt = data.get("format").cloned().unwrap_or(serde_json::Value::Null);
        if fmt.as_str() != Some(config::MANIFEST_FORMAT) {
            bail!("未知のパッケージ形式です: {fmt}");
        }
        if data.get("version").and_then(serde_json::Value::as_i64).unwrap_or(0) > config::MANIFEST_VERSION {
            bail!("このパッケージはより新しいバージョンで作成されています。アプリを更新してください。");
        }
        let package: Package = serde_json::from_value(data).context("manifest.json の内容が不正です")?;
        Ok(Self { zip, package })
    }

    fn extract_entry(&mut self, arc: &str, out_path: &Path) -> Result<bool> {
        let mut entry = match self.zip.by_name(arc) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let mut out = File::create(out_path).with_context(|| format!("{} を作成できません", out_path.display()))?;
        io::copy(&mut entry, &mut out)?;
        Ok(true)
    }

    /// 1 曲を `dest_dir` に展開し、展開先パスを返す。
    ///
    /// ファイルが欠落（`track.file` が空）している場合は `None`。
    /// 同名・同サイズのファイルが既にある場合はそのまま再利用する。
    pub fn extract_track(&mut self, track: &TrackData, dest_dir: &Path) -> Result<Option<PathBuf>> {
        if track.file.is_empty() {
            return Ok(None);
        }
        std::fs::create_dir_all(dest_dir)?;
        let out_path = dest_dir.join(safe_file_name(&track.file_name)?);
        if let (Ok(meta), Some(size)) = (std::fs::metadata(&out_path), track.file_size) {
            if meta.is_file() && meta.len() as i64 == size {
                return Ok(Some(out_path));
            }
        }
        if !self.extract_entry(&track.file, &out_path)? {
            bail!("パッケージに楽曲ファイルがありません: {}", track.file);
        }
        Ok(Some(out_path))
    }

    /// 解析（ANLZ）ファイルを `dest_dir` に展開し、種別ごとの実パスを返す。
    pub fn extract_analysis(&mut self, track: &TrackData, dest_dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
        let mut out = BTreeMap::new();
        if track.analysis.is_empty() {
            return Ok(out);
        }
        std::fs::create_dir_all(dest_dir)?;
        for (kind, arc) in &track.analysis {
            let kind = kind.to_ascii_uppercase();
            let name = match config::anlz_file_name(&kind) {
                Some(n) => n.to_string(),
                None => safe_file_name(arc)?,
            };
            let out_path = dest_dir.join(name);
            if self.extract_entry(arc, &out_path)? {
                out.insert(kind, out_path);
            }
        }
        Ok(out)
    }

    /// アートワーク画像を `dest_dir` に展開し、その実パスを返す。
    pub fn extract_artwork(&mut self, track: &TrackData, dest_dir: &Path) -> Result<Option<PathBuf>> {
        let Some(arc) = track.artwork.as_deref().filter(|a| !a.is_empty()) else {
            return Ok(None);
        };
        std::fs::create_dir_all(dest_dir)?;
        let out_path = dest_dir.join(config::ARTWORK_FILE_NAME);
        Ok(self.extract_entry(arc, &out_path)?.then_some(out_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_components() {
        assert_eq!(safe_file_name("../../evil.mp3").unwrap(), "evil.mp3");
        assert_eq!(safe_file_name(r"..\x\曲.wav").unwrap(), "曲.wav");
        assert!(safe_file_name("..").is_err());
        assert!(safe_file_name("a/").is_err());
    }
}
