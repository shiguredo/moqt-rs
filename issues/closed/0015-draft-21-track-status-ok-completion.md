# TRACK_STATUS_OK の FIN と LARGEST_OBJECT を仕様どおりにする

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/fix-track-status-ok-completion
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS) と §9.20.18 (LARGEST OBJECT Parameter) の MUST を満たす。TRACK_STATUS_OK 後に bidi stream を FIN し、Objects が公開済みなら LARGEST_OBJECT を含める。

## 現状

`src/session/core.rs` の `send_request_ok` は全 context で `SessionEvent::SendOnStream { fin: false }` を積む。TRACK_STATUS は REQUEST_UPDATE を受け付けない "first and only message" であり、`send_request_error` 側は `fin: true` なのに OK 側だけ FIN が欠落している。

同じく `send_ok_for_track_status` は `largest_location: None` 固定で、`effective_largest_object` からの LARGEST_OBJECT 自動注入を行わない。SUBSCRIBE_OK / REQUEST_UPDATE_OK は注入するため非対称。`TRACK_STATUS_OK_ALLOWED_PARAMS` は `[LARGEST_OBJECT]` なので載せられる。

`send_ok_for_track_status` は `parameters` を受け取らないシグネチャ (`&mut self, request_id: u64)`) であり、`send_request_ok` もこの関数に `parameters` を渡していない。LARGEST_OBJECT を注入するには両者の変更が必要になる。

根拠:

- §9.13: "The bidi stream is closed with a FIN after TRACK_STATUS_OK or REQUEST_ERROR are sent."
- §9.20.18: "If Objects have been published on this Track the Publisher MUST include this parameter."

## 設計方針

変更対象と手順:

- `send_request_ok` (`src/session/core.rs`) の `SessionEvent::SendOnStream` 発行で、TRACK_STATUS context のときだけ `fin: true` にする。
- `send_ok_for_track_status` (`src/session/namespace/track_status.rs`) のシグネチャを `(&mut self, request_id: u64, parameters: &mut MessageParameters)` に変更し、`send_request_ok` の dispatch から `&mut parameters` を渡す。
- `send_ok_for_track_status` で対象 Track の largest を取得し、`Some` なら `update_largest_object_in_parameters` (`src/session/subscription/delivery.rs`) で parameters へ注入する。既存値との大きい方を採るため、アプリが parameters に載せた値は下げない。

largest の取得元:

- 既存の `publisher_track_largest` (`src/session/subscription/fill.rs`) を使う。これは `aliases.subscriptions_by_track` の `(track_namespace, track_name, TrackRole::Publisher)` から自側 publisher 役 subscription 群を引き、各 `effective_largest_object` の最大値を返す。
- TRACK_STATUS は Subscription を作らない (draft §9.13) ため、SUBSCRIBE_OK / REQUEST_UPDATE_OK が対象 request の Subscription を直接使うのと異なり、Track 索引からの引き当てが必要になる。`publisher_track_largest` は現在 private のため、`track_status` から呼べる可視性 (`pub(crate)`) へ広げる。この可視性変更は 0016 の largest ヘルパー共通化とは独立に行う。

relay の上流観測:

- draft §9.20.18 の relay MUST (上流 publisher / subscription で観測した largest) は単一 Session の責務外とし、アプリが parameters に載せる。Session の自動注入は自側 publisher 役 subscription が観測した largest に限る。

INCLUDE_PROPERTIES:

- INCLUDE_PROPERTIES=0 のとき Track Properties を空にする既存挙動 (draft §9.20.22) は変更しない。本変更は Track Properties ではなく parameters への LARGEST_OBJECT 追加のみを行う。

## 完了条件

- TRACK_STATUS_OK 送信後に bidi stream の送信方向が FIN されること
- 対象 Track の publisher 役 subscription に観測 largest がある場合、TRACK_STATUS_OK に LARGEST_OBJECT が含まれること
- 観測 largest が無い Track では LARGEST_OBJECT を自動注入しないこと (アプリが指定した parameters は改変しないこと)
- `tests/test_session/namespace/track_status.rs` に FIN と LARGEST_OBJECT の検証が追加されていること

## 解決方法

TRACK_STATUS_OK を仕様どおりに FIN で送信し、公開済み Track の LARGEST_OBJECT を自動注入するようにした。

- `send_request_ok` の `SendOnStream` 発行で、TRACK_STATUS context のみ `fin: true` にした (draft §9.13)。
- `send_ok_for_track_status` のシグネチャに `&mut MessageParameters` を追加し、`publisher_track_largest` (`src/session/subscription/fill.rs`) で
  対象 Track の publisher 役 subscription 群が観測した largest を引いて `update_largest_object_in_parameters` で注入した (draft §9.20.18)。
  アプリ指定値との大きい方を採り、wire に載った最終値を `TrackStatusResponse::Ok.largest_location` にも反映した。
- `publisher_track_largest` を `pub(crate)` に広げ、doc を fill と TRACK_STATUS_OK の共用に更新した。
- `forget_track_status` の doc に「bidi stream 終端 (`recv_request_stream_closed`) 後に呼ぶ」契約を明記し、ライフサイクルテストを終端通知後に forget する順序に修正した。
- `tests/test_session/namespace/track_status.rs` に FIN / 注入 / 未公開時の非注入 / アプリ指定値の非降下 / 引き上げ / 複数 subscription の max / 観測なし×アプリ指定値 のテストを追加した (`tests/test_session.rs` に fin 付きヘルパーを追加)。
- `CHANGES.md` の `[FIX]` にエントリを追加した。

残った制約 (スコープ外): largest は publisher 役 subscription が保持するため、`forget_subscription` 後の Track では LARGEST_OBJECT を省略する。Track 単位で largest を永続保持する改善は別途必要。
