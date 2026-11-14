# rekordsync

Rekordboxのプレイリストを、別環境へ持ち運んで同期するスタンドアロンGUIアプリ
専用パッケージ(`.rsync`)に、**楽曲の生ファイル・プレイリスト・キュー情報・MyTag・解析データ**をまとめ、別の PCのローカルデータベースへ取り込む

## Features

- **データベース検出**: 起動時にローカルのRekordbox 6/7データベースを自動検出
- **エクスポート**: 選んだプレイリストを単一のパッケージに書き出し
- **同期**: `.rsync` ファイルを開いてローカルデータベースへ取り込み
  - 楽曲は任意の **専用フォルダに展開**。
  - 同名プレイリストは **統合 / 別名で作成 / スキップ** から選択。
  - キュー・タグ・解析データの取り込みは個別に ON/OFF 可能。
  - 解析データを取り込むと、取り込み先で **再解析なしに波形・ビートグリッドを復元**。
- **デスクトップアプリ**: Tauri 製。Windows / macOS / Linux 向けにビルドできる。

## 構成

画面は Tauri（Rust + Web）。Rekordbox DB とパッケージの操作も Rust で、アプリ本体と同じプロセスで動く。
Rekordbox DB の操作は [pyrekordbox](https://github.com/dylanljones/pyrekordbox) のうち本アプリが使う機能を Rust へ移植したもの。

```
web/                       画面（TypeScript + Vite）
src-tauri/src/
  main.rs                  Tauri の起動
  backend.rs               画面から呼ばれる処理の窓口（backend_call）
  exporter.rs / importer.rs  書き出し・取り込み
  package.rs / models.rs   パッケージ（.rsync）の読み書きとマニフェストの形式
  rekordbox/               master.db（SQLCipher）・ANLZ・masterPlaylists6.xml の操作
```

## Usage

### Export

1. 「エクスポート」タブで **プレイリストを読み込む**
2. 持ち出したいプレイリストを選択する
3. **選択をエクスポート…** で保存

### Import

1. 「インポート」タブで **パッケージを開く…**
2. 楽曲の **展開先フォルダ** と **同名プレイリストの扱い** を設定
3. 取り込むプレイリストにチェックして **選択を取り込む**

> **注意**: 取り込みはローカルの Rekordbox データベースに書き込みます。
> 実行前に **Rekordbox を終了** し、データベースのバックアップを取ることを推奨します。

## パッケージ形式（`.rsync`）

中身は ZIP で、以下の構成です。

```
manifest.json              メタデータ（プレイリスト・楽曲・キュー・タグ）
tracks/0001/<file>         楽曲の生ファイル（無加工コピー）
tracks/0002/<file>
analysis/0001/ANLZ0000.*   解析データ（波形・ビートグリッド。楽曲と同じ連番）
...
```

`manifest.json` のスキーマは [src-tauri/src/models.rs](src-tauri/src/models.rs) を参照。
旧 Python 版で作ったパッケージもそのまま読める。

## Development

必要なもの: Node.js + pnpm、Rust、Perl（Windows では [Strawberry Perl](https://strawberryperl.com/)）

```bash
pnpm install     # フロントエンド依存関係
pnpm app:dev     # 開発モードで起動
```

`master.db` は SQLCipher で暗号化されているため、OpenSSL をソースからビルドしてアプリに同梱する
（rusqlite の `bundled-sqlcipher-vendored-openssl`）。OpenSSL のビルドに Perl が必要で、
Windows では Git 付属の Perl は使えない。Git Bash から実行する場合は、
`export OPENSSL_SRC_PERL='C:\Strawberry\perl\bin\perl.exe'` で Strawberry Perl を指定する
（PowerShell では通常そのまま見つかる）。初回ビルドは OpenSSL のコンパイルで数分かかる。

環境変数 `REKORDSYNC_DB_PATH` に `master.db` のパスを指定すると、自動検出より優先して使う。

### テスト

```bash
cd src-tauri
cargo test                     # 単体テスト
cargo test -- --ignored        # 実機の DB を使う検証（src/real_db_tests.rs の説明を参照）
```

## Build

```bash
pnpm app:build
```

| 出力                                                      | 内容                               |
|-----------------------------------------------------------|------------------------------------|
| `src-tauri/target/release/rekordsync(.exe)`               | アプリ本体（単体で動く）           |
| `src-tauri/target/release/bundle/nsis/*-setup.exe`        | Windows インストーラー             |

インストールせずに使う場合は、`rekordsync(.exe)` だけを配布すればよい。
Windows では WebView2 ランタイムが必要（Windows 10/11 には通常入っている）。

> macOS / Linux 向けは `src-tauri/tauri.conf.json` の `bundle.targets` を変更する。

## ライセンスと注意

- 本ツールはRekordboxの暗号化データベースを直接編集します。自己責任でご利用ください。
- RekordboxはAlphaTheta (Pioneer DJ)の商標です。本プロジェクトは非公式です。
- `src-tauri/src/rekordbox/` は [pyrekordbox](https://github.com/dylanljones/pyrekordbox)（MIT License, Copyright (c) Dylan Jones）の処理を Rust へ移植したものを含みます。
