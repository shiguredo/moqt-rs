# MSF カタログトラックの優先度を media より高くする

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 A.1.3 は、カタログトラックが記述する track より高い publisher priority を持つこと (SHOULD)、subscriber がカタログ購読に他の同時購読より高い subscriber priority を使うこと (SHOULD) を追加した。
カタログは media の購読判断に先立って必要になるため、media と同じ優先度では輻輳時にカタログが遅延し、購読開始が遅れる。現行実装はカタログと media が同じ既定値で、この SHOULD を満たしていない。

## 現状

- `crates/moq-pub/src/catalog.rs` の `publish_catalog_object` は publisher priority に `DEFAULT_PUBLISHER_PRIORITY` (128) を渡す。
- `crates/moq-pub/src/pipeline.rs` の media 側も `DEFAULT_PUBLISHER_PRIORITY` (128) を使うため、カタログと media の優先度が同じである。
- `crates/moq-sub` の `catalog_subscribe_parameters` は `SUBSCRIBER_PRIORITY` を送らず、既定値の 128 になる。audio の `audio_subscribe_parameters` も既定 128 で、カタログが audio より高いとは言えない。
- [MOQT] §5.1 は優先度を 0〜255 の値とし、値が小さいほど優先度が高い (0 が最高) と定める。既定値は 128。
- v02 A.1.3 はカタログの優先度を media より高くすることを SHOULD とする。

## 設計方針

- カタログ専用の publisher priority 定数を追加し、128 より小さい値にする。値は運用で調整できるよう 1 箇所の定数にまとめる。
- カタログ購読の SUBSCRIBE に `SUBSCRIBER_PRIORITY` を載せ、media の既定値 (128) より小さい値にする。
- 優先度の値は media の種類ごとに変えない。カタログだけを他より高くする。
- `crates/moq-pub` の `publish_catalog_object` の呼び出し元で値を渡している箇所を、新しい定数に置き換える。

## 完了条件

- カタログの Object が media の Object より小さい publisher priority で送信されること。
- カタログ購読の SUBSCRIBE が `SUBSCRIBER_PRIORITY` パラメータを持ち、値が media の既定値より小さいこと。
- media の publisher priority と subscriber priority の既存の値が変わらないこと。
- 上記を `crates/moq-pub` と `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
