//! `masterPlaylists6.xml`（Rekordbox がプレイリストの補助情報を持つファイル）の更新。
//!
//! Rekordbox は `master.db` の `djmdPlaylist` に加えて、このファイルにも
//! プレイリストごとの `<NODE>` を持つ。DB にプレイリストを追加したら、ここにも
//! 同じ ID の `<NODE>` を追加する必要がある（pyrekordbox `MasterPlaylistXml.add` 相当）。
//!
//! 既存の書式を崩さないよう、XML 全体を組み直さずに `</PLAYLISTS>` の直前へ
//! 行を差し込む。

use std::path::Path;

use anyhow::{bail, Context, Result};

/// 追加する `<NODE>` 1 件分。
#[derive(Debug, Clone)]
pub struct PlaylistNode {
    /// `djmdPlaylist.ID`（10 進の文字列）。
    pub id: String,
    /// 親の `djmdPlaylist.ID`。ルートは `"root"`。
    pub parent_id: String,
    /// 0=playlist, 1=folder, 4=smart。
    pub attribute: i64,
    /// 更新時刻（UNIX エポックからのミリ秒）。
    pub timestamp_ms: i64,
}

/// 10 進の ID を XML 用の 16 進大文字表記にする（`"root"` は `"0"`）。
fn hex_id(id: &str) -> Result<String> {
    if id == "root" {
        return Ok("0".into());
    }
    let n: u64 = id
        .parse()
        .with_context(|| format!("プレイリスト ID が数値ではありません: {id}"))?;
    Ok(format!("{n:X}"))
}

/// XML テキストへ `<NODE>` 行を追加した結果を返す。同じ Id の NODE が既にあれば飛ばす。
pub fn add_nodes(xml: &str, nodes: &[PlaylistNode]) -> Result<String> {
    let newline = if xml.contains("\r\n") { "\r\n" } else { "\n" };

    let mut lines = String::new();
    let mut indent: Option<String> = None;
    if let Some(pos) = xml.find("<NODE ") {
        let line_start = xml[..pos].rfind('\n').map_or(0, |p| p + 1);
        let ws = &xml[line_start..pos];
        if ws.chars().all(char::is_whitespace) {
            indent = Some(ws.to_string());
        }
    }

    for node in nodes {
        let id = hex_id(&node.id)?;
        if xml.contains(&format!("<NODE Id=\"{id}\"")) {
            continue;
        }
        let parent = hex_id(&node.parent_id)?;
        lines.push_str(&format!(
            "{{indent}}<NODE Id=\"{id}\" ParentId=\"{parent}\" Attribute=\"{}\" Timestamp=\"{}\" Lib_Type=\"0\" CheckType=\"0\"/>{newline}",
            node.attribute, node.timestamp_ms
        ));
    }
    if lines.is_empty() {
        return Ok(xml.to_string());
    }

    if let Some(pos) = xml.find("</PLAYLISTS>") {
        let line_start = xml[..pos].rfind('\n').map_or(0, |p| p + 1);
        let close_ws = &xml[line_start..pos];
        let (insert_at, close_indent) = if close_ws.chars().all(char::is_whitespace) {
            (line_start, close_ws.to_string())
        } else {
            (pos, String::new())
        };
        let indent = indent.unwrap_or_else(|| format!("{close_indent}  "));
        let block = lines.replace("{indent}", &indent);
        let mut out = String::with_capacity(xml.len() + block.len());
        out.push_str(&xml[..insert_at]);
        out.push_str(&block);
        out.push_str(&xml[insert_at..]);
        return Ok(out);
    }

    if let Some(pos) = xml.find("<PLAYLISTS/>") {
        let line_start = xml[..pos].rfind('\n').map_or(0, |p| p + 1);
        let open_indent: String = xml[line_start..pos].chars().filter(|c| c.is_whitespace()).collect();
        let block = lines.replace("{indent}", &format!("{open_indent}  "));
        return Ok(format!(
            "{}<PLAYLISTS>{newline}{block}{open_indent}</PLAYLISTS>{}",
            &xml[..pos],
            &xml[pos + "<PLAYLISTS/>".len()..]
        ));
    }

    bail!("masterPlaylists6.xml に PLAYLISTS 要素がありません")
}

/// `db_dir/masterPlaylists6.xml` に NODE を追加して保存する。ファイルが無ければ何もしない。
pub fn append_to_file(db_dir: &Path, nodes: &[PlaylistNode]) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let path = db_dir.join("masterPlaylists6.xml");
    if !path.is_file() {
        return Ok(());
    }
    let xml = std::fs::read_to_string(&path)
        .with_context(|| format!("{} を読めません", path.display()))?;
    let updated = add_nodes(&xml, nodes)?;
    if updated == xml {
        return Ok(());
    }
    let tmp = path.with_extension("xml.rekordsync-tmp");
    std::fs::write(&tmp, updated).with_context(|| format!("{} を書けません", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("{} を更新できません", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\n<MASTER_PLAYLIST Version=\"3.0.0\" AutomaticSync=\"0\">\n  <PRODUCT Name=\"rekordbox\" Version=\"7.2.19\" Company=\"Pioneer DJ\"/>\n  <PLAYLISTS>\n    <NODE Id=\"EE62DD9F\" ParentId=\"0\" Attribute=\"0\" Timestamp=\"1792441205823\" Lib_Type=\"0\" CheckType=\"0\"/>\n  </PLAYLISTS>\n</MASTER_PLAYLIST>\n";

    fn node(id: &str) -> PlaylistNode {
        PlaylistNode { id: id.into(), parent_id: "root".into(), attribute: 0, timestamp_ms: 1700000000123 }
    }

    #[test]
    fn appends_with_same_indent() {
        let out = add_nodes(SAMPLE, &[node("255")]).unwrap();
        assert!(out.contains(
            "    <NODE Id=\"FF\" ParentId=\"0\" Attribute=\"0\" Timestamp=\"1700000000123\" Lib_Type=\"0\" CheckType=\"0\"/>\n  </PLAYLISTS>"
        ));
        assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\n<MASTER_PLAYLIST"));
    }

    #[test]
    fn skips_existing_id() {
        let out = add_nodes(SAMPLE, &[node(&0xEE62DD9Fu64.to_string())]).unwrap();
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn handles_empty_playlists() {
        let xml = "<MASTER_PLAYLIST>\n  <PLAYLISTS/>\n</MASTER_PLAYLIST>\n";
        let out = add_nodes(xml, &[node("16")]).unwrap();
        assert!(out.contains("  <PLAYLISTS>\n    <NODE Id=\"10\""));
        assert!(out.contains("CheckType=\"0\"/>\n  </PLAYLISTS>\n</MASTER_PLAYLIST>"));
    }
}
