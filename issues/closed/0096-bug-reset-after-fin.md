# FIN で閉じた受信 data stream に後から届く RESET_STREAM でセッションを閉じる

- Created: 2026-09-16
- Completed: 2026-09-16
- Branch: feature/fix-reset-after-fin
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) は、Subgroup の全 Object と FIN を
送った送信側が、その後に同じ stream を reset する場合があるとし、FIN まで受信済みの
アプリケーションは後続の reset を無視してよいと定めている。

> A sender might send all objects in a Subgroup and the FIN on a QUIC stream, and then reset
> the stream. In this case, the receiving application would receive the FIN if and only if all
> objects were received. If the application receives all data on the stream and the FIN, it can
> ignore any subsequent reset.

QUIC (RFC 9000) では FIN を送った後の "Data Sent" 状態からも RESET_STREAM を送れるため、
この順序は正当なワイヤ表現である。ところが `Session::recv_data_stream_closed` は、未知の
stream id の終端通知を一律に `PROTOCOL_VIOLATION` として扱うため、FIN の終端通知の後に
同じ stream id の RESET が届くとセッションを閉じてしまう。仕様が許容する順序で相手と
接続できなくなるため修正する。

## 現状

- `src/session/data.rs` の `recv_data_stream_closed` は `data_streams.incoming` から id を
  除去したうえで終端を処理する。同じ id の 2 回目の終端通知は
  `data_streams.discarded` (キャンセル由来など、受信そのものを破棄する集合) に含まれる場合を
  除いて `PROTOCOL_VIOLATION` になる。
- FIN で正常終了した stream は `discarded` に入らないため、後続の RESET でセッションが閉じる。
- `discarded` に登録して吸収する案は、`report_mid_object_fin` (直列化途中の FIN 検出) や
  PUBLISH_DONE の open stream 会計など、破棄対象 stream を前提とした複数の判定に影響するため
  採用できない。

## 設計方針

- FIN で閉じた受信 data stream id を保持する専用の集合 (`DataStreamState::fin_closed`) を追加し、
  未知 id の終端通知のうち **RESET** かつ当該集合に含まれるものだけを no-op で吸収する。
  FIN の重複通知は I/O 層の誤りであり、従来どおり `PROTOCOL_VIOLATION` として検出する。
- 保持期間は `peer_alias_retention_ms` (破棄対象 stream id と同じ) とし、
  `tick_discarded_data_stream_ids` で掃除する。QUIC の stream id は再利用されないため、
  保持中に同一 id の正当なイベントは届かない。
- `discarded` の意味 (受信そのものを破棄する stream) は変更しない。

## 完了条件

- FIN の終端通知の後に同じ stream id の RESET が届いてもセッションが `Established` のままであること
- RESET の終端通知の後に FIN が届く場合は従来どおり `PROTOCOL_VIOLATION` になること
- FIN の重複通知は従来どおり `PROTOCOL_VIOLATION` になること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

- `DataStreamState` に `fin_closed` (FIN で閉じた受信 data stream id の保持集合) を追加した。
  保持期間は `peer_alias_retention_ms` で、`tick_discarded_data_stream_ids` が
  `discarded` と同じ tick で期限切れを掃除する。
- `recv_data_stream_closed` は、stream を正常に終端したときに `end` が FIN なら id を
  `fin_closed` へ登録する。unknown id の終端通知では、`end` が RESET かつ `fin_closed` に
  含まれる場合だけ no-op で吸収する。
- FIN の重複通知は従来どおり `PROTOCOL_VIOLATION` のままとした (I/O 層の誤りを検出し続ける)。
  `discarded` の意味 (受信そのものを破棄する stream) も変更していない。

検証:

- `tests/test_session/data_stream.rs` に 2 件追加した。
  `reset_after_fin_on_subgroup_stream_is_ignored` は FIN の終端通知の後に同じ id の RESET が
  届いてもセッションが `Established` のままであることを固定する。
  `second_close_after_reset_is_still_protocol_violation` は RESET の後に FIN が届く場合が
  従来どおり `PROTOCOL_VIOLATION` になることを固定する。
- FIN の重複通知が `PROTOCOL_VIOLATION` になることは既存テスト
  (`subscription::publish_done::publish_done_drain_waits_for_timeout_and_open_stream_close` ほか) が
  固定している。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ることを確認した。
