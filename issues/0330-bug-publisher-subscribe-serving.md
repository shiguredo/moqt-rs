# moq-pub が受信 SUBSCRIBE の購読に Object と PUBLISH_DONE を送る

- Created: 2026-10-19
- Completed: 2026-11-10
- Branch: feature/fix-publisher-subscribe-serving
- Polished: 2026-11-10
- Refreshed: 2026-11-04

## 目的

moq-pub は relay からの SUBSCRIBE を受理して SUBSCRIBE_OK を返すが、その request_id を購読の送出に使わず、Object も PUBLISH_DONE も送らない。結果として受信 SUBSCRIBE の購読は確立するだけで何も届かず、draft-ietf-moq-transport-23 の次の MUST / 規則を満たさない。

- §3.1 (Subscriptions): "When an Object matches the filters of multiple subscriptions to the same Track, the publisher MUST send the Object once for each matching subscription, even when those subscriptions share the same Track Alias."
- §3.1.1 (Pausing Subscriptions): "The publisher does not send Objects on a paused subscription, and does send them when it is not paused."
- §3.3.3 (Combining Filters): "The publisher MUST forward only objects that pass all filters."
- §6.4.2.2 (Graceful Request Stream Closure): "the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a FIN." (§9.9 の MUST NOT は後述のとおり別個に扱う)

## 現状

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` は SUBSCRIBE を受理し、`send_subscribe_ok` を呼ぶが、受理した request_id を呼び出し元 (`run`) へ返さず、writer / datagram の送出に使わない。
- 送出 subgroup stream / datagram / PUBLISH_DONE は、自側が送った PUBLISH の `video_request_id` / `audio_request_id` / `catalog_request_id` に固定されている (`SubgroupWriter::new` / `DatagramWriter::new` / `publish_catalog_object` / `MoqtClient::send_publish_done`)。
- `src/session/data.rs` の `send_subgroup_object` は、stream が保持する request_id の subscription 1 個だけを `object_passes_filters` に渡す。
  受信 SUBSCRIBE の LOCATION_FILTER / Range Filter / `PAUSED=1` (paused) は subscription に保存されるが、その subscription には stream を開かないため評価されない。
  受信 SUBSCRIBE の `published_count` は 0 のままで、PUBLISH_DONE を送る経路も無い。
  FILL_PARAMETERS 付き受信 SUBSCRIBE は例外がある: Session は fill fetch stream の開設を指示し、
  catalog への fill は応答する (media の fill は moq-pub が fill range を判定できないため
  FETCH_HEADER の直後で reset する)。Object の購読配信 (subgroup / datagram) がない本質は変わらない。
- 影響: relay が上流に追加の SUBSCRIBE を送る構成で、受信 SUBSCRIBE の購読には Object が届かない。pause (PAUSED=1) の有無にかかわらず Object が送られない。moq-pub の受信 SUBSCRIBE 経路のテストは無い。
- `serve_peer_request` の doc は「以後の Object は継続して送信中の subgroup stream で届く」としているが、これは PUBLISH 購読の stream であり、受信 SUBSCRIBE の購読には当てはまらない。

## 再現手順

1. `crates/moq-pub` を relay に接続し、video / audio / catalog の PUBLISH を確立する。
2. relay が同一 Track の上流 subscription を持っていても、追加の upstream SUBSCRIBE を作る構成にする (draft-ietf-moq-transport-23 §7.5 の "make additional subscriptions to the same Track")。
3. video track に対する SUBSCRIBE を送る。`serve_peer_request` は `send_subscribe_ok` を返す。
4. publisher の `SubgroupWriter` / `DatagramWriter` が `video_request_id` (PUBLISH の request_id) を使い続けるため、SUBSCRIBE の request_id には Object が送られず、PUBLISH_DONE も送られない。

コード経路の確認:

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` は `request_id` を `send_subscribe_ok` に渡すだけで、`run` 側へ返さない。
- `run` の映像 / 音声 / catalog 送出は `video_request_id` / `audio_request_id` / `catalog_request_id` を `SubgroupWriter::new` / `DatagramWriter::new` / `publish_catalog_object` に渡す。
- `src/session/data.rs` の `send_subgroup_object` は `data_streams.outgoing` の `request_id` から subscription を引き、その 1 個だけで `object_passes_filters` を評価する。

