# SUBSCRIBE_TRACKS stream 終端後の PUBLISH stream 終端でセッションを閉じない

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscribe-tracks-stream-end-request-streams
- Polished: 2026-09-10

## 目的

SUBSCRIBE_TRACKS の bidi request stream が終端した後、その stream から確立した PUBLISH の bidi request stream が終端したときに、セッションを誤って閉じないようにする。

## 現状

`src/session/namespace/track_subscription.rs` の `close_track_subscription_on_stream_end` は、`active_track_aliases` に残る alias から `peer_publisher_aliases` を引き、PUBLISH 由来 subscription の状態を `Terminated` にしたうえで `self.request_streams.remove(&sub_request_id)` を呼び、`RequestTerminated` を発行する。

しかし peer は後から PUBLISH の bidi request stream を FIN / RESET で閉じる。`recv_request_stream_closed` (`src/session/core.rs`) は `request_streams` 未登録かつ `rejected_request_ids` 未登録の request id を `PROTOCOL_VIOLATION` としてセッションを閉じるため、正常なプロトコル手順でセッション断になる。

再現手順:

1. SUBSCRIBE_TRACKS を確立する
2. その prefix に一致する PUBLISH を受信して subscription を確立する
3. SUBSCRIBE_TRACKS の bidi request stream を peer が終端する
4. PUBLISH の bidi request stream を peer が終端する
5. 手順 4 で `PROTOCOL_VIOLATION` によりセッションが閉じる

根拠 (draft-ietf-moq-transport-21 §6.4.2.2): FIN は「その方向に送るメッセージが終わった」ことを示すだけで request の cancel ではない。SUBSCRIBE_TRACKS の終端で PUBLISH stream の識別情報を失う根拠は無い。

当該シーケンスのテストは存在しない。

## 設計方針

- `close_track_subscription_on_stream_end` で暗黙終端する PUBLISH 由来 subscription の request_id を、`request_streams` から削除する前に `rejected_request_ids` へ登録する。後続の PUBLISH stream の FIN / RESET は `recv_request_stream_closed` が `rejected_request_ids` 経由で no-op で吸収する。`forget_fetch` がデータストリーム終端済み
  fetch で同様に登録している前例に合わせる。
- `rejected_request_ids` の doc を「REQUEST_ERROR で拒否した request id」に加えて「SUBSCRIBE_TRACKS 終端で暗黙終端した PUBLISH 由来 subscription の request id」も含む旨に更新する。`recv_request_stream_closed` の doc にも同ケースを追記する（`forget_fetch` の前例に合わせ、フィールド doc と API doc の両方を更新する）。
- `RequestTerminated` の `kind` は `request_streams` から取得する。現状は `RequestKind::Publish` 固定で、共有 alias に SUBSCRIBE_OK 由来の subscription が混在すると誤った kind を通知する。
- `RequestTerminated` は SUBSCRIBE_TRACKS 終端で 1 回だけ発行し、後続の PUBLISH stream close では再発行しない（`rejected_request_ids` で吸収されるため）。

## 対象外

- 共有 alias で SUBSCRIBE_OK 由来の subscription まで SUBSCRIBE_TRACKS 終端で終端してしまう問題は、`peer_publisher_aliases` が確立経路を区別しない設計に起因し、本 issue の対象外とする。

## 依存

- 0044 が本 issue を前提としている。両者は同じ `close_track_subscription_on_stream_end` と `recv_request_stream_closed` を変更するため、本 issue を先に取り込む。本修正は `request_streams` の先行削除を維持したまま、`rejected_request_ids` 登録と `kind` の取得順を追加する。

## 完了条件

- 上記再現手順でセッションが閉じないこと
- SUBSCRIBE_TRACKS 終端で暗黙終端した subscription への後続 PUBLISH stream close が no-op で吸収されること
- `RequestTerminated.kind` が `request_streams` に記録された種別と一致すること
- `rejected_request_ids` の doc と `recv_request_stream_closed` の doc が更新されていること
- 当該シーケンスの回帰テストが `tests/test_session/` に追加されていること
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加すること
