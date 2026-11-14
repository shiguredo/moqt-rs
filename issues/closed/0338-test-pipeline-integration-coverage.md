# example pipeline と e2e の統合経路、FETCH の fuzz をテストする

- Created: 2026-10-19
- Completed: 2026-11-14
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: 2026-11-14
- Refreshed: 2026-11-04

## 目的

library の tests / PBT / fuzz は厚いが、example の pipeline 統合経路 (ピア要求処理、カタログ適用、GOAWAY 移行、メディア FETCH) と FETCH の fuzz 経路は未テストの部分があり、e2e も live 3 本のみである。統合経路の退行を検出できる状態にする。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` / `serve_media_fetch` / `serve_fetch_entries` を直接呼ぶテストが無い。`pipeline.rs` には補助関数の単体テストはあるが、peer と接続して `run` を駆動するテストは無い。
- `crates/moq-sub/src/pipeline.rs` の `receive_catalog` / `resubscribe_tracks` / `run` を直接呼ぶテストが無い。同様に補助関数の単体テストはある。
- `e2e-tests/tests/roundtrip.rs` は映像・音声 datagram・音声冗長の 3 本で、GOAWAY 移行、フィルタ付き / pause 購読、メディア FETCH 応答、MP4 パススルー / 再エンコード、PUBLISH_DONE の track 単位処理を検証しない。
- e2e の `ATTEMPT_TIMEOUT` は 4 秒 (`e2e-tests/tests/roundtrip.rs`) で GOP の既定 2 秒
  (`crates/moq-pub/src/cli.rs` の `DEFAULT_GOP_DURATION_MS`) より長い。さらに moq-sub は映像の
  SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せ (`crates/moq-sub/src/pipeline.rs` の
  `video_subscribe_parameters`)、moq-pub は直前の Group が 300 ms に達した後の最初の入力フレームで
  キーフレームを強制する (`crates/moq-pub/src/gop.rs` の `MIN_GROUP_DURATION_US`)。そのため FETCH が
  壊れていても、購読開始から約 300 ms 後には live のキーフレームで最初のフレームが届き得る。
  ATTEMPT_TIMEOUT を GOP より短くするだけでは FETCH 経路を分離できない。
- `fuzz/fuzz_targets/fuzz_session.rs` は `RecvFetchHeader` と `SendFetch` を持つが、FETCH entry の受信と REQUEST_OK (FETCH_OK) + 応答 stream の成功経路を踏まない。
- `peer_harness` は `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod peer_harness;` であり、`crates/moq-pub` / `crates/moq-sub` / `e2e-tests` から参照できない。`TestPeer` などの型とメソッドも `pub(crate)` である。`peer_harness.rs` の内側の `mod tests` には `#[cfg(test)]` が付いていない。
- `TestPeer::send_goaway` は control stream に書く制御用 GOAWAY 専用で、request stream に GOAWAY を送るメソッドが無い。client が開いた request stream は `accepted_requests` に保持されるが、その後続メッセージと終端を読むメソッドも無い。
- 影響: 0339 の example 改修 (pipeline 分割) を安全に進められない。0339 は 0338 を前提としている。
- 依存: 0330 (受信 SUBSCRIBE の Object / PUBLISH_DONE)、0333 (PUBLISH_DONE の track 単位処理) の実装後に着手する。これらの挙動を固定するテストは、各 issue の実装後に追加する。
  0375 (request stream GOAWAY の request 単位移行) のテストは 0375 の実装後に追加する。
  0332 (MP4 の PROP_TIMESCALE) は解決済みである。0335 (GOAWAY 移行後の Audio Config リセット) のうち publisher 側の `audio_config_sent` のリセットは
  60244e5e で実装済みであり (`crates/moq-pub/src/pipeline.rs`)、publisher 側の再送の確認は 0335 を待たない。
  0427 (未 cache の配信中 track への FETCH を INVALID_RANGE にする) は open であり、その期待値を固定するテストは 0427 の実装後に追加する。

## 設計方針