## 設計方針

- `serve_peer_request` で受理した SUBSCRIBE の request_id と track 種別を呼び出し元 (`run`) へ返し、track ごとに保持する。既存の PUBLISH 購読 (`video_request_id` / `audio_request_id` / `catalog_request_id`) の送出は維持し、受信 SUBSCRIBE ごとに追加の stream / datagram を送る。
- 受理した受信 SUBSCRIBE ごとに一意な Track Alias を採番して SUBSCRIBE_OK に載せる
  (draft-ietf-moq-transport-23 §3.1: "A publisher MAY assign the same or different Track Aliases to these subscriptions.")。
  同じ Track Alias のまま購読ごとの stream を開くと、`Session::send_subgroup_header` の
  `SubgroupTracker::open` が同一 `(track_alias, group_id, subgroup_id)` の並行 stream を
  `PROTOCOL_VIOLATION` で拒否し、draft §11.3 の "A publisher SHOULD NOT open more than one
  stream at a time with the same Subgroup Header field values." にも反する。
  購読ごとに異なる Subgroup ID を使う設計でもよいが、どちらかで header field values を分ける。
- `serve_peer_request` は `subscribe_ok_properties` と `accepted_new_group_request` の呼び出しで `track_alias` を使って track 種別を判定しているため、購読ごとの alias を採番すると壊れる。track 種別を明示的に渡す形に変える。
- 映像 / 音声 / catalog の送出ループは、既存の PUBLISH 購読の writer に加えて、受理した各受信 SUBSCRIBE の writer にも同じ Object を書く。Object ID は全購読で同じ系列に保つ。映像 / 音声の `media_cache` への insert と音声の送信統計 (`record_audio_send`) は既存どおり 1 Object につき 1 回とし、PUBLISH 購読の writer の `Pass` を基準にする (受信 SUBSCRIBE のフィルタで cache を欠落させない)。
- 映像の受信 SUBSCRIBE は、次の Group (キーフレーム) の先頭から購読ごとの writer を開く。Group の途中で受理した購読に現在の Group の途中から送る場合は、`SubgroupObjectState` の `next_object_id` を現在の Object ID に合わせ、`prev_object_id` は最初の Object まで `None` にする。
- 音声の Audio Config は購読ごとに最初の Object に付与する (`audio_config_sent` を購読ごとに持つ)。既存の PUBLISH 購読で 1 回送った後も、後から受理した受信 SUBSCRIBE の最初の Object には Audio Config を付ける。
- フィルタ付き・`PAUSED=1` (paused) の SUBSCRIBE は、`send_subgroup_object` のフィルタ評価で Object をスキップする設計とするか、SUBSCRIBE 受理時に `REQUEST_NOT_SUPPORTED` で明示的に拒否する。拒否する場合は `crates/moq-pub/README.md` に制限を書く。フィルタなし・`PAUSED=0` (unpaused) の購読には必ず Object を送る (§3.1.1)。
- FILL_PARAMETERS 付き受信 SUBSCRIBE の fill fetch stream の扱いは現行どおりとする (catalog の fill は応答し、media の fill は FETCH_HEADER の直後で reset する)。fill による catalog 適用と、受信 SUBSCRIBE への購読配信 (追加の subgroup / datagram 送出) は併存させる。
- PUBLISH_DONE は、受理した各受信 SUBSCRIBE の bidi request stream に、その購読の全 outgoing stream を閉じた後に送る
  (`Session::send_publish_done` は open stream があると拒否する。§9.9: "A sender MUST NOT send PUBLISH_DONE until it has closed all streams it will ever open, and has no further datagrams to send, for a subscription.")。
  送信タイミングはセッション終了時 (shutdown) と GOAWAY 移行時の両方とし、status code は既存の PUBLISH と同じ `PUBLISH_DONE_GOING_AWAY` を使う。
  Stream Count は `MoqtClient::send_publish_done` が `Subscription::stream_counts.published_count` から決める。
