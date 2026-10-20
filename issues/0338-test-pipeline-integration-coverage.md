# example pipeline と e2e の統合経路、FETCH の fuzz をテストする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pipeline-integration-coverage
- Polished: 2026-10-20

## 目的

library の tests / PBT / fuzz は厚いが、example の pipeline 統合経路 (ピア要求処理、カタログ適用、GOAWAY 移行、メディア FETCH) と FETCH の fuzz 経路は未テストの部分があり、e2e も live 3 本のみである。統合経路の退行を検出できる状態にする。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` / `serve_media_fetch` / `serve_fetch_entries` を直接呼ぶテストが無い。`pipeline.rs` には補助関数の単体テストはあるが、peer と接続して `run` を駆動するテストは無い。
- `examples/moq-sub/src/pipeline.rs` の `receive_catalog` / `resubscribe_tracks` / `run` を直接呼ぶテストが無い。同様に補助関数の単体テストはある。
- `e2e-tests/tests/roundtrip.rs` は映像・音声 datagram・音声冗長の 3 本で、GOAWAY 移行、フィルタ付き / pause 購読、メディア FETCH 応答、MP4 パススルー / 再エンコード、PUBLISH_DONE の track 単位処理を検証しない。
- e2e の `ATTEMPT_TIMEOUT` は 4 秒 (`e2e-tests/tests/roundtrip.rs`) で GOP の既定 2 秒
  (`examples/moq-pub/src/cli.rs` の `DEFAULT_GOP_DURATION_MS`) より長い。さらに moq-sub は映像の
  SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せ (`examples/moq-sub/src/pipeline.rs` の
  `video_subscribe_parameters`)、moq-pub は直前の Group が 300 ms に達した後の最初の入力フレームで
  キーフレームを強制する (`examples/moq-pub/src/gop.rs` の `MIN_GROUP_DURATION_US`)。そのため FETCH が
  壊れていても、購読開始から約 300 ms 後には live のキーフレームで最初のフレームが届き得る。
  ATTEMPT_TIMEOUT を GOP より短くするだけでは FETCH 経路を分離できない。
- `fuzz/fuzz_targets/fuzz_session.rs` は `RecvFetchHeader` と `SendFetch` を持つが、FETCH entry の受信と FETCH_OK + 応答 stream の成功経路を踏まない。
- `peer_harness` は `examples/tokio-moq/src/lib.rs` の `#[cfg(test)] mod peer_harness;` であり、`examples/moq-pub` / `examples/moq-sub` / `e2e-tests` から参照できない。`TestPeer` などの型とメソッドも `pub(crate)` である。
- 影響: 0339 / 0340 の example 改修 (pipeline 分割、decoder 共通化) を安全に進められない。
- 依存: 0330 (受信 SUBSCRIBE の Object / PUBLISH_DONE)、0332 (MP4 の PROP_TIMESCALE)、0333 (PUBLISH_DONE の track 単位処理)、0335 (GOAWAY 移行後の Audio Config リセット) の実装後に着手する。これらの挙動を固定するテストは、各 issue の実装後に追加する。

## 設計方針

- `peer_harness` をテスト専用 crate 間で共有できるようにする。
  - `examples/tokio-moq/src/lib.rs` の `#[cfg(test)] mod peer_harness;` を `#[cfg(any(test, feature = "test-support"))] pub mod peer_harness;` に変える。
  - `examples/tokio-moq/Cargo.toml` に `[features] test-support = ["dep:rcgen"]` と optional な `rcgen` 依存を追加する。`peer_harness` は自己署名証明書の生成に `rcgen` を使う。既存の `[dev-dependencies] rcgen` は tokio-moq 自身のテスト用に残す。
  - `peer_harness` の `TestPeerServer` / `TestPeer` と、テストから使う `start` / `accept` / `establish` /
    `send_request_start` / `recv_request_start` / `recv_response` / `send_request_ok` / `send_subscribe_ok` /
    `send_fetch_ok` / `open_uni_send_stream` / `send_goaway` / `finish_request_stream` /
    `recv_uni_data_stream` などの可視性を `pub` にする。
  - `send_subscribe_ok` に `MessageParameters` と `TrackProperties` を渡せるようにする (LARGEST_OBJECT / DYNAMIC_GROUPS をテストから載せるため)。既存の呼び出しは新しい引数に合わせて更新する。PUBLISH_DONE を送る `send_publish_done` を追加する。
  - `peer_harness.rs` の doc コメントにある「`#[cfg(test)]` の module は他クレートへ公開されない」という記述を、`test-support` feature で参照できる旨に更新する。
  - `examples/moq-pub/Cargo.toml` / `examples/moq-sub/Cargo.toml` の `[dev-dependencies]` に `tokio-moq = { path = "../tokio-moq", features = ["test-support"] }` を追加する。
  - 統合テストは各 example crate の `#[cfg(test)] mod tests` に置き、`run` または private な pipeline 関数をクレート内から呼ぶ。pipeline の private 関数をテストのために公開しない。