- `peer_harness` をテスト専用 crate 間で共有できるようにする。
  - `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod peer_harness;` を `#[cfg(any(test, feature = "test-support"))] pub mod peer_harness;` に変える。
  - `peer_harness.rs` の内側の `mod tests` に `#[cfg(test)]` を付ける。付けないと `test-support` が有効な非テストビルドでテスト用 helper だけが未使用になり、`clippy --all-targets -- -D warnings` が落ちる。
  - `crates/tokio-moq/Cargo.toml` に `[features] test-support = ["dep:rcgen"]` と optional な `rcgen` 依存を追加する。`peer_harness` は自己署名証明書の生成に `rcgen` を使う。既存の `[dev-dependencies] rcgen` は tokio-moq 自身のテスト用に残す。
  - `peer_harness` の `TestPeerServer` / `TestPeer` と、テストから使う `start` / `accept` / `establish` /
    `send_request_start` / `recv_request_start` / `recv_response` / `send_request_ok` / `send_subscribe_ok` /
    `send_fetch_ok` / `open_uni_send_stream` / `send_goaway` / `finish_request_stream` /
    `recv_uni_data_stream` などの可視性を `pub` にする。
  - `send_subscribe_ok` に `MessageParameters` と `TrackProperties` を渡せるようにする (LARGEST_OBJECT / DYNAMIC_GROUPS をテストから載せるため)。既存の呼び出しは新しい引数に合わせて更新する。
  - request stream GOAWAY を送る `send_goaway_on_request_stream(request_id, new_session_uri, timeout_ms)` を追加する。`Session::send_goaway_on_request_stream` を呼び、積まれた `SessionEvent::SendOnStream` を `accepted_requests` の該当 stream へ書く (`flush_queued_response` と同じ経路)。
  - client が開いた request stream の後続メッセージと終端を読む `recv_request_stream_message(request_id)` を追加する。REQUEST_UPDATE の受信と、旧 request stream の FIN / RESET_STREAM の観測に使う。
  - `send_publish_done(request_id, status_code, stream_count)` を追加する。
  - `peer_harness.rs` の doc コメントにある「`#[cfg(test)]` の module は他クレートへ公開されない」という記述を、`test-support` feature で参照できる旨に更新する。
  - `crates/moq-pub/Cargo.toml` / `crates/moq-sub/Cargo.toml` の `[dev-dependencies]` に
    `tokio-moq = { path = "../tokio-moq", default-features = false, features = ["test-support"] }` を追加する。
    `default-features = false` は必須である。省略すると tokio-moq の既定 `quic-s2n` が有効になり、
    `--no-default-features --features quic-tobi` / `quic-ngtcp2` のテストビルドで
    `crates/tokio-moq/src/quic.rs` の `compile_error!` に当たる。
  - shiguredo-rust は「テストのために private な API を feature フラグで公開しない」と定めるが、
    `peer_harness` は production の private API ではなくテスト専用モジュールである。`tokio-moq` は
    `publish = false` で `test-support` は既定 off のため、この feature 公開を例外とする。
  - 統合テストは各 example crate の `#[cfg(test)] mod tests` に置き、`run` または private な pipeline 関数をクレート内から呼ぶ。pipeline の private 関数をテストのために公開しない。
- moq-pub の統合テスト:
  - `serve_peer_request` 経由で video / audio / catalog の SUBSCRIBE が SUBSCRIBE_OK になり、未知の track が REQUEST_ERROR(REQUEST_DOES_NOT_EXIST) になること。
  - 配信中の track と同じ track name で別の Track Namespace を指す SUBSCRIBE / FETCH が、REQUEST_ERROR(REQUEST_DOES_NOT_EXIST) になること (`published_track` の照合経路)。
  - `NEW_GROUP_REQUEST` がエンコーダ経路 (live capture と MP4 再エンコード) の映像で受理され、MP4 パススルーでは受理されないこと (`accepted_new_group_request` の呼び出し経路)。テストは live capture の受理と MP4 パススルーの不受理の 2 ケースでよい。
  - フィルタ付き / PAUSED 1 (旧 FORWARD 0) の SUBSCRIBE は受理され、Object が `object_passes_filters` でフィルタされること (0330 の設計。明示的な拒否はしない)。
  - catalog の FETCH が Object / Empty / InvalidRange の各応答になること。
  - media の FETCH が unknown track / empty range / descending group order / range filter の各応答になること。
    unknown track は REQUEST_ERROR(REQUEST_DOES_NOT_EXIST)、empty range は FETCH_OK と FETCH_HEADER だけの stream、
    descending group order と range filter は REQUEST_ERROR(REQUEST_NOT_SUPPORTED) になる。
    `FetchPlan::NoPublishedObjects` は通常到達しない防御分岐であるため、本 issue ではテスト対象にしない。
    配信中で Object を 1 件も cache に入れていない track の応答は 0427 の範囲であり、0427 の実装後に追加する。
  - `serve_fetch_entries` が End of Non-Existent Range / End of Unknown Range / Object を正しい順序で書き、FIN で閉じること。
  - `run` を `--input-mp4` で起動し、映像の全 Object に `PROP_TIMESCALE` が付き、PTS 間隔が入力トラックの timescale と一致すること。MP4 fixture は `shiguredo_mp4` の muxer でテスト内に生成する。
