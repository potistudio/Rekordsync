//! Rekordbox のローカルライブラリ（`master.db`・解析ファイル・補助 XML）の操作。
//!
//! pyrekordbox のうち本アプリが使う機能を Rust へ移植したもの。

pub mod anlz;
mod db;
mod key;
mod locate;
mod playlist_xml;

pub use db::{non_empty, ContentRow, CueRow, PlaylistRow, RekordboxDb, Table};
pub use locate::locate_database;
