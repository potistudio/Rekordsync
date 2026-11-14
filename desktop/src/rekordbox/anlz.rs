//! Rekordbox 解析ファイル（ANLZ: `.DAT` / `.EXT` / `.2EX`）の操作。
//!
//! ファイル構造（数値はすべてビッグエンディアン）:
//!
//! - ファイルヘッダー: `"PMAI"`、ヘッダー長 (u32)、ファイル全体の長さ (u32)、…
//! - タグの並び: 種別 4 文字、タグのヘッダー長 (u32)、タグ全体の長さ (u32)、本体
//! - `PPTH` タグの本体: パスのバイト数 (u32)、UTF-16BE のパス + 終端 `00 00`
//!
//! 本モジュールは `PPTH`（楽曲ファイルのパス）だけを差し替え、他のタグは
//! バイト列をそのまま残す。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

/// `ANLZ[0-9]{4}.(DAT|EXT|2EX)` に前方一致するファイル名なら、その種別を返す。
/// pyrekordbox の `RE_ANLZ` と同じ判定（区切りの 1 文字は任意）。
fn anlz_kind(name: &str) -> Option<&'static str> {
    let b = name.as_bytes();
    if b.len() < 12 || &b[..4] != b"ANLZ" || !b[4..8].iter().all(u8::is_ascii_digit) {
        return None;
    }
    match &b[9..12] {
        b"DAT" => Some("DAT"),
        b"EXT" => Some("EXT"),
        b"2EX" => Some("2EX"),
        _ => None,
    }
}

/// フォルダ内の ANLZ ファイルを種別（`DAT` / `EXT` / `2EX`）ごとに返す。
/// フォルダが無い・読めない場合は空。
pub fn anlz_paths(dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(kind) = name.to_str().and_then(anlz_kind) {
            out.insert(kind.to_string(), entry.path());
        }
    }
    out
}

fn be32(data: &[u8], offset: usize) -> Result<usize> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("ANLZ ファイルが途中で切れています"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()) as usize)
}

/// ANLZ ファイルのバイト列の `PPTH` タグを `new_path` に書き換えた新しいバイト列を返す。
///
/// パス区切りは `/` にそろえる（pyrekordbox `PPTHAnlzTag.set` と同じ）。
/// `PPTH` タグが無いファイルはエラー。
pub fn set_ppth(data: &[u8], new_path: &str) -> Result<Vec<u8>> {
    if data.get(..4) != Some(b"PMAI") {
        bail!("ANLZ ファイルではありません");
    }
    let len_header = be32(data, 4)?;
    let len_file = be32(data, 8)?;
    if len_header < 12 || len_header > data.len() || len_file > data.len() {
        bail!("ANLZ ファイルのヘッダーが不正です");
    }

    let mut out = data[..len_header].to_vec();
    let mut found = false;
    let mut i = len_header;
    while i < len_file {
        let tag_type = data
            .get(i..i + 4)
            .ok_or_else(|| anyhow!("ANLZ ファイルが途中で切れています"))?;
        let tag_header = be32(data, i + 4)?;
        let tag_len = be32(data, i + 8)?;
        if tag_len < 12 || i + tag_len > len_file {
            bail!("ANLZ タグの長さが不正です");
        }
        if tag_type == b"PPTH" {
            if tag_header != 16 {
                bail!("PPTH タグのヘッダー長が想定外です: {tag_header}");
            }
            let mut path: Vec<u8> = new_path
                .replace('\\', "/")
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect();
            path.extend_from_slice(&[0, 0]);
            out.extend_from_slice(b"PPTH");
            out.extend_from_slice(&(tag_header as u32).to_be_bytes());
            out.extend_from_slice(&((tag_header + path.len()) as u32).to_be_bytes());
            out.extend_from_slice(&(path.len() as u32).to_be_bytes());
            out.extend_from_slice(&path);
            found = true;
        } else {
            out.extend_from_slice(&data[i..i + tag_len]);
        }
        i += tag_len;
    }
    if !found {
        bail!("PPTH タグがありません");
    }
    let total = out.len() as u32;
    out[8..12].copy_from_slice(&total.to_be_bytes());
    Ok(out)
}

/// `PPTH` タグに入っている楽曲パスを読む。
#[cfg(test)]
pub fn read_ppth(data: &[u8]) -> Option<String> {
    let len_header = be32(data, 4).ok()?;
    let len_file = be32(data, 8).ok()?;
    let mut i = len_header;
    while i < len_file {
        let tag_len = be32(data, i + 8).ok()?;
        if data.get(i..i + 4)? == b"PPTH" {
            let n = be32(data, i + 12).ok()?;
            let raw = data.get(i + 16..i + 16 + n.saturating_sub(2))?;
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            return String::from_utf16(&units).ok();
        }
        if tag_len == 0 {
            return None;
        }
        i += tag_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ヘッダー + PQTZ（ダミー）+ PPTH + PVBR（ダミー）の最小 ANLZ を作る。
    fn sample(path: &str) -> Vec<u8> {
        let mut tags = Vec::new();
        tags.extend_from_slice(b"PQTZ");
        tags.extend_from_slice(&24u32.to_be_bytes());
        tags.extend_from_slice(&28u32.to_be_bytes());
        tags.extend_from_slice(&[7u8; 16]);
        let p: Vec<u8> = path.encode_utf16().flat_map(u16::to_be_bytes).chain([0, 0]).collect();
        tags.extend_from_slice(b"PPTH");
        tags.extend_from_slice(&16u32.to_be_bytes());
        tags.extend_from_slice(&((16 + p.len()) as u32).to_be_bytes());
        tags.extend_from_slice(&(p.len() as u32).to_be_bytes());
        tags.extend_from_slice(&p);
        tags.extend_from_slice(b"PVBR");
        tags.extend_from_slice(&16u32.to_be_bytes());
        tags.extend_from_slice(&20u32.to_be_bytes());
        tags.extend_from_slice(&[9u8; 8]);
        let mut out = Vec::new();
        out.extend_from_slice(b"PMAI");
        out.extend_from_slice(&28u32.to_be_bytes());
        out.extend_from_slice(&((28 + tags.len()) as u32).to_be_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&tags);
        out
    }

    #[test]
    fn rewrites_ppth_and_lengths() {
        let before = sample("C:/old/a.mp3");
        let after = set_ppth(&before, r"D:\新しい\曲.mp3").unwrap();
        assert_eq!(read_ppth(&after).unwrap(), "D:/新しい/曲.mp3");
        assert_eq!(be32(&after, 8).unwrap(), after.len());
        // 前後のタグはそのまま残る。
        assert_eq!(&after[28..56], &before[28..56]);
        assert_eq!(&after[after.len() - 20..], &before[before.len() - 20..]);
    }

    #[test]
    fn kind_detection() {
        assert_eq!(anlz_kind("ANLZ0000.DAT"), Some("DAT"));
        assert_eq!(anlz_kind("ANLZ0000.2EX"), Some("2EX"));
        assert_eq!(anlz_kind("ANLZ.DAT"), None);
        assert_eq!(anlz_kind("artwork.jpg"), None);
    }
}
