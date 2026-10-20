# moq-pub が受信 SUBSCRIBE の購読に Object と PUBLISH_DONE を送る

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-subscribe-serving
- Polished: 2026-10-20

## 目的

moq-pub は relay からの SUBSCRIBE を受理して SUBSCRIBE_OK を返すが、その request_id を購読の送出に使わず、Object も PUBLISH_DONE も送らない。結果として受信 SUBSCRIBE の購読は確立するだけで何も届かず、draft-ietf-moq-transport-22 の次の MUST / 規則を満たさない。

- §3.1 (Subscriptions): "When an Object matches the filters of multiple subscriptions to the same Track, the publisher MUST send the Object once for each matching subscription, even when those subscriptions share the same Track Alias."
- §3.1.1 (Pausing Subscriptions): "The publisher does not send Objects on a paused subscription, and does send them when it is not paused."
- §3.3.3 (Combining Filters): "The publisher MUST forward only objects that pass all filters."
- §6.4.2.2 (Graceful Request Stream Closure) / §9.9 (PUBLISH_DONE): "the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a FIN."

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は SUBSCRIBE を受理し、`send_subscribe_ok` を呼ぶが、受理した request_id を呼び出し元 (`run`) へ返さず、writer / datagram の送出に使わない。
- 送出 subgroup stream / datagram / PUBLISH_DONE は、自側が送った PUBLISH の `video_request_id` / `audio_request_id` / `catalog_request_id` に固定されている (`SubgroupWriter::new` / `DatagramWriter::new` / `publish_catalog_object` / `MoqtClient::send_publish_done`)。
- `src/session/data.rs` の `send_subgroup_object` は、stream が保持する request_id の subscription 1 個だけを `object_passes_filters` に渡す。
  受信 SUBSCRIBE の LOCATION_FILTER / Range Filter / `FORWARD=0` は subscription に保存されるが、その subscription には stream を開かないため評価されない。
  受信 SUBSCRIBE の `published_count` は 0 のままで、PUBLISH_DONE を送る経路も無い。
- 影響: relay が上流に追加の SUBSCRIBE を送る構成で、受信 SUBSCRIBE の購読には Object が届かない。pause (FORWARD=0) の有無にかかわらず Object が送られない。moq-pub の受信 SUBSCRIBE 経路のテストは無い。
- `serve_peer_request` の doc は「以後の Object は継続して送信中の subgroup stream で届く」としているが、これは PUBLISH 購読の stream であり、受信 SUBSCRIBE の購読には当てはまらない。

## 再現手順

1. `examples/moq-pub` を relay に接続し、video / audio / catalog の PUBLISH を確立する。
2. relay が同一 Track の上流 subscription を持っていても、追加の upstream SUBSCRIBE を作る構成にする (draft-ietf-moq-transport-22 §7.4 の "make additional subscriptions to the same Track")。
3. video track に対する SUBSCRIBE を送る。`serve_peer_request` は `send_subscribe_ok` を返す。
4. publisher の `SubgroupWriter` / `DatagramWriter` が `video_request_id` (PUBLISH の request_id) を使い続けるため、SUBSCRIBE の request_id には Object が送られず、PUBLISH_DONE も送られない。

コード経路の確認:

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は `request_id` を `send_subscribe_ok` に渡すだけで、`run` 側へ返さない。
- `run` の映像 / 音声 / catalog 送出は `video_request_id` / `audio_request_id` / `catalog_request_id` を `SubgroupWriter::new` / `DatagramWriter::new` / `publish_catalog_object` に渡す。
- `src/session/data.rs` の `send_subgroup_object` は `data_streams.outgoing` の `request_id` から subscription を引き、その 1 個だけで `object_passes_filters` を評価する。

## 設計方針

- `serve_peer_request` で受理した SUBSCRIBE の request_id と track 種別を呼び出し元 (`run`) へ返し、track ごとに保持する。既存の PUBLISH 購読 (`video_request_id` / `audio_request_id` / `catalog_request_id`) の送出は維持し、受信 SUBSCRIBE ごとに追加の stream / datagram を送る。
- 受理した受信 SUBSCRIBE ごとに一意な Track Alias を採番して SUBSCRIBE_OK に載せる
  (draft-ietf-moq-transport-22 §3.1: "A publisher MAY assign the same or different Track Aliases to these subscriptions.")。
  同じ Track Alias のまま購読ごとの stream を開くと、`Session::send_subgroup_header` の
  `SubgroupTracker::open` が同一 `(track_alias, group_id, subgroup_id)` の並行 stream を
  `PROTOCOL_VIOLATION` で拒否し、draft §11.3 の "A publisher SHOULD NOT open more than one
  stream at a time with the same Subgroup Header field values." にも反する。
  購読ごとに異なる Subgroup ID を使う設計でもよいが、どちらかで header field values を分ける。
