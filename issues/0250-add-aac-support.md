# moq-pub / moq-sub で AAC-LC を扱えるようにする

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/add-aac-support
- Polished: {YYYY-MM-DD}

## 目的

example の音声は Opus のみに対応している。AAC-LC を扱えるようにし、AAC しか持たない再生環境との
相互接続と、音声コーデックの選択肢を増やす。

## 現状

- `examples/moq-pub/src/cli.rs` に音声コーデックのオプションが無く、音声は Opus 固定である
  (`examples/moq-pub/src/encoder/opus.rs` が catalog の codec 文字列 `opus` を返し、
  `PROP_AUDIO_CONFIG` に OpusHead を載せる)
- `examples/moq-sub/src/decoder.rs` の音声デコーダは `OpusDecoder` 固定であり、
  `examples/moq-sub/src/pipeline.rs` の複数の関数シグネチャが `OpusDecoder` を直接受け取る
- `examples/moq-sub/src/pipeline.rs` の `PROP_AUDIO_CONFIG` の解釈は OpusHead 前提である
  (`parse_opus_head` / `validate_audio_config`)
- library 側の `src/msf.rs` の `is_audio_codec` は `mp4a.*` を audio codec として既に認識しており、
  catalog の検証は通る。MSF が要求する `samplerate` / `channelConfig` も example が送出済みである
- AAC のエンコード / デコードは Apple の Audio Toolbox を使う `shiguredo_audio_toolbox`
  (audio-toolbox-rs) で行う。**macOS 専用**であり、`Encoder` / `Decoder` は `Send` を実装しない

## 設計方針

- AAC は **AAC-LC のみ** を対象とする (`shiguredo_audio_toolbox` の `EncoderCodec::AacLc`)。
  macOS 限定の機能とし、`[target.'cfg(target_os = "macos")'.dependencies]` に依存を置く
- catalog の codec 文字列は `mp4a.40.2` とする (`mp4a` は MPEG-4 Audio、`40` は object type の
  16 進表記、`2` は AAC-LC。RFC 6381 §3.3)
- `PROP_AUDIO_CONFIG` には AudioSpecificConfig (ISO/IEC 14496-3 §1.6.2.1) を載せる。
  audioObjectType (5 bit) / samplingFrequencyIndex (4 bit) / channelConfiguration (4 bit) と
  GASpecificConfig の 3 bit を合わせた 2 バイトであり、サンプルレートとチャンネル数から構築できる
- AAC-LC の 1 フレームは 1024 サンプル固定である (ISO/IEC 14496-3 §4.5.2.1)。
  48 kHz では 21.33 ms に相当し、Opus の 20 ms (960 サンプル) とは異なる
- `AudioEncoder` / `AudioDecoder` の enum を追加し、catalog の codec 文字列または CLI から
  実装を選ぶ。`OpusDecoder` 固定の関数シグネチャを enum に置き換える
- `shiguredo_audio_toolbox` の `Encoder` / `Decoder` は `Send` ではない。publisher は
  `pipeline::run` の future を直接 await するため移動は不要だが、subscriber は
  `tokio::spawn` したストリームタスクでデコーダを共有するため、**専用スレッドが実体を所有し
  `tokio::sync::mpsc` と `oneshot` で要求 / 応答する Send なハンドル**にする
- AAC では `PROP_AUDIO_CONFIG` を OpusHead として解釈しない。catalog の codec が opus のときだけ
  OpusHead のパースと検証を行う
- Audio Toolbox のデコーダ出力は **16-bit ステレオ固定** である (入力がモノラルでもステレオに
  アップミックスされる)。カタログの `channelConfig` は入力 (符号化) のチャンネル数として
  デコーダに渡し、再生に渡すチャンネル数はデコーダの出力に合わせる

## 完了条件

- `moq-pub --audio-codec aac` で AAC-LC の音声をエンコードして配信できること (macOS のみ)
- catalog の audio track の codec が `mp4a.40.2` になり、MSF の検証を通ること
- `PROP_AUDIO_CONFIG` に AudioSpecificConfig が載ること
- moq-sub が catalog の `mp4a.*` から AAC-LC デコーダを生成し、PCM を再生できること (macOS のみ)
- AAC では OpusHead の検証を行わないこと
- AAC のエンコード → デコードのラウンドトリップテストが examples にあること
- macOS 以外では AAC を選ぶとエラーになること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
- ソースコードのコメントに issue 番号や issue への言及を書かないこと

## 解決方法

Apple Audio Toolbox のバインディングである `shiguredo_audio_toolbox` を使い、AAC-LC の
エンコード (moq-pub) とデコード再生 (moq-sub) に対応した。macOS 限定である。

### 変更内容

- `examples/moq-pub/Cargo.toml` / `examples/moq-sub/Cargo.toml`:
  `[target.'cfg(target_os = "macos")'.dependencies]` に `shiguredo_audio_toolbox` を追加する
- `examples/moq-pub/src/encoder/aac.rs`: `AacEncoder` (専用スレッド + チャネル越しのハンドル) と
  `audio_specific_config()` を追加する
- `examples/moq-pub/src/encoder/audio.rs`: `AudioEncoder` enum を追加し、`encode` /
  `samples_per_frame` / `catalog_codec_string` / `audio_config` を codec ごとに分岐する
- `examples/moq-pub/src/encoder.rs`: `aac` (macOS のみ) と `audio` のモジュールを追加する
- `examples/moq-pub/src/encoder/opus.rs`: `build_opus_head()` を pipeline から移す
- `examples/moq-pub/src/cli.rs`: `--audio-codec opus|aac` と `AudioCodec` を追加する
- `examples/moq-pub/src/pipeline.rs`: `AudioEncoder` を codec から生成し、Audio Config を
  エンコーダから取得する。音声の `encode` は `await` する