- 受信 SUBSCRIBE が STOP_SENDING / RESET_STREAM で終端した場合は、その購読の writer を閉じて一覧から除く。終端済みの購読に PUBLISH_DONE を送らない。
- `MoqtClient::next_event` は現在 `RequestTerminated` を notable として返さないため、受信 SUBSCRIBE の終端をアプリが観測できない。`RequestTerminated` を notable に追加するか、`subscription_track_name(request_id)` が `None` になった時点で writer を閉じて一覧から除く経路を用意する。
- 回帰テストを追加する。検証手段は次の 1 方針に確定する (`peer_harness` / 0338 のテスト基盤を
  前提にしない。0338 は本 issue の実装後に着手すると「依存」で自ら定めている):
  session 層の既存テスト (`tests/test_session/`) で、受信 SUBSCRIBE 購読への送出 API
  (per-subscription の `send_subgroup_header` / `send_object_datagram` / Track Alias 採番 /
  `send_publish_done` の Stream Count) の挙動を固定する。受信 SUBSCRIBE の Object 配信・
  PUBLISH_DONE・フィルタ / `PAUSED=1` の扱いは、relay が上流に追加の SUBSCRIBE を送れる構成の
  実リレー E2E (`#[ignore]` を付け、CI が `--ignored` で実行) で検証する。relay 側構成が用意
  できない場合は、0338 の基盤整備後に行う統合テストへ置き換える
  (`MoqtClient::subscribe_track_with_parameters` でテスト購読者を用意する形)。
  既存の moq-sub は PAUSED と Range Filter を送れず、Location Filter は Absolute Start の
  1 種に限られるため、テスト側で `MoqtClient::subscribe_track_with_parameters`
  を使う (`e2e-tests` に `tokio-moq` の dev-dependency を追加する)。
  session 層の送出挙動を固定する単体テストは 0338 を待たず本 issue で追加する。

## 完了条件

- フィルタなし・`PAUSED=0` (unpaused) の受信 SUBSCRIBE で確立した購読に Object が届くこと。
- フィルタ付き・`PAUSED=1` (paused) の受信 SUBSCRIBE は、フィルタ / `PAUSED` を反映して Object を送るか、`REQUEST_NOT_SUPPORTED` で明示的に拒否すること。拒否した場合は `crates/moq-pub/README.md` に制限が書かれていること。
- 受理した各受信 SUBSCRIBE に PUBLISH_DONE が送られること (拒否した購読には REQUEST_ERROR)。PUBLISH_DONE の Stream Count がその購読で開いた stream 数と一致すること。
- セッション終了時 (shutdown / GOAWAY 移行) の両方で、受理した受信 SUBSCRIBE が PUBLISH_DONE で終端されること。購読者が STOP_SENDING / RESET_STREAM で終端した購読には PUBLISH_DONE を送らないこと。
- 受信 SUBSCRIBE のフィルタを追加しても、FETCH が返す Object が欠落しないこと。
- 受信 SUBSCRIBE 購読への送出 API の挙動を固定する session 層の単体テストが追加されること
  (per-subscription の送出、Track Alias 採番、`send_publish_done` の Stream Count)。
