# 未対応 request の Range Filter を §3.3.2 の MUST で検証する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-request-range-filter-validation
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §3.3.2 (Range Filters) の 2 つの MUST を未対応 request の経路でも満たす。

> If the same combination of Parameter Type, SetID, and Property Type (only in the Track and
> Object Property Filters) repeat in any message, an endpoint MUST reject this with REQUEST_ERROR
> with error code INVALID_FILTER.

> Range Filters are only allowed if the setup option MAX_FILTER_RANGES is non-zero, which limits
> the total number of Ranges allowed in all Range Filter parameters for a given subscription or
> fetch. If this limit is exceeded, an endpoint MUST reject this with REQUEST_ERROR with error
> code INVALID_FILTER.

未対応 request の 3 種 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) のうち
SUBSCRIBE_TRACKS は §3.6.2 (Parameters on SUBSCRIBE_TRACKS) により SUBSCRIBE の許可
パラメータを継承し、Range Filter 5 種 (0x25-0x29) を載せられる (§9.18 (SUBSCRIBE_TRACKS))。
本経路でも §3.3.2 の MUST を満たす。

## 現状

- `src/session/core.rs` の `Session::handle_unsupported_request` は 1. Request ID →
  2. 本体解析 → 3. 本体 Request ID 一致 → 4. パラメータのスコープ検証 (§9.20.1) →
  5. AUTHORIZATION_TOKEN の一意性 (§8.9) → 6. REGISTER の適用 (§8.9) → 7. 予約・セッション
  名前空間 (§2.4.3 / §6.5) → 8. `GOING_AWAY` / `NOT_SUPPORTED` の順で処理する。Range Filter の
  内容は検証しない。
- `src/session/core.rs` の `Session::check_incoming_range_filters` が自側の
  `MAX_FILTER_RANGES` (`local_max_filter_ranges`) との比較、`MessageParameters::
  validate_range_filters` による重複・デルタ溢出の検証、FILL_PARAMETERS (§9.20.15) の内側の
  Range Filter の検証を行う。受理する request では `src/session/subscription/recv.rs` の
  `accept_incoming_track_request` (SUBSCRIBE / TRACK_STATUS) / `handle_peer_publish` /
  `handle_peer_request_update`、`src/session/fetch.rs` の `handle_peer_fetch` が呼ぶ。
- そのため SUBSCRIBE_TRACKS が §3.3.2 に違反する Range Filter を持っていても、`INVALID_FILTER`
  ではなく後続の拒否 (`NOT_SUPPORTED`、予約・セッション名前空間なら `DOES_NOT_EXIST`、control
  GOAWAY 送信済みなら `GOING_AWAY`) になる。
- `tests/test_session/request_stream.rs` の
  `subscribe_tracks_allows_track_property_filter_with_other_parameters` は `establish_pair()`
  (自側 `MAX_FILTER_RANGES` 未宣言 = 0) で 1 Range を持つ TRACK_PROPERTY_FILTER と
  LOCATION_FILTER を送り `NOT_SUPPORTED` を期待している。本 issue の実装後は `INVALID_FILTER`
  になるため、テストの更新が必要である。

## 設計方針

- `handle_unsupported_request` で `check_incoming_range_filters` を呼び、違反時は
  `Session::emit_request_error` で `REQUEST_ERROR` の `INVALID_FILTER` (0x36) を FIN 付きで
  送ってメッセージ単位に拒否し、セッションは維持する (`rejected_request_ids` に記録され、
  後続の終端通知は no-op になる)。
- 検証順は 0273 で確定した順序を引き継ぎ、Range Filter を 6 の後ろ、8 の前に挿入する。
  1. Request ID の parity と重複
  2. 本体の解析
  3. 本体の Request ID とメッセージの Request ID の一致
  4. パラメータのスコープ検証 (§9.20.1 の MUST)
  5. AUTHORIZATION_TOKEN の一意性 (§8.9 の MUST)
  6. REGISTER の token cache への適用 (§8.9 の MUST)
  7. Range Filter の検証 (§3.3.2 の MUST。違反は `INVALID_FILTER`)
  8. 予約・セッション名前空間の検証 (§2.4.3 / §6.5 の MUST。違反は `DOES_NOT_EXIST`)
  9. `GOING_AWAY` (control GOAWAY 送信済み) または `NOT_SUPPORTED`

  REGISTER 適用を 7 より前に置くのは、§8.9 の REGISTER MUST ("even if the message fails for
  other reasons") を満たすためである。8 を 9 より前に置くのは 0273 の決定 (§2.4.3 / §6.5 の
  MUST を §9.2 の MAY / §1.6 の SHOULD より優先する) を引き継ぐためである。
- メッセージ単位の拒否の優先順は `MALFORMED_AUTH_TOKEN` > `INVALID_FILTER` >
  `DOES_NOT_EXIST` > `GOING_AWAY` > `NOT_SUPPORTED` とする。セッション終了を招く検証 (本体
  解析・Request ID 不一致・スコープ違反・REGISTER 適用失敗) はこれらより常に先である。
  受理経路では `accept_peer_request` が publisher 側 request の `GOING_AWAY` を Range Filter
  より先に判定するが、未対応 request 経路には `accept_peer_request` が無く、`INVALID_FILTER`
  は MUST、`GOING_AWAY` は MAY のため、本経路では `INVALID_FILTER` を先に置く。この差は意図した
  ものである。
- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE は Range Filter を許可しない (§9.14 / §9.15 は
  AUTHORIZATION_TOKEN のみを許可)。Range Filter が載った場合は 4 のスコープ検証が先に
  `PROTOCOL_VIOLATION` でセッションを閉じるため、7 には到達しない。3 種共通で 7 を呼ぶが、
  テストで順序を固定する。
- 未対応 request は subscription を作らないため、Range 総数は 1 メッセージ内 (FILL_PARAMETERS
  の内側を含む) の検証に限る。累積の subscription 単位の検証は行わない。
- Length=0 の Range Filter (Range 0 個) は `check_incoming_range_filters` の既存方針どおり
  上限判定を通過し、`NOT_SUPPORTED` のまま拒否する (§3.3.2 は受信側に Range 数 0 の拒否 MUST を
  課していない)。
- 次の doc を実装に合わせて更新する。
  - `src/session/core.rs` の `Session::recv_request` / `Session::handle_unsupported_request`
    の doc (検証順を 1〜9 に更新し、拒否コードに `INVALID_FILTER` を追加する)
  - `src/session/types.rs` の `RequestKind` の doc
  - `src/message.rs` の `ControlMessage::Unsupported` の doc
  - `examples/tokio-moq/src/moqt_client.rs` の `BidiMessage::PeerRequestRejected` の doc
  - `docs/moqt.md` の「未対応」節 (拒否コードの優先順を
    `MALFORMED_AUTH_TOKEN` > `INVALID_FILTER` > `DOES_NOT_EXIST` > `GOING_AWAY` >
    `NOT_SUPPORTED` に更新し、検証一覧に §3.3.2 / §9.1.6 を追加する)
  - `Session::check_incoming_range_filters` の doc に、未対応 request 経路からも呼ばれること
    を追記する
- 0274 (未対応 request の未登録 alias の扱い) の (a)/(b) は先取りしない。未登録 alias の扱いは
  現行のまま (受理経路はセッション終了、拒否経路はメッセージ単位の拒否) とし、本 issue では
  変えない。Range Filter の検証は alias 解決・一意性検証より後ろにあるため、未登録 alias と
  Range Filter 違反が同時に成立する場合は `INVALID_FILTER` になる。0274 で (b) を採用する場合は
  順序の再調整が必要になる。
- テストは `tests/test_session/request_stream.rs` に追加する。server 側の SETUP に
  `MAX_FILTER_RANGES` を宣言する場合は `tests/test_session.rs` の `establish_pair_with_options`
  と `opts_with(SETUP_OPTION_MAX_FILTER_RANGES, n)` を使う。Range Filter の wire 表現は
  `PARAM_SUBGROUP_FILTER` (0x29) の LengthPrefixed を直接組み立てる (既存の
  `unsupported_request_with_namespace_and_raw_parameters` が使える)。モック / スタブは使わない。

## 完了条件

- 重複検出 (§3.3.2 の 1 つ目の MUST) を上限判定と切り分けて固定すること。
  server の `MAX_FILTER_RANGES=2`、同一 (Parameter Type, SetID, Property Type) の 1 Range
  フィルタ 2 個 (合計 2 Range で上限内) を持つ SUBSCRIBE_TRACKS が `INVALID_FILTER` で拒否され、
  reason が `"Range Filter validation failed"` であること。セッションは `Established` を維持する。
- 上限超過 (§3.3.2 の 2 つ目の MUST) を固定すること。server の `MAX_FILTER_RANGES=1`、1 個の
  フィルタに 2 Range を持つ SUBSCRIBE_TRACKS が `INVALID_FILTER` で拒否され、reason が
  `"Range Filters exceed MAX_FILTER_RANGES"` であること。
- 既定値 `MAX_FILTER_RANGES=0` で 1 Range を持つ SUBSCRIBE_TRACKS が `INVALID_FILTER` で拒否
  されること。
- FILL_PARAMETERS の内側の Range Filter も検証されること。`MAX_FILTER_RANGES=2` で内側に
  同一フィルタ 2 個 (合計 2 Range で上限内) を持つ SUBSCRIBE_TRACKS が `INVALID_FILTER` で
  拒否され、reason が `"Range Filter validation failed inside FILL_PARAMETERS"` であること。
- Length=0 の Range Filter を持つ SUBSCRIBE_TRACKS は従来どおり `NOT_SUPPORTED` で拒否される
  こと。
- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE に Range Filter が載った場合は、スコープ検証
  (§9.20.1) が先に `PROTOCOL_VIOLATION` でセッションを閉じること。
- 優先順がテストで固定されていること。
  - 重複トークン + Range Filter 違反: `MALFORMED_AUTH_TOKEN`
  - `.` 単独の予約名前空間 + Range Filter 違反: `INVALID_FILTER`
  - control GOAWAY 送信済み + Range Filter 違反: `INVALID_FILTER`
  - スコープ違反 (許可されないパラメータ) + Range Filter 違反: `PROTOCOL_VIOLATION` で
    セッション終了
- `INVALID_FILTER` で拒否する場合も REGISTER が token cache に登録されること (§8.9 の MUST)。
- 既存テスト `subscribe_tracks_allows_track_property_filter_with_other_parameters` を更新し、
  Range Filter を含む正当な SUBSCRIBE_TRACKS が引き続き `NOT_SUPPORTED` で拒否されることを
  維持すること (server の `MAX_FILTER_RANGES` を、テストが送る Range 総数以上に宣言する)。
- doc と実装が一致していること。「設計方針」に列挙した doc を更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
