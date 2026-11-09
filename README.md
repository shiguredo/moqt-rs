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
  - 音声は、到着の揺らぎ (直近 2 秒の百分位) と実出力の先行量 (直近 1 秒の p95) から、ジッターバッファの再生遅延を設定した下限 (既定 50 ms) 〜100 ms の範囲で決めます
  - 間に合わない音は波形の周期で少しずつ詰め、再同期で浅くなった深さは周期の重ね合わせで伸ばして戻します
  - 欠けた区間は、直前の音の周期補間と線形予測で埋めます
  - 映像は、表示時刻に合わせて表示待ちのキューからフレームを選びます
  - 音声と映像の表示時刻のずれを補正し、ライブ配信から遅れたら追いつきます
- C4M の認可トークン (CAT / CWT) の検証と発行
  - `moqt` クレームの scope 認可、bearer トークンの接続束縛、CWT 形式の DPoP proof に対応します
  - 同梱の暗号実装 (`AwsLcRsCrypto`) での署名検証と発行には、オプション機能 `aws-lc-rs` が必要です
- 対応仕様
  - [Media over QUIC Transport](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-23) `draft-23`
  - [Low Overhead Container](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-05) `draft-05`
  - [MOQT Streaming Format](https://datatracker.ietf.org/doc/html/draft-ietf-moq-msf-01) `draft-01`
  - [Authorization scheme for MOQT using Common Access Tokens](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-02) `draft-02`

## 使い方

### MOQT のメッセージを扱う

MOQT では、接続の確立や購読の開始などを「制御メッセージ」という単位でやり取りします。
このライブラリは、メッセージをバイト列に変換して送信できるようにし、受信したバイト列からメッセージを取り出せるようにします。

```rust,no_run
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::{message::{ControlMessage, Setup}, parameter::SetupOptions};
use shiguredo_moqt::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE;

fn main() -> Result<(), MessageError> {
    // SETUP メッセージを作成してエンコード
    let msg = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    // peer の SETUP 受信前は既定値 (65535) が送信上限になる
    let bytes = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)?;
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
use shiguredo_moqt::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE;

fn main() -> Result<(), MessageError> {
    // 自側が受信を許容する制御メッセージ長の上限を渡す
    let mut decoder = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);

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
                // 制御ストリームに msg.encode(peer の MAX_CONTROL_MESSAGE_SIZE) を書き出す
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
このライブラリは CAT (CWT / COSE) のデコードと、署名の検証、クレームの検証、`moqt` クレームによる scope の認可、トークンの発行に対応しています。
同梱の暗号実装 (`AwsLcRsCrypto`) で署名の検証と発行を行うには、オプション機能 `aws-lc-rs` が必要です。`CoseCrypto` を自前で実装する場合は必要ありません。

```rust,no_run
use shiguredo_moqt::c4m::cat::{
    BearerReplayCache, BearerVerificationOptions, CatError, CatToken, VerifyOptions,
    MOQT_AUTH_TOKEN_TYPE_CAT,
};
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CoseKey};
use shiguredo_moqt::c4m::MoqtAction;

