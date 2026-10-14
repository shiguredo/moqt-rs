# moq-sub で datagram 配送の音声を再生できるようにする

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/add-subscriber-datagram-audio
- Polished: {YYYY-MM-DD}

## 目的

`moq-pub` は `--audio-datagram` で音声トラックを OBJECT_DATAGRAM 配送にできるが、`moq-sub` は
datagram を Session へ渡すだけで payload を decode しないため、datagram 配送の音声が再生も
録音もされない。draft-ietf-moq-loc-04 §4.1 (Application with one audio track) が示す
「1 audio chunk = 1 Object = 1 Group の datagram 配送」を購読側でも成立させる。
draft-ietf-moq-transport-22 §2.1 (Objects) は同一 Track 内での Subgroup と Datagram の混在を
許容しており、publisher が datagram を選んでも購読できない理由は無い。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `run` の datagram 受信タスクは
  `handle_for_datagram.recv_datagrams()` で受け取った payload を
  `data_plane_for_datagram.recv_datagram(&payload)` に渡すだけである。戻り値
  (`DatagramAcceptance`) を見ず、payload も decode しない
- `Session` の `recv_object_datagram` はフィルタ再適用・LARGEST_OBJECT の更新・
  gap / Malformed Track の検証を行うだけで、payload をアプリへ返す API を持たない
  (`SessionEvent` にも Object を運ぶ variant は無い)。受信した Object の decode は
  アプリ側の責務である
- 音声のデコードと再生は subgroup stream を読む stream task の `decode_audio_stream` から
  のみ行われ、`audio_tx` (`std::sync::mpsc::Sender<DecodedAudioFrame>`) を経て
  `examples/moq-sub/src/main.rs` の jitter buffer へ流れる。datagram 経由の音声はこの経路に
  入らない
- `examples/moq-sub/README.md` は `--mp4` について「datagram 経由で届いたメディアは対象外」
  と明記している
- datagram の受信経路自体は 3 つの transport で有効になっている
  (`examples/moq-sub/src/pipeline.rs` の `receive_datagrams()` 呼び出し)
- `ObjectDatagram::decode` (`src/stream/datagram.rs`) は payload を decode せず
  `(header, 消費バイト数)` を返すため、payload は消費バイト数以降のスライスとして得られる

## 設計方針

- datagram 受信タスクで `ObjectDatagram::decode` により header と payload を取り出し、
  音声トラック (`track_map` の `TrackKind::Audio`) の Object を subgroup stream と同じ
  配送処理へ流す
- 配送処理 (録画 → Audio Config の検証 → デコード → `audio_tx` 送出) を
  `decode_audio_stream` から切り出し、両経路が同じ関数を通るようにする。datagram 経路と
  stream 経路で挙動が食い違わないようにするためである
- Session への通知 (`recv_datagram`) は従来どおり行い、応答が
  `DatagramAcceptance::Object(TrackDataAcceptance::Accepted)` 以外の Object は
  メディアとして扱わない (フィルタ不通過・キャンセル済みの Object を再生しない)
- 映像 / カタログの datagram は対応しない。LOC の映像は 1 group = 1 unidirectional stream で
  配送され (draft-ietf-moq-loc-04 §4.2)、`moq-pub` も映像を datagram で送らない。届いた場合は
  トラックごとに 1 回だけ警告する
- LOC Properties の書式違反は stream 経路と同じくセッション終了を依頼する
  (draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) の MUST)

## 完了条件

- `moq-pub --audio-datagram` が配信した音声が `moq-sub` でデコードされ、`audio_tx` に
  `DecodedAudioFrame` が届くこと (実 relay の E2E テスト)
- `--mp4` 指定時に datagram 配送の音声が MP4 に保存されること
- フィルタ不通過の Object を配送しないこと、映像 / カタログの datagram を警告して読み飛ばす
  こと、書式違反でセッション終了を依頼することが単体テストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

`moq-sub` の datagram 受信タスクで payload を decode し、subgroup stream と同じ音声の配送経路へ
流すようにした。

- `examples/moq-sub/src/pipeline.rs` の datagram 受信タスクは、`recv_datagram` で Session に
  wire の検証を行わせたうえで、同じ datagram を `ObjectDatagram::decode` で decode して
  payload を取り出す。`DatagramAcceptance::Object(TrackDataAcceptance::Accepted)` 以外
  (Padding Datagram・フィルタ不通過・キャンセル済み subscription) は配送しない。
  アプリ側で decode を代行せず raw を Session へ渡すのは、wire の検証順序と終了コードを
  Session に一本化するためである
- `AudioDelivery` を追加し、録画 → Audio Config (OpusHead) の検証 → デコード →
  `audio_tx` への送出を 1 か所にまとめた。`decode_audio_stream` もこの関数を通るため、
  subgroup stream と datagram で順序と挙動が食い違わない。Audio Config の処理済みフラグは
  購読で共有し、どちらの経路で届いても 1 回だけ検証する
- `plan_datagram` が配送対象を決める。Object Status のみの Object と未知の alias は破棄し、
  映像 / カタログの datagram はトラック alias ごとに 1 回だけ警告する
- 終了時に datagram 受信タスクを abort + join する。このタスクが `audio_tx` を保持したままだと
  プレイヤーが音声チャネルの切断を観測できず終了できないためである
- テストを追加した。単体テスト 5 件 (実 Opus エンコーダで生成したパケットのデコード、実
  `Recorder` で書き出した MP4 の demux によるサンプル確認、書式違反でのセッション終了依頼、
  Object Status のみ / 未知 alias の破棄、映像 / カタログの非対応報告) はモックを使わず
  実物のエンコーダ / デコーダ / 録画で検証する。E2E テスト
  `publish_and_subscribe_audio_datagram_roundtrip` (`e2e-tests/tests/roundtrip.rs`) は
  `moq-pub --audio-datagram` と `moq-sub` の往復で音声フレームの継続受信・PTS の進行・
  MP4 への保存を検証し、実 relay が必要なため `#[ignore]` としている
  (relay を用意した環境で `cargo test -p e2e-tests -- --ignored` を実行する)
- `examples/moq-sub/README.md` の `--mp4` の記述から「datagram 経由で届いたメディアは対象外」
  を外し、datagram 配送の音声の節を追加した。`e2e-tests/README.md` に 2 つ目のテストを追記した

`cargo test --workspace` (moq-sub 149 件、うち新規 5 件)、
`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、
`prek run --all-files` が通ることを確認した。E2E テストはこの環境に relay が無いため未実行である。
