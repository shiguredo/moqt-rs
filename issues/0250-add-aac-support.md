# moq-pub / moq-sub で AAC-LC を扱えるようにする

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
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

{対応時に記入}