- moq-sub の統合テスト:
  - `receive_catalog` が fill fetch stream と subgroup stream の両方から catalog を適用し、delta を適用すること。
  - `CATALOG_TIMEOUT` までに catalog が決まらない場合だけ `receive_catalog` が Err を返すこと。
  - fill の request_id 不一致・デコード失敗・REQUEST_ERROR / RESET は Err にせず、警告して購読側で届く catalog を待つこと
    (`read_catalog_fetch_stream` の単体テストと、`receive_catalog` が継続することの 2 つで確認する)。
  - `apply_catalog_object` の不正 Object は Err になること。別 alias の subgroup stream は読み捨てること。
  - `resubscribe_tracks` が GOAWAY 移行先で catalog を FILL_PARAMETERS 付きで購読し、video に REQUEST_UPDATE を送り、映像にだけ直近 Group の FETCH を発行すること (video / audio の購読に FILL_PARAMETERS は載せない。音声の明示 FETCH は発行しない)。
  - GOAWAY による session 移行の前後で音声が欠落しないこと。旧 session が最後に配送した位置から、新しい session の
    live の音声までの TIMESTAMP が連続し、同じ Object が二重に流れないことを確認する。音声の明示 FETCH は発行しないため、
    この連続性は移行後の再購読が `audio_subscribe_parameters` の Absolute Start で移行中の音声を取り直し、
    `AudioDeliveryOrder` が重複を捨てることで成立する。移行完了まで relay が旧 session の購読を維持すること
    (draft-ietf-moq-transport-23 §6.6.1 (Graceful Session Migration)) を前提に置く。
  - request stream 単位の再発行の前後で音声が欠落しないこと。request stream 上の GOAWAY を送れるのは request の receiver (relay) だけで、
    再発行するのは request の sender (moq-sub) である (draft-ietf-moq-transport-23 §9.2 (GOAWAY) の
    "Only the receiver of a request can send GOAWAY on its request stream.")。`reissue_request` は旧 subscription に
    `STOP_SENDING` を送ってから再購読する (peer の `RESET_STREAM` は待たない)。この前後で音声が連続することを確認する。
  - `run` が catalog 取得から購読確立、shutdown までを完了すること。音声 1 Object の受信経路は、既存のテスト用 Opus データを使って検証する。
- GOAWAY 移行と PUBLISH_DONE の track 単位処理は peer_harness で検証する。
  - 2 つの `TestPeerServer` を立て、移行元から `send_goaway` で移行先を指定する。移行先で再購読と FETCH が行われることを確認する。
  - 移行後に publisher が最初の音声 Object に Audio Config を載せ直すことを確認する。同一 URL の再発行と別 URL の side session 移行の両方で `audio_config_sent` がリセットされる。harness は Object のプロパティから `PROP_AUDIO_CONFIG` を取り出して確認する。
  - subscriber 側の Audio Config の検証し直しは、別 URL の side session へ移行した場合だけ `session_generation` が進み、`claim_audio_config_generation` で新しい世代として処理される。この配線は 0335 の純関数テストが固定済みであり、本 issue では移行後の音声 Object が音声チャネルへ 1 度だけ流れることを確認する。
  - 移行先の harness が audio の SUBSCRIBE にだけ PUBLISH_DONE(TRACK_ENDED) を送り、video の受信が継続することを確認する (0333 の実装後)。
