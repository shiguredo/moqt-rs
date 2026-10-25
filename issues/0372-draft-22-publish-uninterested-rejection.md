# moq-sub が不要な PUBLISH を UNINTERESTED で拒否する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §9.8 (PUBLISH) は "A subscriber receiving a PUBLISH for a Track it does not wish to receive SHOULD
send PUBLISH_ERROR with error code UNINTERESTED, and abandon reading any publisher initiated streams associated with that
subscription using a STOP_SENDING frame." と SHOULD を課す。PUBLISH_ERROR は §1.5 (Response Message Naming) の shorthand で、wire 上は
request stream 上の REQUEST_ERROR である。UNINTERESTED (0x20) は §12.3 (Request Error Codes) で PUBLISH /
PUBLISH_NAMESPACE への応答用として定義され、コード値は §16.11.2 (Request Error Codes) のレジストリに載る。
ライブラリの定数名は `REQUEST_UNINTERESTED`。moq-sub は受信する PUBLISH をすべて
`REQUEST_NOT_SUPPORTED` で拒否しており、不要な Track の PUBLISH に対する適切なエラーコードになっていない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `ClientEvent::Request` 分岐は `IncomingRequest.message` の種別を見ず、すべての peer
  request を `REQUEST_NOT_SUPPORTED` で拒否する。
- PUBLISH はライブラリが subscriber 役として受理できる request であり、実装していない request ではない。不要な Track の
  PUBLISH は「未対応」ではなく「不要」である。
- draft §9.8 は "The publisher will start transmitting objects immediately, possibly before PUBLISH_OK." と定める。session 層は
  `handle_peer_publish` で PUBLISH 受理時に Track Alias を登録するため、PUBLISH への応答送信前に publisher 起点の data
  stream が開きうる。現状は REQUEST_ERROR 送信で subscription を `Terminated` にするが、open な受信 data stream への
  STOP_SENDING 指示は発行しない。
- `src/session/types.rs` の `SessionEvent::StopSendingDataStream` は、現在 DATA_STREAM_TIMEOUT の期限到達で発行する用途に
  限定した doc になっており、request 拒否時の発行契機が無い。
- `docs/missing-features.md` には moq-pub 側の peer request 拒否と TRACK_STATUS 拒否は記載があるが、moq-sub の PUBLISH
  拒否方針は記載がない。

## 設計方針

- moq-sub で `ControlMessage::Publish` を受けた場合は `REQUEST_ERROR` の `UNINTERESTED` を返す。それ以外の request
  (SUBSCRIBE / FETCH / TRACK_STATUS) は従来どおり `REQUEST_NOT_SUPPORTED` のままとする。
- 応答コードの選択を純関数 (例: `request_rejection_code(message) -> u64`) に切り出し、PUBLISH では
  `REQUEST_UNINTERESTED`、それ以外では `REQUEST_NOT_SUPPORTED` を返すことを unit test で固定する。
- session 層の subscriber 役 subscription が `Pending` から REQUEST_ERROR 送信で `Terminated` になるとき、当該 subscription に
  属する open な受信 data stream を破棄対象へ移し、`SessionEvent::StopSendingDataStream` を発行する。具体的には
  DATA_STREAM_TIMEOUT 経路 (`discard_timed_out_subgroup_stream` / `retain_discarded_stream_id`) と同じ手順で
  `data_stream_last_activity_ms` から外して `retain_discarded_stream_id` し、stream の会計を戻す。これにより
  DATA_STREAM_TIMEOUT の二重発行と、I/O 層が終端を通知しないことによる open 数の残留を防ぐ。
- 対象 stream は「当該 subscription が現在の帰属先である受信 stream」に限る。共有 Track Alias で別の生きた subscription に
  帰属する stream は破棄しない (`discard_cancelled_subgroup_stream` / `remove_incoming_data_streams_for_request` と同じ選別)。
  既に `Discarded` の stream と終端通知済みの stream も対象外とする。
- `StopSendingDataStream` の `error_code` は `STREAM_CANCELLED` (0x1) とする (DATA_STREAM_TIMEOUT 経路と同じ)。
  UNINTERESTED (0x20) は REQUEST_ERROR 専用で §12.5 (Stream Reset Error Codes) に無いため使わない。
- `SessionEvent::StopSendingDataStream` の doc に request 拒否時の発行契機を追記し、`examples/tokio-moq/src/moqt_client.rs` の
  「timed out data stream」固定のログ文言も発行契機を限定しない文言へ更新する。`skills/shiguredo-moqt/SKILL.md` の
  SessionEvent 表も更新する。
- STOP_SENDING フレームの実送出は I/O 層 (moq-sub の `receive_registered_stream_data`) が行い、Session はイベント発行と
  stream の破棄会計までを担う。I/O 層はイベント受信後に `send_data_stream_stop_sending` を呼ぶ必要なし
  (Session 側で破棄対象へ移し、以後の受信・終端を no-op で吸収する)。
- 遅延して開く publisher 起点 data stream (REQUEST_ERROR 送信後に開く in-flight stream) は既存の破棄経路で読み捨てる。
  §9.8 の STOP_SENDING 要求のうち、本 issue は「REQUEST_ERROR 時点で open な stream の abandon」を満たすものとし、
  遅延 stream の受信自体は止めない。`forget_subscription` 後の STOP_SENDING は open の 0318 の範囲であり、
  本 issue では扱わない (0318 実装後に二重発行しないことも確認する)。
- `docs/missing-features.md` に moq-sub の PUBLISH 拒否方針 (UNINTERESTED) を追記する。

## 完了条件

- moq-sub が peer から PUBLISH を受けたとき、`REQUEST_ERROR` の `UNINTERESTED` (0x20) を返すこと。SUBSCRIBE / FETCH /
  TRACK_STATUS は従来どおり `NOT_SUPPORTED` (0x3) を返すこと。応答コード選択の純関数の unit test で固定する。
- subscriber 役 subscription の REQUEST_ERROR 送信時に、open な受信 data stream へ `SessionEvent::StopSendingDataStream` が
  `STREAM_CANCELLED` で発行されること (session 層のテストで検証する)。
- 上記の拒否後、対象 stream が破棄対象の保持集合へ移り、DATA_STREAM_TIMEOUT が二重発行されないこと (同じく session 層の
  テストで、`tick` を期限まで進めても StopSendingDataStream が追加発行されないことを確認する)。
- STOP_SENDING フレームの実送出は I/O 層の責務であることを doc に明記し、`SessionEvent::StopSendingDataStream` の doc に
  request 拒否時の発行契機を追記すること。
- `docs/missing-features.md` に moq-sub の PUBLISH 拒否方針を追記し、`SessionEvent::StopSendingDataStream` の doc・
  `examples/tokio-moq/src/moqt_client.rs` のログ文言・`skills/shiguredo-moqt/SKILL.md` の SessionEvent 表を更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
