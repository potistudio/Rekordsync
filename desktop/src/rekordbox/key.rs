//! `master.db`（SQLCipher）の鍵の復元。
//!
//! 鍵は pyrekordbox と同じく難読化した形で保持し、実行時に
//! Base85 デコード → XOR → zlib 展開の順で元に戻す。

use std::io::Read;

use anyhow::{bail, Context, Result};

/// 難読化済みの鍵（pyrekordbox `db6.database.BLOB` と同一）。
const BLOB: &[u8] = b"PN_Pq^*N>(JYe*u^8;Yg76HuZ<mR13S?=>)b9;DpoTXV(6ItkU`}8*m6tx_I{Solh_N#dfe{v=";

/// XOR に使う鍵（pyrekordbox `utils.BLOB_KEY` と同一）。
const BLOB_KEY: &[u8] = b"657f48f84c437cc1";

/// Python `base64.b85decode` と同じ文字表（RFC 1924）。
const B85_ALPHABET: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// Base85（RFC 1924 の文字表）をデコードする。末尾の端数は Python と同じく
/// `~` で埋めて計算し、埋めた分のバイトを捨てる。
fn b85decode(input: &[u8]) -> Result<Vec<u8>> {
    let mut table = [u8::MAX; 256];
    for (i, &c) in B85_ALPHABET.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let padding = (5 - input.len() % 5) % 5;
    let mut out = Vec::with_capacity(input.len() / 5 * 4 + 4);
    let padded = input.iter().copied().chain(std::iter::repeat(b'~').take(padding));
    let chars: Vec<u8> = padded.collect();
    for chunk in chars.chunks(5) {
        let mut acc: u64 = 0;
        for &c in chunk {
            let v = table[c as usize];
            if v == u8::MAX {
                bail!("Base85 として不正な文字です: {:?}", c as char);
            }
            acc = acc * 85 + v as u64;
        }
        if acc > u32::MAX as u64 {
            bail!("Base85 の値が範囲外です");
        }
        out.extend_from_slice(&(acc as u32).to_be_bytes());
    }
    out.truncate(out.len() - padding);
    Ok(out)
}

/// 埋め込みの難読化データから SQLCipher の鍵を復元する。
pub fn database_key() -> Result<String> {
    let data = b85decode(BLOB)?;
    let xored: Vec<u8> = data
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ BLOB_KEY[i % BLOB_KEY.len()])
        .collect();
    let mut key = String::new();
    flate2::read::ZlibDecoder::new(xored.as_slice())
        .read_to_string(&mut key)
        .context("データベースの鍵を復元できません")?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b85_matches_python() {
        // Python: base64.b85encode(b"rekordsync!") == b"a%F38a%6LPZet+"
        assert_eq!(b85decode(b"a%F38a%6LPZet+").unwrap(), b"rekordsync!");
        assert_eq!(b85decode(b"").unwrap(), b"");
    }

    #[test]
    fn key_looks_valid() {
        // pyrekordbox は "402fd" で始まらない鍵を不正とみなす。
        let key = database_key().unwrap();
        assert!(key.starts_with("402fd"), "鍵の形式が想定と違います");
    }
}
