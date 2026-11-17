# MSF カタログトラックの優先度を media より高くする

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 Appendix A.1.3 は、カタログトラックが記述する track より高い publisher priority を持つこと (SHOULD)、subscriber がカタログ購読に他の同時購読より高い subscriber priority を使うこと (SHOULD) を追加した。

カタログは media の購読判断に先立って必要になるため、media と同じ優先度では輻輳時にカタログが遅延し、購読開始が遅れる。現行実装はカタログと media が同じ既定値で、この SHOULD を満たしていない。

## 現状

- `crates/moq-pub/src/catalog.rs` の `publish_catalog_object` は `SubgroupWriter::new` にリテラル `128` を渡す。`crates/moq-pub/src/pipeline.rs` の `DEFAULT_PUBLISHER_PRIORITY` (128) は private で、カタログ側は参照していない。
- media 側も `DEFAULT_PUBLISHER_PRIORITY` (128) を使うため、カタログと media の優先度が同じである。
- カタログの Object は `publish_catalog_object` だけでなく、`crates/tokio-moq` の `send_fill_fetch_response` と `send_fetch_response` でも送られる。どちらも publisher priority に 128 を固定している。
- `crates/moq-sub` の `catalog_subscribe_parameters` は `SUBSCRIBER_PRIORITY` を送らず、既定値の 128 になる。audio の `audio_subscribe_parameters` も既定 128、video は REQUEST_UPDATE で 192 を送る。
- [MOQT] §5.1 は優先度を 0〜255 の値とし、値が小さいほど優先度が高い (0 が最高) と定める。既定値は 128。

## 設計方針

- カタログ専用の publisher priority 定数を追加し、128 より小さい値にする。値は運用で調整できるよう 1 箇所の定数にまとめる。
- `publish_catalog_object` 内のリテラル `128` を新しい定数に置き換える。呼び出し元に priority 引数を追加するのではなく、カタログ用の値を使う。
- `send_fill_fetch_response` と `send_fetch_response` でカタログ Object を送る経路にもカタログ用の publisher priority を渡せるようにする。カタログと media を区別する引数または呼び出し経路を追加し、カタログの全送信経路で同じ値を使う。
- カタログ購読の SUBSCRIBE に `SUBSCRIBER_PRIORITY` を載せ、media の既定値 (128) より小さい値にする。
- media の publisher priority と subscriber priority の既存の値は変えない。

## 完了条件

- `publish_catalog_object`、`send_fill_fetch_response`、`send_fetch_response` のすべてで、カタログの Object が media の Object より小さい publisher priority で送信されること。
- カタログ購読の SUBSCRIBE が `SUBSCRIBER_PRIORITY` パラメータを持ち、値が media の既定値より小さいこと。
- media の publisher priority (128) と subscriber priority の既存の値が変わらないこと。
- カタログの全送信経路が同じ定数を参照していること。
- 上記を `crates/moq-pub` と `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/moq-pub` に `CATALOG_PUBLISHER_PRIORITY = 64` を追加し、`publish_catalog_object` のリテラル 128 を置き換えた。media の `DEFAULT_PUBLISHER_PRIORITY` (128) は変えていない。
- `MoqtClient::send_fetch_response` / `send_fill_fetch_response` に `publisher_priority: u8` 引数を追加し、moq-pub のカタログ FETCH / fill fetch 経路が `CATALOG_PUBLISHER_PRIORITY` を渡すようにした。カタログの全送信経路が同じ定数を参照する。
- `crates/moq-sub` に `CATALOG_SUBSCRIBER_PRIORITY = 64` を追加し、`catalog_subscribe_parameters` が `SUBSCRIBER_PRIORITY` を載せるようにした。
- `MoqtClient::subscribe_track_inner` は、呼び出し側が `SUBSCRIBER_PRIORITY` を指定していれば上書きせず、未指定時だけ既定値 128 を追加するようにした。
- `crates/moq-pub` / `crates/moq-sub` / `crates/tokio-moq` に priority のテストを追加し、`docs/msf.md` にカタログ優先度を追記した。media の既存 priority は変えていない。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