- moq-pub の統合テスト:
  - `serve_peer_request` 経由で video / audio / catalog の SUBSCRIBE が SUBSCRIBE_OK になり、未知の track が REQUEST_ERROR(REQUEST_DOES_NOT_EXIST) になること。
  - `NEW_GROUP_REQUEST` が live capture の映像だけで受理され、MP4 パススルーでは受理されないこと (`accepted_new_group_request` の呼び出し経路)。
  - フィルタ付き / Forward 0 の SUBSCRIBE は、0330 の実装に従って Object をフィルタするか REQUEST_NOT_SUPPORTED で拒否すること。
  - catalog の FETCH が Object / Empty / InvalidRange の各応答になること。
  - media の FETCH が unknown track / no published objects / empty range / descending group order / range filter の各応答になること。
  - `serve_fetch_entries` が End of Non-Existent Range / End of Unknown Range / Object を正しい順序で書き、FIN で閉じること。
  - `run` を `--input-mp4` で起動し、映像の全 Object に `PROP_TIMESCALE` が付き、PTS 間隔が入力トラックの timescale と一致すること (0332 の実装後)。MP4 fixture は `shiguredo_mp4` の muxer でテスト内に生成する。
- moq-sub の統合テスト:
  - `receive_catalog` が fill fetch stream と subgroup stream の両方から catalog を適用し、delta を適用し、期限切れ・request_id 不一致・不正 Object をエラーにすること。
  - `resubscribe_tracks` が GOAWAY 移行先で catalog / video / audio を FILL_PARAMETERS 付きで購読し、video に REQUEST_UPDATE を送り、直近 Group の FETCH を発行すること。
  - `run` が catalog 取得から購読確立、shutdown までを完了すること。音声 1 Object の受信経路は、既存のテスト用 Opus データを使って検証する。
- GOAWAY 移行と PUBLISH_DONE の track 単位処理は peer_harness で検証する。
  - 2 つの `TestPeerServer` を立て、移行元から `send_goaway` で移行先を指定する。移行先で再購読と FETCH が行われることを確認する。
  - 移行後に Audio Config が再送されることは 0335 の実装後に確認する。
  - 移行先の harness が audio の SUBSCRIBE にだけ PUBLISH_DONE(TRACK_ENDED) を送り、video の受信が継続することを確認する (0333 の実装後)。
- e2e は real relay でしか検証できない経路に限定する。
  - 既存の live 3 本はそのまま維持する。
  - relay が GOAWAY を送れる場合だけ、GOAWAY 移行の e2e を追加する。送れない場合は peer_harness のテストだけで検証する。
  - MP4 入力の PTS 間隔と FETCH 応答は peer_harness のテストで検証する。live の最初のフレーム到着では FETCH 応答を判定しない (NEW_GROUP_REQUEST により live のキーフレームが先に届き得るため)。
- `fuzz_session` に FETCH のライフサイクルを追加する。
  - `SendFetch` が返した request_id を保持し、`RecvStreamMessage` にはその request_id を、`RecvFetchHeader` にはその request_id を持つ FETCH_HEADER を、`RecvFetchEntry` には同じ stream_id を使う。
  - server role のとき、`RecvRequest` で FETCH を受けたら、その request_id で `send_fetch_ok` / `send_fetch_header` / `send_fetch_object` / `send_fetch_data_stream_closed` を呼ぶ成功応答経路を追加する。
  - `RecvFetchHeader` の後に `RecvFetchEntry { stream_id }` を追加し、entry 受信のライフサイクルを踏む。
  - 既存の `RecvFetchHeader` / `SendFetch` は残す。

## 完了条件

- 上記の peer_harness 統合テストが追加され、`cargo test --workspace` が通ること。
- `fuzz_session` の変更が `cargo +nightly fuzz build fuzz_session` でビルドでき、`cargo +nightly fuzz run fuzz_session -- -max_total_time=30` が通ること。
- e2e テストを追加した場合、`MOQT_E2E_URL` を設定した `cargo test -p e2e-tests -- --ignored` が通ること (CI の E2E Test で確認する)。
- 各テストについて、対応する実装を 1 箇所ずつ壊してテストが落ちることを確認していること。確認した変異と結果は issue の解決方法に記録する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