- `serve_peer_request` は `subscribe_ok_properties` と `accepted_new_group_request` の呼び出しで `track_alias` を使って track 種別を判定しているため、購読ごとの alias を採番すると壊れる。track 種別を明示的に渡す形に変える。
- 映像 / 音声 / catalog の送出ループは、既存の PUBLISH 購読の writer に加えて、受理した各受信 SUBSCRIBE の writer にも同じ Object を書く。Object ID は全購読で同じ系列に保つ。映像 / 音声の `media_cache` への insert と音声の送信統計 (`record_audio_send`) は既存どおり 1 Object につき 1 回とし、PUBLISH 購読の writer の `Pass` を基準にする (受信 SUBSCRIBE のフィルタで cache を欠落させない)。
- 映像の受信 SUBSCRIBE は、次の Group (キーフレーム) の先頭から購読ごとの writer を開く。Group の途中で受理した購読に現在の Group の途中から送る場合は、`SubgroupObjectState` の `next_object_id` を現在の Object ID に合わせ、`prev_object_id` は最初の Object まで `None` にする。
- 音声の Audio Config は購読ごとに最初の Object に付与する (`audio_config_sent` を購読ごとに持つ)。既存の PUBLISH 購読で 1 回送った後も、後から受理した受信 SUBSCRIBE の最初の Object には Audio Config を付ける。
- フィルタ付き・Forward 0 の SUBSCRIBE は、`send_subgroup_object` のフィルタ評価で Object をスキップする設計とするか、SUBSCRIBE 受理時に `REQUEST_NOT_SUPPORTED` で明示的に拒否する。拒否する場合は `examples/moq-pub/README.md` に制限を書く。フィルタなし・Forward 1 の購読には必ず Object を送る (§3.1.1)。
- PUBLISH_DONE は、受理した各受信 SUBSCRIBE の bidi request stream に、その購読の全 outgoing stream を閉じた後に送る
  (`Session::send_publish_done` は open stream があると拒否する。§9.9: "A sender MUST NOT send PUBLISH_DONE until it has closed all streams it will ever open, and has no further datagrams to send, for a subscription.")。
  送信タイミングはセッション終了時 (shutdown) と GOAWAY 移行時の両方とし、status code は既存の PUBLISH と同じ `PUBLISH_DONE_GOING_AWAY` を使う。
  Stream Count は `MoqtClient::send_publish_done` が `Subscription::stream_counts.published_count` から決める。
- 受信 SUBSCRIBE が STOP_SENDING / RESET_STREAM で終端した場合は、その購読の writer を閉じて一覧から除く。終端済みの購読に PUBLISH_DONE を送らない。
- `MoqtClient::next_event` は現在 `RequestTerminated` を notable として返さないため、受信 SUBSCRIBE の終端をアプリが観測できない。`RequestTerminated` を notable に追加するか、`subscription_track_name(request_id)` が `None` になった時点で writer を閉じて一覧から除く経路を用意する。
- 回帰テストを追加する。受信 SUBSCRIBE の Object 配信・PUBLISH_DONE・フィルタ / Forward 0 の扱いを、
  `peer_harness` を使った統合テスト (0338 のテスト基盤) か、relay が上流に追加の SUBSCRIBE を送る構成の実リレー E2E で検証する。
  実リレー E2E は CODEBASE.md に従い `#[ignore]` を付け、CI が `--ignored` で実行する。
  既存の moq-sub はフィルタ / pause を送れないため、テスト側で `MoqtClient::subscribe_track_with_parameters` を使うか、テスト用の購読者を追加する。
  `MoqtClient` を使う場合は `e2e-tests` に `tokio-moq` の dev-dependency を追加する。

## 完了条件

- フィルタなし・Forward 1 の受信 SUBSCRIBE で確立した購読に Object が届くこと。
- フィルタ付き・Forward 0 の受信 SUBSCRIBE は、フィルタ / Forward を反映して Object を送るか、`REQUEST_NOT_SUPPORTED` で明示的に拒否すること。拒否した場合は `examples/moq-pub/README.md` に制限が書かれていること。
- 受理した各受信 SUBSCRIBE に PUBLISH_DONE が送られること (拒否した購読には REQUEST_ERROR)。PUBLISH_DONE の Stream Count がその購読で開いた stream 数と一致すること。
- セッション終了時 (shutdown / GOAWAY 移行) の両方で、受理した受信 SUBSCRIBE が PUBLISH_DONE で終端されること。購読者が STOP_SENDING / RESET_STREAM で終端した購読には PUBLISH_DONE を送らないこと。
- 受信 SUBSCRIBE のフィルタを追加しても、FETCH が返す Object が欠落しないこと。
- 受信 SUBSCRIBE の Object 配信・PUBLISH_DONE・フィルタ / Forward 0 の扱いを検証する回帰テストが追加され、実リレー E2E の場合は `#[ignore]` を付けて CI の `--ignored` で実行されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 実リレー E2E を追加した場合、`.github/workflows/e2e-test.yml` の実行で通ること。
