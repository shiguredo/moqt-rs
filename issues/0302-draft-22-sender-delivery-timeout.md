# 送信側 delivery timeout を先頭 Object の Object Property override と送信前チェックに対応させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-sender-delivery-timeout
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §5.2 (Delivery Timeouts and Data Reliability) は、publisher の値について「先頭 Object の Object Property があればそれ、なければ Track Property」とし、transport に渡す前に経過時間を検査して超過時に `DELIVERY_TIMEOUT` で reset することを MUST とする。現状は受信側だけが override を実装し、送信側は Track Property しか見ず、送信直前チェックも無い。

## 現状

- `src/session/data.rs` の `send_subgroup_object` は `subscription.delivery_timeouts.effective_object_ms` が `Some` のときだけ `object_delivery_header_complete_ms` を記録し、`properties_bytes` はフィルタ評価にしか使わない。
- `src/session/data.rs` の `close_outgoing_subgroup_stream` は `effective_subgroup_ms` が `Some` のときだけ FIN タイマーを開始し、同じく override を見ない。
- datagram 送信も `effective_object_ms` / `effective_subgroup_ms` だけを使う。
- 受信側は `src/session/data.rs` の `subgroup_effective_delivery_timeout` と `subgroup_overrides` で override を実装しており、送信側だけが非対称である。
- 期限判定は tick 時の処理に限られ、送信 API は「transport に渡す前の検査」を行っていない。
- 影響: delivery timeout を設定した publisher で override が無視され、期限切れ Object が transport に渡り得る。現在の example は timeout を設定しないため表面化していない。

## 設計方針

- 送信側でも先頭 Object の Properties を decode し、(group_id, subgroup_id) 単位の override を保持する。FirstObjectId モードは subgroup_id 解決後の最初の Object で適用する。
- `min(購読者値, override または Track Property)` を effective 値として使う。
- `send_subgroup_object` / `send_object_datagram` で送信時点の経過時間を検査し、超過時は `ResetDataStream(DELIVERY_TIMEOUT)` を発行して Object を送らない。
- 送信側の override と送信直前チェックをテストで固定する。

## 完了条件

- 先頭 Object の Object Property が送信側の timeout に反映されること。
- 期限切れ Object を送信しようとすると DELIVERY_TIMEOUT で reset されること。
- Track Property のみのケースでも送信前チェックが働くこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
