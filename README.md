# moqt-rs

[![shiguredo_moqt](https://img.shields.io/crates/v/shiguredo_moqt.svg)](https://crates.io/crates/shiguredo_moqt)
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

Media over QUIC Transport (MOQT) を Rust で利用するためのライブラリです。MOQT は、QUIC と WebTransport の上で動作する、配信と購読のためのプロトコルです。

MOQT のクライアントやサーバーを実装する場合に利用できます。

## 特徴

- Sans I/O で実装しており、ネットワーク、タイマー、スレッドを扱いません
- no_std に対応しており、`alloc` だけで動作します
- 依存は `hashbrown` / `noflate` / `nojson` / `base64ct` のみです
- インクリメンタルデコーダー
  - 受信バッファにデータを追加しながら、メッセージやオブジェクトを 1 つずつデコードできます
- FETCH 応答の差分圧縮エンコーダー
  - 同じ値の繰り返しを省いて、FETCH 応答を小さくエンコードできます
- 受信側の再生
  - 音声は、到着の遅れからジッターバッファの目標遅延を自動で調整します
  - 間に合わない音は波形の周期で少しずつ詰め、欠けた区間は直前の音を繰り返して補間します
  - 映像は、表示時刻に合わせて表示待ちのキューからフレームを選びます
  - 音声と映像の表示時刻のずれを補正し、ライブ配信から遅れたら追いつきます
- C4M の署名検証と発行
  - オプション機能 `aws-lc-rs` で有効になります
- 対応仕様
  - [Media over QUIC Transport](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-22) `draft-22`
  - [Low Overhead Container](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04) `draft-04`
  - [MOQT Streaming Format](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01) `draft-01`
  - [Authorization scheme for MOQT using Common Access Tokens](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-01) `draft-01`

## 使い方

### MOQT のメッセージを扱う

MOQT では、接続の確立や購読の開始などを「制御メッセージ」という単位でやり取りします。
このライブラリは、メッセージをバイト列に変換して送信できるようにし、受信したバイト列からメッセージを取り出せるようにします。

```rust,no_run
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::{message::{ControlMessage, Setup}, parameter::SetupOptions};

fn main() -> Result<(), MessageError> {
    // SETUP メッセージを作成してエンコード
    let msg = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let bytes = msg.encode()?;
    // bytes を制御ストリームに書き出す...

    // 受信バイト列からデコード
    let (decoded, consumed) = ControlMessage::decode(&bytes)?;
    let _ = (decoded, consumed);
    Ok(())
}
```

### 受信したデータを少しずつデコードする

ネットワークからは、メッセージやオブジェクトが一度に全部届くとは限りません。
受信した分をバッファに追加しながら、メッセージが揃うたびに 1 つずつ取り出せます。データストリームのオブジェクトも同じ方法で扱えます。

```rust,no_run
use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::error::MessageError;

fn main() -> Result<(), MessageError> {
    let mut decoder = MessageDecoder::new();

    // 受信したバイト列を追加する
    let data: &[u8] = &[];
    decoder.push(data);

    // メッセージが揃うたびに 1 つずつ取り出す
    while let Some(msg) = decoder.try_decode_message()? {
        // msg を Session::recv_control 等に渡す
        let _ = msg;
    }
    Ok(())
}
```

### 接続の状態を管理する

接続の確立、購読の開始と終了、データの到着など、MOQT の接続ではさまざまな状態を管理する必要があります。
このライブラリの `Session` がその状態を管理し、アプリケーションは「次に何を送るべきか」をイベントとして受け取ります。

```rust,no_run
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{SessionError, SessionEvent, Transport};

fn main() -> Result<(), SessionError> {
    let mut session = Session::new_client(Transport::Quic, SetupOptions::new())?;

    // 送信が必要なものはイベントとして取り出せる。I/O 層がそれに従って書き出す
    while let Some(event) = session.poll_event() {
        match event {
            SessionEvent::SendControl(msg) => {
                // 制御ストリームに msg.encode() を書き出す
                let _ = msg;
            }
            SessionEvent::Established => {
                // 両側の SETUP が完了
            }
            SessionEvent::CloseSession(e) => {
                // QUIC CONNECTION_CLOSE 等で接続を閉じる
                let _ = e;
            }
            _ => {}
        }
    }
    Ok(())
}
```

### LOC のプロパティを扱う

LOC は Low Overhead Container の略で、音声や映像のデータを運ぶための軽量なコンテナ形式です。タイムスタンプやコーデックの設定などをプロパティとして持ちます。
このライブラリは、これらのプロパティのエンコードとデコードに対応しています。

```rust,no_run
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::loc::{LocProperties, LocProperty, LocPropertyValue, PROP_TIMESTAMP};

fn main() -> Result<(), MessageError> {
    // Object のプロパティ。タイムスタンプ (マイクロ秒) を持たせる
    let mut properties = LocProperties::new();
    properties.push(LocProperty {
        prop_id: PROP_TIMESTAMP,
        value: LocPropertyValue::VarInt(1_700_000_000_000_000),
    });

    // Object Properties のバイト列に変換する
    let bytes = properties.encode()?;

    // 受信したバイト列からプロパティを読み出す
    let (decoded, _) = LocProperties::decode(&bytes)?;
    for property in decoded.iter() {
        // タイムスタンプなどの ID と値を取り出せる
        let _ = (property.prop_id, &property.value);
    }
    Ok(())
}
```

### MSF のカタログ、タイムライン、URI を扱う

MSF は MOQT Streaming Format の略で、MOQT で音声や映像を配信するための形式です。メディアのパッケージ方法、カタログ、タイムライン、配信の手順を定めます。

このライブラリは、カタログ、メディアタイムライン、イベントタイムラインの JSON を読み書きできます。URI と fragment の解釈にも対応しています。

```rust,no_run
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::msf::MsfCatalogDocument;

fn main() -> Result<(), MessageError> {
    // 受信したカタログの JSON。配信するトラックの一覧などを含む
    let json: &[u8] = br#"{"version": "draft-01", "tracks": []}"#;

    // 全体カタログと差分更新を自動で判別してデコードする
    let doc = MsfCatalogDocument::decode(json)?;

    // 全体カタログなら、トラックの名前とコーデックを一覧できる
    if let MsfCatalogDocument::Full(catalog) = &doc {
        for track in &catalog.tracks {
            let _ = (&track.name, &track.codec);
        }
    }

    // JSON に戻す
    let encoded = doc.encode()?;
    let _ = encoded;
    Ok(())
}
```

### C4M の認可トークンを扱う

C4M は Common Access Token for MOQT の略で、MOQT で配信や購読を許可するための認可トークンです。
このライブラリは、トークンのデコードと、署名の検証、トークンの発行に対応しています。署名の検証と発行には、オプション機能 `aws-lc-rs` を有効にする必要があります。

```rust,no_run
use shiguredo_moqt::c4m::cat::{CatError, CatToken, MOQT_AUTH_TOKEN_TYPE_CAT};

fn main() -> Result<(), CatError> {
    // SETUP や SUBSCRIBE の AUTHORIZATION_TOKEN に載る Type と値
    let token_type = MOQT_AUTH_TOKEN_TYPE_CAT;
    let token: &[u8] = b"";

    // C4M トークンとしてデコードする
    let decoded = CatToken::decode_moqt_auth_token(token_type, token)?;

    // 発行者や有効期限などのクレームを確認できる
    let claims = decoded.claims();
    let _ = (&claims.issuer, claims.expiration);
    Ok(())
}
```

### 音声と映像を再生する

受信した音声と映像を、到着の揺らぎに合わせて鳴らす・表示するための処理です。

- 音声は、到着の遅れと実際の再生結果を見て、再生開始までの遅延を自動で調整します。間に合わない音は波形の周期で少しずつ詰め、欠けた区間は直前の音を繰り返して埋めます
- 映像は、決めた表示時刻に合わせて、表示待ちのキューからフレームを選びます
- 音声と映像は、表示時刻のずれを補正し、ライブ配信から遅れたら追いつきます

```rust,no_run
use shiguredo_moqt::playout::buffer::{JITTER_BUFFER_MAX_QUEUED_FRAMES, PlayoutBuffer};
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track};

fn main() {
    let mut timeline = PlayoutTimeline::new();

    // 受信側の時刻と、送信側が付けた時刻。どちらもマイクロ秒
    let (wall_clock_us, timestamp_us) = (0, 0);

    // 音声: 復号した音声の時刻を記録すると、鳴らす時刻が返る
    timeline.observe(Track::Audio, wall_clock_us, timestamp_us);
    let play_at_us = timeline.present_us(Track::Audio, timestamp_us);

    // 映像: 復号したフレームを積み、表示時刻が来たものを取り出す
    let mut buffer: PlayoutBuffer<u32> = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    let _ = buffer.enqueue(0, Some(timestamp_us), &timeline);
    let selection = buffer.select(wall_clock_us, &timeline);

    let _ = (play_at_us, selection);
}
```

実際に組み込んだ例が [moq-sub](examples/moq-sub/README.md) です。詳しい仕組みと制約は [docs/audio-playout.md](docs/audio-playout.md) に書いています。

## サンプル

実際に動くサンプルとして、配信側の [moq-pub](examples/moq-pub/README.md)、受信側の [moq-sub](examples/moq-sub/README.md)、両者が共有するトランスポート層の [tokio-moq](examples/tokio-moq/README.md) があります。

実行方法、前提、接続設定は [examples/README.md](examples/README.md) を参照してください。

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
