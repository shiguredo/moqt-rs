# PUBLISH の FILL_PARAMETERS から初回 fill を開設する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-publish-fill-fetch
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.4、§3.4.1、§9.8、§9.19.15](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` に FILL_PARAMETERS がなく、`src/session/subscription/fill.rs` の開設処理は SUBSCRIBE / REQUEST_UPDATE を中心に呼ばれる。
`src/session/core.rs` の `range_filter_usage` の説明も送信元をこの 2 種に限っている。
`src/session/data.rs` の `Session::recv_fetch_header` は未登録 Request ID でセッションを終了する。既存の `PendingSubgroupBuffer` は Track Alias を使う subgroup 専用であり、先着 fill にそのまま適用できない。

## 設計方針

PUBLISH でも FILL_PARAMETERS を受理・送信し、publisher が PUBLISH を送るときに unpaused なら初回 fill を開く。
初回 FETCH_HEADER は subscription の Request ID を使う。
LARGEST_OBJECT、INCLUDE_PROPERTIES、フィルタと優先度、内側 Range の会計を既存 fill と整合させる。
PUBLISH_OK を送る subscriber が誤って fill を送信しないよう、起点と役割を明確にする。
先着する初回 fill について、`Session::recv_fetch_header` は対応 PUBLISH が未登録であることを再試行可能な結果で返し、その時点ではセッションを終了しない。
`crates/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle::recv_fetch_header` から受信側へ伝え、Request ID をキーに stream と decode 済みヘッダを上限・期限付きで保留する。
PUBLISH 受理後にヘッダを Session へ再通知し、受理されてから entry 処理へ進める。保留の上限・期限超過時は stream を破棄して会計を解放する。
`crates/moq-sub/src/pipeline.rs` の `handle_stream_body` / `fetch_stream_action` も変更対象とし、既存の `fetch_targets` にない Request ID を読み捨てる判定で PUBLISH 起因の fill を失わないようにする。

## 完了条件

- PUBLISH の fill が送信時に 1 本開設され、paused・FILL_PARAMETERS 省略では開設されないこと。
- PUBLISH より先着した fill がセッション終了や読み捨てにならず、有限保留から PUBLISH 受理後に再投入されること。保留の上限・期限超過時に stream と会計が残らないこと。
- 初回と REQUEST_UPDATE 起因の Request ID、Range 上限、プロパティの条件を検証すること。

## 依存・関連

- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)
