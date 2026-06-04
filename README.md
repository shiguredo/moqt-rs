# moqt-rs

[![shiguredo_moqt](https://img.shields.io/crates/v/shiguredo_moqt.svg)](https://crates.io/crates/shiguredo_moqt)
[![Documentation](https://docs.rs/shiguredo_moqt/badge.svg)](https://docs.rs/shiguredo_moqt)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Discord](https://img.shields.io/badge/Discord-%235865F2.svg?logo=discord&logoColor=white)](https://discord.gg/shiguredo)

> [!NOTE]
> ブラウザ上で動作する MOQT 実装は [shiguredo/moqt-js](https://github.com/shiguredo/moqt-js) で公開しています。

> [!WARNING]
> このライブラリは開発中であり、仕様が積極的に変更される場合があります。

## About Shiguredo's open source software

We will not respond to PRs or issues that have not been discussed on Discord. Also, Discord is only available in Japanese.

Please read <https://github.com/shiguredo/oss> before use.

## 時雨堂のオープンソースソフトウェアについて

利用前に <https://github.com/shiguredo/oss> をお読みください。

## 概要

Rust で実装した Sans I/O かつ no_std 対応の Media over QUIC Transport (MOQT) ライブラリです。
MOQT / LOC / MSF の codec と、1 本の Transport Session に閉じたセッション状態機械を提供します。

## 特徴

- Sans I/O
- no_std 対応
- 依存は `hashbrown` / `noflate` / `nojson` の 3 つのみ
- 1 本の Transport Session に閉じた endpoint-local な `Session` 状態機械
  - I/O、非同期処理、relay 固有の routing / fan-out / cache / policy は含まない
- バッファ付きインクリメンタルデコーダー (`MessageDecoder` / `SubgroupStreamDecoder` / `FetchStreamDecoder`)
- FETCH 応答のデルタ圧縮エンコーダー (`FetchStreamEncoder`)
- 対応仕様
  - Media over QUIC Transport (MOQT) `draft-21`
  - Low Overhead Container (LOC) `draft-04`
  - MOQT Streaming Format (MSF) `draft-01`

## 使い方

### 制御メッセージの encode / decode

`ControlMessage` は `Type (varint) | Length (u16 big-endian) | Message Body` 形式で encode / decode します。

```rust
use shiguredo_moqt::{message::{ControlMessage, Setup}, parameter::SetupOptions};

// SETUP メッセージを作成してエンコード
let msg = ControlMessage::Setup(Setup {
    options: SetupOptions::new(),
});
let bytes = msg.encode()?;
// bytes を制御ストリームに書き出す...

// 受信バイト列からデコード
let (decoded, consumed) = ControlMessage::decode(&bytes)?;
```

### インクリメンタルデコード

QUIC / WebTransport のフレーム境界とメッセージ境界は一致しないため、受信バッファに蓄積しながらデコードします。

```rust
use shiguredo_moqt::decoder::MessageDecoder;

let mut decoder = MessageDecoder::new();
// 受信データを追記する
// decoder.push(&data);
if let Some(msg) = decoder.try_decode_message()? {
    // msg を Session::recv_control 等に渡す
}
```

Data stream も同じ push + try_decode パターンです。

- `SubgroupStreamDecoder`：`try_decode_header()` でヘッダー、`try_decode_object()` でオブジェクトを順次デコードし、ペイロードは `try_read_payload()` / `consume_payload()` で進める
- `FetchStreamDecoder`：FETCH 応答のデコード
- `FetchStreamEncoder`：FETCH 応答のデルタ圧縮エンコード

### セッション (SETUP 確立とイベント駆動)

`Session` は I/O を持たない純粋な状態機械です。
`new_client` / `new_server` で生成し、`poll_event()` で取り出した `SessionEvent::SendControl` 等を I/O 層に渡して送信します。

```rust
use shiguredo_moqt::{session::{core::Session, types::{SessionEvent, Transport}}, parameter::SetupOptions};

let mut session = Session::new_client(Transport::Quic, SetupOptions::new())?;
while let Some(event) = session.poll_event() {
    match event {
        SessionEvent::SendControl(msg) => {
            // 制御ストリームに msg.encode() を書き出す
        }
        SessionEvent::Established => {
            // 両側の SETUP が完了
        }
        SessionEvent::CloseSession(e) => {
            // QUIC CONNECTION_CLOSE 等で接続を閉じる
        }
        _ => {}
    }
}
```

受信側：

- bidi request stream の先頭メッセージ：`recv_request()`
- bidi request stream の応答：`recv_stream_message(request_id, msg)`
- uni data stream：`recv_data_stream_type()` / `recv_subgroup_header()` / `recv_subgroup_object()` / `recv_fetch_header()`
- datagram：`recv_object_datagram()`

送信側：

- `send_subscribe()` / `send_publish()` / `send_fetch()` 等
- `tick(now_ms)` で GOAWAY drain や delivery timeout の期限を進める

### カタログとタイムライン (MSF)

```rust
use shiguredo_moqt::msf::MsfCatalogDocument;

// Full / Delta を自動判別してデコード
let doc = MsfCatalogDocument::decode(&bytes)?;
let bytes = doc.encode()?;
```

Media / Event Timeline は `msf::{encode_media_timeline / decode_media_timeline / encode_event_timeline / decode_event_timeline}` で JSON と相互変換します。
`TimelineEncodingOptions { gzip: true }` で gzip 圧縮し、デコード時は gzip magic (`0x1F 0x8B`) を検出して自動展開します。

## 実装状況

MOQT / LOC / MSF の対応仕様、コントロールメッセージ、パラメータ、プロパティ、モジュール構成は [`docs/IMPLEMENTATION.md`](docs/IMPLEMENTATION.md) を参照してください。

## サンプル

サンプルは [Tokio](https://github.com/tokio-rs/tokio)、[s2n-quic](https://github.com/aws/s2n-quic)、[shiguredo_http3](https://github.com/shiguredo/http3-rs) を利用しています。
引数のパースには [noargs](https://github.com/sile/noargs)、ログには tracing を利用しています。

- `moqt-publisher`：QUIC / WebTransport で relay に接続し、video / audio / `catalog` track を PUBLISH する
- `moqt-subscriber`：QUIC / WebTransport で relay に接続し、`catalog` を FETCH して video / audio を SUBSCRIBE して再生する
- `moqt-transport`：publisher / subscriber が共有する QUIC / WebTransport over HTTP/3 トランスポート層 (ライブラリ)

前提条件は Rust 1.94 以降です。
`moqt-publisher` の依存が 1.94 を要求するためです。
`moqt-subscriber` / `moqt-transport` だけであれば 1.93 で構築できます。

```bash
# publisher (疑似キャプチャ)
cargo run -p moqt-publisher -- --url moqt://127.0.0.1:4443 --fake-capture-device

# subscriber
cargo run -p moqt-subscriber -- --url moqt://127.0.0.1:4443
```

接続先となる MoQT relay は別途用意してください。
詳細なオプションは [`examples/README.md`](examples/README.md) と各クレートの `--help` を参照してください。

## Agent Skills

[Agent Skills](https://agentskills.io/) 形式のスキルを同梱しています。
`gh skill install` コマンドで対応する AI エージェント (Claude Code, Cursor, GitHub Copilot, Gemini CLI 等) にインストールできます。
エージェントはこのライブラリの API や準拠仕様を理解した上で支援できます。

```bash
gh skill install shiguredo/moqt-rs shiguredo-moqt
```

スキルの内容は [`skills/shiguredo-moqt/SKILL.md`](skills/shiguredo-moqt/SKILL.md) を参照してください。

## 規格書

このライブラリが準拠している主な IETF draft です。

- [draft-ietf-moq-transport-21](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-21)：Media over QUIC Transport
- [draft-ietf-moq-loc-04](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04)：Media over QUIC - Low Overhead Container
- [draft-ietf-moq-msf-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01)：MOQT Streaming Format

## ビルドとテスト

```bash
# 全テスト実行
make test

# PBT のみ実行
make pbt

# カバレッジ付きで全テスト実行
make cover

# Fuzzing を全ターゲットで 30 秒ずつ実行
make fuzzing

# clippy
make clippy

# フォーマット
make fmt

# ビルド確認
make check
```

## ライセンス

Apache License 2.0

```text
Copyright 2026 Shiguredo Inc.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```