- request stream GOAWAY による request 単位移行を検証する (0375 の実装後)。
  - publisher が非空 URI の同一 URL の request stream GOAWAY で、現在の session に PUBLISH を再発行すること。
  - publisher が別 URL の request stream GOAWAY で新しい session に PUBLISH を移行し、旧 session の他の track の送信が続くこと。
  - 同一の移行先 URL を指す複数 request が 1 つの新 session を共有すること。
  - 移行期限切れで警告を出して main ループが終了し、`run` が終了して NO_ERROR (0) の CONNECTION_CLOSE を送ること。request stream GOAWAY の Timeout で Session 自体が閉じないことは library のテスト (`tests/test_session/goaway.rs`) が固定済みであり、ここでは対象外とする。FETCH の移行に失敗した場合は警告して購読を続け、main ループを終了しないことも確認する。
  - FETCH の request stream GOAWAY で、同じ namespace / track name / パラメータの FETCH が新 session で再発行されること。
  - request 単位移行後に、最初にフィルタを通過する音声 Object に Audio Config が載ること (別 URL の移行)。
- e2e は real relay でしか検証できない経路に限定する。
  - 既存の live 3 本はそのまま維持する。
  - 既定では e2e を追加しない。relay が GOAWAY を送れることを確認できた場合だけ、GOAWAY 移行の e2e を追加する (追加しない場合は peer_harness のテストだけで検証する)。
  - MP4 入力の PTS 間隔と FETCH 応答は peer_harness のテストで検証する。live の最初のフレーム到着では FETCH 応答を判定しない (NEW_GROUP_REQUEST により live のキーフレームが先に届き得るため)。
- `fuzz_session` に FETCH のライフサイクルを追加する。FETCH の成功経路は入力の decode 待ちに依存させず、専用の Op で決定的に踏む。
  - `SendFetch` の返す request_id をローカルに保持する。
  - client role 用の Op を追加する。直前に送信した FETCH の request_id で REQUEST_OK (FETCH_OK) を受信し、同じ request_id を持つ FETCH_HEADER を入力の stream_id で受信する。FETCH_HEADER を受け付けたらその stream_id を保持する。
  - `RecvFetchEntry` を追加し、保持した stream_id から entry を受信する。entry は `RecvSubgroupObject` と同じく入力から object_id / status / properties を作る。FETCH stream の終端は既存の `RecvDataStreamClosed` で通知する。
  - server role 用の Op を追加し、固定 namespace と入力の track 名 / request_id で FETCH request を組み立てて `recv_request` へ渡す。続けて同じ request_id で `send_fetch_ok` / `send_fetch_header` / `send_fetch_object` / `send_fetch_data_stream_closed` を呼ぶ成功応答経路を踏む。
  - 既存の `RecvFetchHeader` / `SendFetch` は残す。

## 完了条件

- 上記の peer_harness 統合テスト (request stream GOAWAY 移行の 6 項目を含む) が追加され、`cargo test --workspace` が通ること。0427 に依存する項目は 0427 の実装後に追加する。
- `fuzz_session` の変更が `cargo +nightly fuzz build fuzz_session` でビルドでき、`cargo +nightly fuzz run fuzz_session -- -max_total_time=30` が通ること。FETCH の成功経路は専用の Op で決定的に踏むため、任意入力の decode 待ちに依存しない。
- e2e テストを追加した場合、`MOQT_E2E_URL` を設定した `cargo test -p e2e-tests -- --ignored` が通ること (CI の E2E Test で確認する)。
- 各テストについて、対応する実装を 1 箇所ずつ壊してテストが落ちることを確認していること。確認した変異と結果は issue の解決方法に記録する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

example の pipeline 統合経路を実 peer (実 QUIC 接続の `TestPeer` / `TestPeerServer`) で駆動するテストと、
FETCH の fuzz 経路を追加した。

### テスト基盤

