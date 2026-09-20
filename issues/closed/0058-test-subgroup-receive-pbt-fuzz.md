# 受信 subgroup 経路の PBT と fuzz を追加する

- Created: 2026-09-11
- Completed: 2026-09-18
- Branch: feature/test-subgroup-receive-pbt-fuzz
- Polished: 2026-09-18

## 目的

受信 subgroup 経路に「候補ごとのフィルタ再適用と帰属」「stream 会計（open 数）」「delivery timeout override のライフサイクル」が加わったが、固定ケースの単体テストでしか検証されていない。ランダムな操作列で不変条件を検証し、共有 Track Alias まわりの不整合の再発を防ぐ。

## 現状

- `pbt/tests/prop_session/` には受信 subgroup の帰属・会計・override を対象にした PBT がない（`datagram.rs` に datagram 経路の PBT はある）。
- `fuzz/fuzz_targets/fuzz_session.rs` の `Op` は data stream type の通知と close のみで、subgroup header / object の受信操作がない。
- 単体テストでは、帰属先 subscription の forget、キャンセル、Malformed 検出、override 削除の組合せが個別ケースでしか固定されていない。

## 設計方針

- PBT（noprop）で次の不変条件を検証する。
  - `open_incoming_subgroup_count` が incoming の生きた data stream 数 (subgroup + fill fetch) と一致し、全 stream 終端 / forget 後に 0 になる
  - 全 stream 終端 / forget 後に delivery timeout override が残らない
  - 帰属先が「候補順で最初の非キャンセルかつ `object_passes_filters` 合格」で決定的である
  - `FilteredOut` / `Discarded` の Object が subscription スコープの状態（`largest_received_location` / `ended_groups` / Object tracker）を更新しない
- fuzz の `Op` に subgroup header / object / close の受信操作を追加し、panic と状態破壊を検出する。decode 済みの構造体を直接渡す操作列でよい。
- 単体テストと同じ固定ケースを PBT で重複させない。PBT はサンプラーで入力の組合せを生成し、単体テストはエラーパス・境界値に残す。
- 既存テストの流儀に合わせ、モックやスタブは使わない。

## 完了条件

- 上記の不変条件が `pbt/tests/prop_session/` で検証されていること
- `fuzz_session` が subgroup 経路の操作列を生成し、クラッシュしないこと
- `cargo test -p pbt` と `cargo check --manifest-path fuzz/Cargo.toml` が通ること

## 解決方法

`pbt/tests/prop_session/subgroup.rs` を追加し、`pbt/tests/prop_session/main.rs` に
`mod subgroup;` を登録した。`fuzz/fuzz_targets/fuzz_session.rs` の `Op` に受信 subgroup 経路の
操作を追加した。`src/` は変更していない。

### PBT で検証した不変条件

- `subgroup_receive_stream_accounting`: 同一 Track Alias を共有する 2〜3 subscription を
  確立し、stream の open / Object 受信 / FIN・RESET 終端 / subscription のキャンセルと
  forget をランダムな順序で適用する。`open_incoming_subgroup_count` の全 subscription 合計が
  モデルが追跡する open 中の受信 stream 数と各操作後に一致し、全 stream 終端 / forget 後に
  0 に戻ること、および残った subscription に per-subgroup delivery timeout override が
  残らないことを検証する。帰属判定も候補順のモデルと突き合わせる。
- `subgroup_object_attribution_is_deterministic`: 互いに重複しない OBJECTID_FILTER を候補ごとに
  設定し、任意の Object ID の帰属先が「候補順で最初の生きた合格候補」に一意に決まること、
  帰属先以外の subscription スコープの状態 (`largest_received_location` / `ended_groups` /
  `end_of_track` / override / 受信 stream 数 / Forward State / Terminated) が変化しないことを
  検証する。
- `discarded_object_does_not_reach_object_trackers`: `FilteredOut` / `Discarded` になった
  Object が Object 系 tracker に到達しないことを、PRIOR_OBJECT_ID_GAP を宣言した Object を
  破棄させたうえで gap 内の Object が Malformed Track にならないことで観測する。キャンセルも
  フィルタもせず受理させた参照ケースでは同じ入力が Malformed Track になることも合わせて
  検証する。
- `subgroup_delivery_timeout_override_lifecycle`: 先頭 Object の
  SUBGROUP_DELIVERY_TIMEOUT / OBJECT_DELIVERY_TIMEOUT が帰属先 subscription の
  per-subgroup override として登録され、FIN 終端と forget の両経路で削除されること、fill fetch
  stream も Stream Count の open 数に含まれることを検証する。
- `decoded_subgroup_header_and_object_do_not_break_session`: fuzz の `Op` と同じ手順
  (encode 済みバイト列を `SubgroupHeader::decode` してから通知し、decode 済みの
  `DecodedSubgroupObject` を渡す) を property として反復し、panic せず受理判定と会計が
  モデルと一致することを検証する。

### fuzz の Op 追加内容

- `RecvSubgroupHeader { stream_id, bytes }`: 入力バイト列を `SubgroupHeader::decode` して
  `recv_subgroup_header` に渡す。
- `RecvSubgroupObject { stream_id, object_id, status, properties_bytes }`: decode 済みの
  `DecodedSubgroupObject` を直接渡す。status の有無で payload 長を 1 / 0 に切り替える。
- `RecvFetchHeader { stream_id, bytes }`: 入力バイト列を `FetchHeader::decode` して
  `recv_fetch_header` に渡し、fill fetch stream の会計経路も踏む。

いずれも送信で発行した request_id とは相関させない。`Op` の doc コメントに「受信 subgroup
経路も相関させず、共有 Track Alias の帰属判定・stream 会計・override のライフサイクルを
任意の操作列で fuzz する」旨を追記した。

### 検証

- `cargo test --workspace` / `cargo test -p pbt`
- `cargo check --manifest-path fuzz/Cargo.toml`
- `cargo +nightly fuzz run fuzz_session -- -runs=10000` と同 50000 回 (`-max_len=4096`) で
  panic なし
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`

### 判断した点

- PBT のモデルは Session の内部状態 (`data_streams.incoming`) を参照できないため、公開 API の
  `Subscription::stream_counts` と操作列から「open 中の受信 stream 数」を追跡する。キャンセル
  直後は Session が次の Object 受信まで会計を保持する挙動に合わせ、モデル側も帰属実績のある
  stream だけを drain 中として数える。
- キャンセルと forget は open 中の stream が 1 本も無いときにだけ選ぶ。stream の所有者は最初の
  Object を受理するまで確定せず、キャンセル後の会計が移管先の subscription へ移るため、
  stream と独立に扱えるこの条件下で会計の不変条件を検証する。
- subscription スコープの Object 系 tracker (`peer_object_properties` / `peer_object_fields`) は
  非公開のため、フィルタ不通過 Object の到達有無を後続 Object の Malformed Track 検出として
  観測した。
- `SubgroupIdMode::FirstObjectId` / `Zero` と `END_OF_GROUP` の組合せは、単体テスト
  (`tests/test_session/subgroup_object_filter/`) が境界とエラーパスを固定しているため、PBT では
  `Explicit` のみを生成して重複を避けた。