- `examples/moq-pub/src/error.rs`: `AudioToolbox` variant と `From` 実装を追加する (macOS のみ)
- `examples/moq-sub/src/decoder/aac.rs`: `AacDecoder` (専用スレッド + チャネル越しのハンドル) を
  追加する
- `examples/moq-sub/src/decoder.rs`: `AudioDecoder` enum と `build()` を追加し、`mp4a` 単独または
  `mp4a.` 始まりを AAC として扱う。購読する audio track の選択にも `mp4a.*` を加える
- `examples/moq-sub/src/pipeline.rs`: `audio_decoder` の型を `AudioDecoder` に変え、`decode` を
  `await` する。Audio Config は catalog の codec が opus のときだけ OpusHead として解釈する。
  再生と録画に渡すチャンネル数はデコーダの出力に合わせる
- `examples/moq-sub/src/error.rs`: `AudioToolbox` variant と `From` 実装を追加する (macOS のみ)
- `examples/README.md`: 対応コーデックと `--audio-codec` の記述を更新する

### 判断と根拠

- **AAC-LC のみを対象にする**: `shiguredo_audio_toolbox` のエンコーダが公開しているのは
  `EncoderCodec::AacLc` のみである
- **codec 文字列は `mp4a.40.2`**: `mp4a` は MPEG-4 Audio、`40` は object type の 16 進表記、`2` は
  AAC-LC を表す (RFC 6381 §3.3)。`src/msf.rs` の `is_audio_codec` が `mp4a.*` を認識済みであり、
  library 側の変更は不要である
- **Audio Config は AudioSpecificConfig (2 バイト)**: ISO/IEC 14496-3 §1.6.2.1 の
  audioObjectType (5 bit) / samplingFrequencyIndex (4 bit) / channelConfiguration (4 bit) と
  GASpecificConfig の 3 bit から構築する。48 kHz モノラルでは `0x11 0x88` になる。
  samplingFrequencyIndex の表に無いサンプルレートはエラーにする
- **1 フレームは 1024 サンプル**: AAC-LC のフレーム長は 1024 サンプル固定である
  (ISO/IEC 14496-3 §4.5.2.1)。Opus の 20 ms (960 サンプル) とは異なるため、
  `AudioEncoder::samples_per_frame()` で codec ごとの値を返し、publisher はその値で
  チャンクを切り出す
- **`Send` でないエンコーダ / デコーダは専用スレッドへ移す**: `shiguredo_audio_toolbox` の
  `Encoder` / `Decoder` は `Send` を実装しない。subscriber は `tokio::spawn` したストリーム
  タスクでデコーダを共有し、publisher の future は `e2e-tests` が `tokio::spawn` するため、
  どちらも `Send` なハンドルが要る。実体を専用スレッドが所有し、`tokio::sync::mpsc` と
  `oneshot` で要求 / 応答する形にした (`Drop` で送信側を閉じてから join する)
- **依存は `2026.2.0-canary.2` を使う**: 2026.1.0 の `Decoder` は 1 回の
  `AudioConverterFillComplexBuffer` の中で同じパケットを複数回入力するため、1 パケットに対して
  出力バッファ 1 杯分 (5760 フレーム) の PCM を返し、同じ音声が複数回現れていた
  (実測: 1024 サンプル × 7 パケット相当のデコードで 40320 フレーム)。上流の
  2026.2.0-canary.0 で `packet_provided_in_this_fill` による修正が入っており、これを取り込む
- **AAC の MP4 録画は対象外**: `examples/moq-sub/src/mp4.rs` の `build_audio_entry` は Opus の
  サンプルエントリーを構築し、録画するデータは圧縮 payload そのものであるため、AAC を録画すると
  コンテナの表記と実データが食い違う。音声トラックの選択に `mp4a.*` を加えたことでこの経路に
  入り得るようになったため、`--mp4` と AAC の組み合わせでは音声を録画せず警告する
  (映像のみの MP4 になる)

### テスト

- `examples/moq-pub/src/encoder/aac.rs`: 1 フレームのエンコードで 1 パケットが得られること、
  drop でスレッドが終了すること、Audio Config が `0x11 0x88` (48 kHz モノラル) /
  `0x11 0x90` (ステレオ) になること、表に無いサンプルレートを拒否すること
- `examples/moq-pub/src/cli.rs`: `--audio-codec opus|aac` を解析できること、未対応の codec を
  拒否すること
- `examples/moq-pub/src/catalog.rs`: `mp4a.40.2` の catalog が encode できること
- `examples/moq-pub/src/encoder/opus.rs`: OpusHead のフォーマット (移動元のテスト)
- `examples/moq-sub/src/decoder/aac.rs`: 実エンコーダで作った AAC-LC パケットをデコードして
  PCM が得られること、デコード結果が入力のサンプル数を超えないこと (2026.1.0 の重複不具合の
  回帰検出)、drop でスレッドが終了すること
- `examples/moq-sub/src/decoder.rs`: `opus` → Opus、`mp4a` / `mp4a.40.2` → AAC、未対応の codec を
  拒否すること

### スコープ外 (別 issue)

- AAC の MP4 録画 (`examples/moq-sub/src/mp4.rs` の `build_audio_entry` は Opus のみ)
- `--input-mp4-reencode` の入力音声は従来どおり Opus のみである (出力は AAC にできる)

### 確認

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
- 実 relay での確認は relay を用意できないため未実施である。代替として、エンコード →
  デコードのラウンドトリップ (実 Audio Toolbox を使用。モックなし) と catalog の codec 文字列を
  単体テストで固定した