- 受信 SUBSCRIBE の Object 配信・PUBLISH_DONE・フィルタ / `PAUSED=1` の扱いを検証する回帰テストが追加され、
  実リレー E2E の場合は `#[ignore]` を付けて CI の `--ignored` で実行されること
  (relay 側構成が用意できない場合は、session 層の単体テストで送出経路を固定し、統合テスト (0338) に
  配信経路の検証を引き継ぐことを issue の解決方法に明記すること)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` を書き換えた。受信 SUBSCRIBE を受理したとき、Track Alias を PUBLISH の固定値から採番する代わりに `next_peer_alias` カウンタ (PUBLISH の固定値 0 / 1 / 2 の次から) で購読ごとに一意な alias を採番して SUBSCRIBE_OK に載せる (draft-ietf-moq-transport-23 §3.1 の "A publisher MAY assign the same or different Track Aliases to these subscriptions." / 同一 (track_alias, group_id, subgroup_id) の並行 stream を避ける §11.3 の SHOULD NOT)。`subscribe_ok_properties` を track 種別 (新設 `PublishedTrack`) を明示的に受け取る形に変え、alias による種別判定を除去した。受理結果を新設の `AcceptedPeerSubscribe` (request_id / track / alias) として呼び出し元へ返す。
- 受信 SUBSCRIBE の購読の送出状態を新設の `PeerSubscription` (request_id / track / alias / writer / audio_config_sent / ended) で管理する。映像は Group 境界で PUBLISH 購読の writer と同じく閉じて開き直し、次の Group (キーフレーム) の先頭から配信する (Group の途中で受理した購読は次の Group から届く)。各 Object は PUBLISH 購読の writer に加えて各受信 SUBSCRIBE の writer にも書く (§3.1 の MUST)。Object ID は writer を Group 境界で開き直すため PUBLISH 購読と同じ系列に保たれる。フィルタと `PAUSED=1` は Session が subscription ごとに評価する設計 (`send_subgroup_object` の `object_passes_filters`) を採用した (README 制限の追加は不要)。
- 音声は Audio Config を購読ごとの最初の Object に付与する (`audio_config_sent` を購読ごとに保持)。datagram モードの購読は Object ごとに `DatagramWriter` を作り、subgroup モードの購読は PUBLISH 側と同じ 1 object = 1 stream で送る。`media_cache` への insert と送信統計 (`record_audio_send`) は PUBLISH 購読の `Pass` を基準に従来どおり 1 Object につき 1 回とする。
- カタログは受理した時点で現在のカタログ Object を新しい Group で送り直し (`republish_catalog_at_next_group`)、以後の republish ごとに受信 SUBSCRIBE の購読にも同じ Object を書く (`republish_catalog_at_group` を拡張)。`catalog_json` は受信 SUBSCRIBE の writer にも同じ Object を送るための経路として使う。
- 終端検出は tick で `subscription_track_name(request_id)` が `None` になった購読を送出一覧から除く (writer の finish と `ended` 設定)。
- PUBLISH_DONE: run 終了時の後始末で受信 SUBSCRIBE の購読の writer を閉じ (`finish`)、`PUBLISH_DONE(GOING_AWAY)` を送る (§6.4.2.2 の MUST / §9.9 の MUST NOT)。終端済み (`ended`) の購読には送らない。GOAWAY 移行時も同じ処理を移行分岐に入れ、旧 session での送信を `shutdown_old_session` に `peer_request_ids` として渡す。Session が open stream があると拒否するため、PUBLISH_DONE の前に writer を閉じる順序を保証する。
- フィルタ付き・`PAUSED=1` の受信 SUBSCRIBE は `object_passes_filters` で Object をスキップする設計としたため、明示的な拒否 (`REQUEST_NOT_SUPPORTED`) と README 制限は行わない。
- テスト: session 層の送出 API の挙動を固定する単体テスト `tests/test_session/peer_subscribe_serving.rs` を新規追加した (受信 SUBSCRIBE 購読への subgroup 送出 + Stream Count 1 の PUBLISH_DONE 成功 / datagram 送出 / open stream 残置時の PUBLISH_DONE 拒否)。
