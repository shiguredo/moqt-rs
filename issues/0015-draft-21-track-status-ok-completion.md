# TRACK_STATUS_OK の FIN と LARGEST_OBJECT を仕様どおりにする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-status-ok-completion

## 目的

draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS) と §9.20.18 (LARGEST OBJECT Parameter) の MUST を満たす。TRACK_STATUS_OK 後に bidi stream を FIN し、Objects が公開済みなら LARGEST_OBJECT を含める。

## 現状

`src/session/core.rs` の `send_request_ok` は全 context で `SessionEvent::SendOnStream { fin: false }` を積む。TRACK_STATUS は REQUEST_UPDATE を受け付けない "first and only message" であり、`send_request_error` 側は `fin: true` なのに OK 側だけ FIN が欠落している。

同じく `send_ok_for_track_status` は `largest_location: None` 固定で、`effective_largest_object` からの LARGEST_OBJECT 自動注入を行わない。SUBSCRIBE_OK / REQUEST_UPDATE_OK は注入するため非対称。`TRACK_STATUS_OK_ALLOWED_PARAMS` は `[LARGEST_OBJECT]` なので載せられる。

根拠:

- §9.13: "The bidi stream is closed with a FIN after TRACK_STATUS_OK or REQUEST_ERROR are sent."
- §9.20.18: "If Objects have been published on this Track the Publisher MUST include this parameter."

## 設計方針

`send_request_ok` の TrackStatus context で `fin: true` にする。`send_ok_for_track_status` で対象 Track の `effective_largest_object` を取得し、Objects が公開済みなら parameters へ LARGEST_OBJECT を注入する。INCLUDE_PROPERTIES=0 のとき Track Properties を空にする既存挙動と整合させる。

## 完了条件

- TRACK_STATUS_OK 送信後に bidi stream の送信方向が FIN されること
- Objects 公開済みの Track への TRACK_STATUS_OK に LARGEST_OBJECT が含まれること
- 未公開 Track では LARGEST_OBJECT を含めないこと
- `tests/test_session/namespace/track_status.rs` に FIN と LARGEST_OBJECT の検証が追加されていること