fn authorize_bearer<C: CoseCrypto>(
    crypto: &C,
    key: &CoseKey,
    token_value: &[u8],
    cache: &mut BearerReplayCache,
    connection_id: &[u8],
) -> Result<bool, CatError> {
    // SETUP や SUBSCRIBE の AUTHORIZATION_TOKEN に載る Type と値
    let token = CatToken::decode_moqt_auth_token(MOQT_AUTH_TOKEN_TYPE_CAT, token_value)?;

    // cnf 付きトークンは bearer 用ではない。DPoP proof と合わせて検証する
    if token.claims().confirmation.is_some() {
        return Ok(false);
    }

    // 非ゼロの moqt-reval は再検証できる場合だけ受理する。ここでは拒否する
    if token.claims().moqt_reval.is_some_and(|value| value != 0.0) {
        return Ok(false);
    }

    // 署名・クレーム・scope 認可・catreplay の接続束縛をまとめて検証する。
    // 信頼済みの鍵、現在時刻、接続識別子は呼び出し側が用意する
    let options = BearerVerificationOptions {
        reference_time_seconds: 1_700_000_000.0,
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
        verify_options: VerifyOptions {
            expected_algorithm: None,
            expected_type: None,
        },
        action: MoqtAction::Subscribe,
        namespace: &[b"example.com"],
        track_name: b"live",
        connection_id,
    };
    match token.verify_bearer(crypto, key, &options, cache) {
        Ok(()) => Ok(true),
        Err(CatError::Claims(_)) | Err(CatError::ActionNotAuthorized(_)) => Ok(false),
        Err(error) => Err(error),
    }
}
```

`CatToken::decode` / `CatToken::decode_moqt_auth_token` が成功しても、それはトークンをデコードできたというだけです。
署名検証だけでも action は認可できません。認可には `moqt` クレームの scope の確認まで必要です。
接続束縛 (`catreplay`) を含む bearer トークンは `CatToken::verify_bearer` を、`cnf` 付きトークンは DPoP proof と合わせて `CwtDpopProof::verify_against_cat_token` を使ってください。
`catreplay` が 1 でない bearer トークンでも `verify_bearer` を使い、接続識別子と replay cache を渡してください。
詳しい責務の分担は [docs/c4m.md](docs/c4m.md) に書いています。

### 音声と映像を再生する

受信した音声と映像を、到着の揺らぎに合わせて鳴らす・表示するための処理です。

- 音声は、到着の揺らぎ (直近 2 秒の百分位) と実出力の先行量 (直近 1 秒の p95) から、再生開始までの遅延を設定した下限 (既定 50 ms) 〜100 ms の範囲で決めます。間に合わない音は波形の周期で少しずつ詰め、再同期で浅くなった深さは周期の重ね合わせで伸ばして戻します
- 欠けた区間は、直前の音の周期補間と線形予測で埋めます
- 映像は、表示したい時刻に合わせて、表示待ちのキューから最も近いフレームを選びます
- 音声と映像は、表示時刻のずれを補正し、ライブ配信から遅れたら追いつきます

```rust,no_run
use shiguredo_moqt::playout::buffer::{
    JITTER_BUFFER_MAX_QUEUED_FRAMES, MAX_PRESENTATION_LAG_US, PlayoutBuffer,
};
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track};

fn main() {
    let mut timeline = PlayoutTimeline::new();

    // 受信側の時刻と、送信側が付けた時刻。どちらもマイクロ秒
    let (wall_clock_us, timestamp_us) = (0, 0);

    // 音声: 復号した音声の時刻を記録すると、鳴らす時刻が返る
    timeline.observe(Track::Audio, wall_clock_us, timestamp_us);
    let play_at_us = timeline.present_us(Track::Audio, timestamp_us);

    // 映像: 復号したフレームを積み、表示したい時刻に最も近いものを 1 枚取り出す。
    // 第 2 引数は目標からの許容幅、第 3 引数は遅れて提示してよい上限 (マイクロ秒)
    let mut buffer: PlayoutBuffer<u32> = PlayoutBuffer::new(JITTER_BUFFER_MAX_QUEUED_FRAMES, Track::Video);
    let _ = buffer.enqueue(0, Some(timestamp_us), &timeline);
    let selection = buffer.select_nearest(
        wall_clock_us,
        5_000,
        MAX_PRESENTATION_LAG_US,
        &timeline,
    );

    let _ = (play_at_us, selection);
}
```

実際に組み込んだ例が [moq-sub](crates/moq-sub/README.md) です。詳しい仕組みと制約は [docs/audio-playout.md](docs/audio-playout.md) に書いています。

## サンプル

実際に動くサンプルとして、配信側の [moq-pub](crates/moq-pub/README.md)、受信側の [moq-sub](crates/moq-sub/README.md)、両者が共有するトランスポート層の [tokio-moq](crates/tokio-moq/README.md) があります。

実行方法、前提、接続設定は [crates/README.md](crates/README.md) を参照してください。

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