- `crates/tokio-moq/src/lib.rs` の `peer_harness` を
  `#[cfg(any(test, feature = "test-support"))] pub mod peer_harness;` に変え、`[features] test-support =
  ["dep:rcgen"]` と optional な `rcgen` を追加した。`crates/moq-pub` / `crates/moq-sub` の
  `[dev-dependencies]` は `default-features = false` を付けて `test-support` を有効にする
  (付けないと tokio-moq の既定 `quic-s2n` が有効になり、`quic-tobi` / `quic-ngtcp2` のテストビルドが
  `compile_error!` に当たる)。`peer_harness` の内側の `mod tests` には `#[cfg(test)]` を付けた。
- `TestPeer` に次の API を追加した。`send_goaway_on_request_stream` / `recv_request_stream_message`
  (`RequestStreamRead::{Message, Closed}`) / `send_publish_done` / `recv_uni_data_stream_end`
  (FIN と RESET_STREAM を区別して返す) / `note_published_object` (publisher 役 subscription の
  LARGEST_OBJECT を Session に記録する。wire には送らない)。
- `send_subscribe_ok` に `MessageParameters` と `TrackProperties` を渡せるようにした。
- `recv_request_start` は、Session がメッセージ単位で自動拒否した request (REQUEST_ERROR を積んで
  `recv_request` が `Ok` を返す場合) を検出して Err を返すようにした。拒否を受理と取り違えるテストを防ぐ。
- 他クレートのテストから使わない API は `#[cfg(test)] pub(crate)` に戻し、test-support で公開する範囲を
  必要なものに限った。

### moq-pub の統合テスト (15 件)

`crates/moq-pub/src/pipeline.rs` の `mod peer_integration_tests` に、SUBSCRIBE の受理と未知 track の拒否、
別 namespace の拒否、`NEW_GROUP_REQUEST` の受理条件 (live capture は受理、MP4 パススルーは不受理)、
フィルタ付き / PAUSED 購読のフィルタと RESET、catalog と media の FETCH 応答、`serve_fetch_entries` の
entry 順序と FIN、`--input-mp4` の `PROP_TIMESCALE` と PTS 間隔、request stream GOAWAY (同一 URL の再発行 /
別 URL の side session 移行と旧 session の継続 / 同一移行先 URL の session 共有 / 期限切れでの終了と
NO_ERROR の close と警告ログ / Audio Config の再送) を追加した。

### moq-sub の統合テスト (16 件)

`crates/moq-sub/src/pipeline.rs` の `mod peer_harness_integration_tests` に、`receive_catalog` の fill /
subgroup 経路と delta 適用、`CATALOG_TIMEOUT`、fill の request ID 不一致・デコード失敗・RESET での継続、
不正 delta の拒否、別 alias の読み捨て、`resubscribe_tracks` の FILL_PARAMETERS と REQUEST_UPDATE と
映像だけの FETCH、session 移行の音声連続性と重複排除、request 単位移行の音声連続性、`run` の
catalog 取得から shutdown まで、FETCH の side session 再発行と旧 stream の終端、FETCH 移行失敗の
警告と購読継続、side session 移行後の音声が 1 度だけ届くこと、audio の PUBLISH_DONE(TRACK_ENDED) 後も
映像が継続することを追加した。

### fuzz

`fuzz/fuzz_targets/fuzz_session.rs` に、`SendFetch` の request_id を保持し、
`RecvFetchResponse` (REQUEST_OK + FETCH_HEADER) / `RecvFetchEntry` / `ServeFetch` (server role で
FETCH を受けて `send_fetch_ok` / `send_fetch_header` / `send_fetch_object` /
`send_fetch_data_stream_closed` を呼ぶ) を追加した。FETCH の成功経路は入力バイト列の decode に
依存せず決定的に踏む。

### 変異確認

各テストについて、対応する実装を 1 箇所だけ壊してテストが落ちることを確認した (確認後に復元した)。

