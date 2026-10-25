# moq-sub が不要な PUBLISH を UNINTERESTED で拒否する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.8 (PUBLISH) は "A subscriber receiving a PUBLISH for a Track it does not wish to receive SHOULD
send PUBLISH_ERROR with error code UNINTERESTED, and abandon reading any publisher initiated streams associated with that
subscription using a STOP_SENDING frame." と SHOULD を課す。moq-sub は受信する PUBLISH をすべて `REQUEST_NOT_SUPPORTED` で拒否しており、不要な Track の
PUBLISH に対する適切なエラーコードになっていない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `ClientEvent::Request` 分岐は `IncomingRequest.message` の種別を見ず、すべての peer request を
  `REQUEST_NOT_SUPPORTED` で拒否する。
- PUBLISH はライブラリが subscriber 役として受理できる request であり、実装していない request ではない。不要な Track の PUBLISH は「未対応」ではなく「不要」である。
- session 層は受信 PUBLISH への REQUEST_ERROR 応答で subscription を `Terminated` にするが、その時点で open な受信 data stream への STOP_SENDING
  は発行しない。遅延して開く stream は既存の破棄経路で読み捨てる。
- `docs/missing-features.md` には moq-pub 側の peer request 拒否と TRACK_STATUS 拒否は記載があるが、moq-sub の PUBLISH 拒否方針は記載がない。

## 設計方針

- moq-sub で `ControlMessage::Publish` を受けた場合は `REQUEST_ERROR` の `UNINTERESTED` を返す。それ以外の request (SUBSCRIBE / FETCH /
  TRACK_STATUS) は従来どおり `REQUEST_NOT_SUPPORTED` のままとする。
- 応答コードの選択を小さな関数に切り出し、PUBLISH では `UNINTERESTED`、それ以外では `NOT_SUPPORTED` を返すことを unit test で固定する。
- session 層の subscriber 役 subscription が `Pending` から REQUEST_ERROR 送信で `Terminated` になるとき、当該 subscription に属する open な受信 data
  stream に `SessionEvent::StopSendingDataStream` を発行する。これにより §9.8 の STOP_SENDING 要求を満たす。
- 遅延して開く publisher 起点 data stream は既存の破棄経路で読み捨てる。`forget_subscription` 後の STOP_SENDING は別 issue の範囲であり、本 issue では扱わない。

## 完了条件

- moq-sub が peer から PUBLISH を受けたとき、`REQUEST_ERROR` の `UNINTERESTED` (0x20) を返すこと。SUBSCRIBE / FETCH / TRACK_STATUS は従来どおり
  `NOT_SUPPORTED` (0x3) を返すこと。
- subscriber 役 subscription の REQUEST_ERROR 送信時に、open な受信 data stream へ STOP_SENDING が発行されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
