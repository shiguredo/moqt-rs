# moqt-subscriber に --audio-output-device を追加して音声出力を無効にできるようにする

- Created: 2026-09-25
- Completed: {YYYY-MM-DD}
- Branch: feature/add-audio-output-device
- Polished: {YYYY-MM-DD}
- Reporter: @voluntas

## 目的

リレーへの接続で映像と音声の経路を検証するとき、音声トラックは受信・デコードしたままスピーカーへ音を出したくない場合がある。現状は音声トラック自体を止める `--no-audio` しか無く、受信経路を残したまま出力だけを止められない。出力だけを切り替える CLI オプションを追加する。

## 現状

- `examples/moqt-subscriber/src/cli.rs` の `Config` は `--no-video` / `--no-audio` のみを持ち、音声の出力先を指定できない
- `examples/moqt-subscriber/src/main.rs` の `run_raw_player` は無条件に `raw_player::AudioPlayer::new()` を作り、`play()` で SDL のデフォルト出力デバイスを開く。音を出したくない場合は `SDL_AUDIODRIVER=dummy` のような環境変数に頼るしかない
- `raw_player` 2026.2 の `AudioPlayer` はデフォルト出力デバイスしか開けず、デバイス名の指定や無効化を受け付けない

## 設計方針

- `--audio-output-device <DEVICE>` を追加し、`default` (SDL のデフォルト出力デバイス、既定) と `none` (出力しない) の 2 値だけを受理する。`raw_player` がデバイス名を扱えないため、それ以外の値はエラーとして拒否する
- `none` のときは `AudioPlayer` を生成しない。音声トラックの受信とデコードは継続し、チャンク数のログで出力の有無が分かるようにする
- 音声トラック自体を止める `--no-audio` はそのまま残し、独立した指定として扱う

## 完了条件

- `--audio-output-device none` で音声を出力せずに映像と音声の受信・デコードが継続すること
- `--audio-output-device default` および未指定で従来どおり出力されること
- 未対応の値がエラーになることが単体テストで固定されていること
- `examples/README.md` にオプションが記載されていること
- `cargo test --workspace` と `cargo clippy --all-targets` が通ること