| 対象 | 壊した箇所 | 結果 |
| --- | --- | --- |
| moq-pub | SUBSCRIBE の受理条件から `config.input_mp4.is_none()` を削除 | 検出 (MP4 パススルーで NEW_GROUP_REQUEST を受理) |
| moq-pub | `published_track` の namespace 照合を削除 | 検出 |
| moq-pub | `SubgroupWriter` の Skip 判定を無効化 | 検出 |
| moq-pub | `catalog_fetch_response` の InvalidRange 判定を削除 | 検出 |
| moq-pub | `serve_media_fetch` の range filter 拒否を無効化 | 検出 |
| moq-pub | `serve_fetch_entries` の Non-Existent を Unknown で書く | 検出 |
| moq-pub | `serve_fetch_entries` を `finish` ではなく `reset` で閉じる | 検出 |
| moq-pub | MP4 パススルーの `video_timescale` を None にする | 検出 |
| moq-pub | 同一 URL 再発行で `audio_config_sent = false` を削除 | 検出 |
| moq-pub | 別 URL 移行 (`Migrated`) で `audio_config_sent = false` を削除 | 検出 |
| moq-pub | side session 共有を無効化して毎回接続する | 検出 |
| moq-pub | `run` の後始末を `close(0)` から `close(0x1)` に変える | 検出 |
| moq-pub | 移行期限切れの警告ログを削除 | 検出 |
| moq-pub | `GopTracker::request_new_group` を no-op にする | 検出 |
| moq-sub | `receive_catalog` の fill / subgroup の `apply_catalog_object` 呼び出しを削除 | 検出 |
| moq-sub | `CATALOG_TIMEOUT` を 100 ms に短縮 | 検出 |
| moq-sub | `read_catalog_fetch_stream` の request ID 照合を削除 | 検出 |
| moq-sub | fill の読み取り失敗を `receive_catalog` から伝播させる | 検出 |
| moq-sub | `apply_catalog_object` の Err を握りつぶす | 検出 |
| moq-sub | `read_catalog_subgroup_stream` の alias 判定を無効化 | 検出 |
| moq-sub | catalog 購読から FILL_PARAMETERS を削除 | 検出 |
| moq-sub | 再購読の Absolute Start を削除 | 検出 |
| moq-sub | 再発行の STOP_SENDING を送らない | 検出 |
| moq-sub | `run` で audio を購読しない | 検出 |
| moq-sub | `note_published_object` の呼び出しを削除 (FETCH が自動拒否される) | 検出 |
| moq-sub | FETCH 再発行でパラメータを渡さない | 検出 |
| moq-sub | 移行成功後の旧 FETCH stream の終端を削除 | 検出 |
| moq-sub | FETCH 移行失敗で `break 'main` する | 検出 |
| moq-sub | FETCH 移行失敗の警告ログを削除 | 検出 |
| moq-sub | `AudioDeliveryOrder::is_duplicate` を無効化 | 検出 |
| moq-sub | audio の PUBLISH_DONE を全 media 終了として扱う | 検出 |
| tokio-moq | `recv_request_start` の自動拒否検出を無効化 | 検出 |

### 検証

- `cargo test -p moq-pub --lib` (253 件) / `cargo test -p moq-sub --lib` (259 件) /
  `cargo test -p tokio-moq --lib` (297 件)
- `cargo test -p moq-pub -p moq-sub --lib --no-default-features --features quic-tobi` /
  `quic-ngtcp2` の統合テスト (15 件 / 16 件)
- `cargo +nightly fuzz build fuzz_session` / `cargo +nightly fuzz run fuzz_session -- -max_total_time=30`
  (516976 runs、クラッシュなし)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo clippy -p tokio-moq --all-targets --features test-support -- -D warnings` /
  `cargo fmt --all -- --check`

### 残る制約

- e2e は追加していない。real relay が request stream GOAWAY を送れることを確認できていないため、移行は
  peer_harness の統合テストだけで検証している。
- 配信中で Object を 1 件も cache に入れていない track への FETCH の応答は 0427 の範囲であり、
  テストを追加していない。
- `receive_catalog` は制御メッセージを処理しないため、fill fetch への REQUEST_ERROR は本 issue の
  テスト対象外である (`run` の `RequestErrorReceived` 経路で扱う)。
- ngtcp2 バックエンドの並列実行で、稀に受信待ちがタイムアウトすることがある (34 回中 2 回)。
  `--test-threads=1` では安定する。ngtcp2 の受信経路の残課題として切り分けが必要である。
