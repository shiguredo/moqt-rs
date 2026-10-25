# publisher 側 FETCH / fill 応答 Object の Location を要求範囲で検証する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §3.3.1 (Location Filters) は "A publisher MUST NOT send objects from outside the requested range." と
MUST を課す。§3.4 (Fill Semantics) は fill fetch stream が subscription の Location Filter を継承し、FILL_PARAMETERS 内の
LOCATION_FILTER が外側を上書きすると定める。subscription の送信経路は `object_passes_filters` でこれを強制しているが、
FETCH / fill fetch の応答経路は Object の Location を検証していないため、ライブラリ利用者が MUST 違反の Object を送れてしまう。

## 現状

- `src/session/data.rs` の `Session::send_fetch_object` は `outgoing_fetch` / `outgoing_fill` に登録済みの stream かどうかだけを
  検証し、Location を受け取らない。Object の Location とペイロードはアプリが直接 wire に書くため、Session は何も検査できない。
- `src/session/fetch.rs` の `handle_peer_fetch` は要求された LOCATION_FILTER から start / end を解決しているが、解決結果を
  `Fetch` に保存せず捨てている。publisher 役 `Fetch` の `fetch_start` は `None` のままで、要求範囲の終端も保持しない。
- `src/session/data.rs` の `OutgoingFetchStream` は `request_id` だけを保持し、fill range を保持しない。
- `src/session/subscription/fill.rs` の `should_open_fill_stream` は effective fill range を計算して
  `SessionEvent::OpenFillFetchStream` に載せているが、`Session::send_fill_fetch_header` はその range を受け取らない。
- `src/session/subscription/validation.rs` の `location_filter_pass` は Location Filter の範囲判定を行う。
- `examples/tokio-moq/src/moqt_client.rs` の `FetchResponseWriter` / `send_fetch_response` / `send_fill_fetch_response` は
  Object の Location を知っているが、Session へ渡していない。`tests/test_session/fetch/` も `send_fetch_object` を呼んでいる。
- FETCH 応答の End of Range (§11.4.1.2) は Object ではないため、Location 検証の対象外とし、別 API で通知する必要がある。

## 設計方針

- `src/session/fetch.rs` の `handle_peer_fetch` で解決した start / end を publisher 役 `Fetch` に保存する
  (`fetch_start` に start、新設の要求範囲終端フィールドに end)。`fetch_start` の doc を publisher 側も保持する形に更新する。
- `Session::send_fetch_object` を `(&mut self, stream_id: DataStreamId, location: &Location) -> Result<(), SendRequestError>`
  に変更し、次の判定を行う。
  - 通常の FETCH 応答: `fetch_start` / 要求範囲終端の両方の内側であること (`location_filter_pass` を再利用する)。
    あわせて、`send_fetch_ok` が保存した `Fetch::end_location` を超えないこと (FETCH_OK の End Location は
    `send_fetch_header` が Established を要求するため Object 送信時点で必ず確定している)。
  - fill fetch stream: `Session` に `pending_fill_ranges: HashMap<u64, (Location, Location)>` (fill request id →
    effective fill range) を追加し、`maybe_open_fill_stream` が `should_open_fill_stream` の計算結果を保存する。
    `send_fill_fetch_header` が該当 entry を取り出して `OutgoingFetchStream` に start / end を保存し、その範囲で判定する。
    未消費の entry は subscription 破棄時 (`forget_subscription` / cancel) に掃除する。range が不明な stream では
    判定をスキップする。
  - 範囲外は subscription の送信経路と同じ `SendRequestError::LocalFilterMismatch` を返す。セッションは閉じない。
- End of Range 用に `Session::send_fetch_end_of_range(stream_id) -> Result<(), SessionError>` を追加する。範囲検証は行わず、
  stream が登録済みであることだけを検証する。
- `examples/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle::send_fetch_object(stream_id, location)` /
  `send_fetch_end_of_range(stream_id)` を追加し、`FetchResponseWriter::write_object` は
  `Location { group_id: input.group_id, object_id: input.object_id }` を渡す (`FetchObjectInput` は `location`
  フィールドを持たない)。`write_end_of_non_existent_range` / `write_end_of_unknown_range` は location 引数を取らない
  `send_fetch_end_of_range` へ移す。`send_fetch_response` / `send_fill_fetch_response` も同様に Location を渡す。
- `FetchResponseWriter::write_object` は Session への通知を encoder の encode より前に行い、拒否された Object では
  encoder の直前 Location 状態を進めない。`DataPlaneHandle` の `send_fetch_object` / `send_fetch_end_of_range` は
  `SendRequestError` を `TransportError::Internal` に写して返し、呼び出し側 (`moq-pub` の FETCH / fill 応答経路) は
  その応答の送信を中断して stream の終端通知を行う (既存のエラー経路と同じ扱い)。
- `tests/test_session/fetch/` の `send_fetch_object` 呼び出しを Location 付きへ追従させ、FETCH 応答の Object が要求範囲内なら
  `Ok`、範囲外なら `LocalFilterMismatch`、End of Range は `send_fetch_end_of_range` で `Ok`、fill は effective fill range で
  判定されることをテストする。
- `docs/moqt.md` の該当記述を実装に合わせて更新する。`skills/shiguredo-moqt/SKILL.md` の data plane 送信 API 一覧を新しい
  署名に更新する。
- 本 issue では Location Filter の範囲検証と FETCH_OK の End Location 超過検証を対象とする。Range Filters (§3.3.2:
  Subgroup ID / Object ID / Publisher Priority / Object Properties) は、FETCH Object の API に Subgroup ID / Priority /
  Properties を渡す設計が必要になるため別 issue で扱う (既知の制約として doc に明記する)。

## 完了条件

- 要求 Location Filter の範囲外の Location で `send_fetch_object` を呼ぶと `SendRequestError::LocalFilterMismatch` が返り、
  セッションは閉じないこと。
- 要求範囲内の Location では `Ok` が返ること (no-filter の FETCH でも既定 range で判定されること)。
- fill fetch stream では FILL_PARAMETERS の上書きを反映した effective fill range で判定されること。
- End of Range は `send_fetch_end_of_range` で `Ok` となり、`send_fetch_object` の範囲検証を通らないこと。
- FETCH_OK の End Location を超える Location で `send_fetch_object` を呼ぶと `SendRequestError::LocalFilterMismatch` が
  返り、セッションは閉じないこと。
- FETCH_OK より前の Object 送信 (MAY) など、本 issue の範囲外の順序契約を変えないこと。Range Filters の評価は本 issue の
  対象外であることを doc に明記すること。
- `skills/shiguredo-moqt/SKILL.md` の API 一覧が新しい署名に更新されていること。
- `FetchResponseWriter` の拒否時に encoder の状態が進まず、呼び出し側が応答を中断できること (コードレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/session/types.rs` の `Fetch` に `request_end: Option<Location>` を追加し、`src/session/fetch.rs` の
  `handle_peer_fetch` が解決済みの start / end を publisher 役 fetch に保存するようにした。
- `src/session/data.rs` の `send_fetch_object` を `(stream_id, location: &Location) -> Result<(), SendRequestError>`
  に変更した。通常の FETCH 応答は `fetch_start` / `request_end` / `end_location` で Location を検証し、
  範囲外は `SendRequestError::LocalFilterMismatch` を返す。fill fetch stream は
  `OutgoingFetchStream::range` (effective fill range) で検証する。
- `OutgoingFetchStream` に `range: Option<(Location, Location)>` を追加し、送信 Object がない stream は
  `None` とした上で、fill stream の登録時に `Session::pending_fill_ranges` から effective fill range を移す。
  `maybe_open_fill_stream` が subscription ごとに計算結果を積み、`send_fill_fetch_header` が 1 件消費する。
  未消費の range は `forget_subscription` で掃除する。
- `Session::send_fetch_end_of_range(stream_id)` を追加した。End of Range は Object ではないため
  Location 検証を行わず、stream 登録の検証だけを行う。
- `examples/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle::send_fetch_object(stream_id, location)` /
  `send_fetch_end_of_range(stream_id)` を追加し、`FetchResponseWriter::write_object` は Session 通知を
  encode より先に行う (拒否時に encoder の直前 Location を進めない)。`write_end_of_non_existent_range` /
  `write_end_of_unknown_range`、`send_fetch_response` / `send_fill_fetch_response` を新 API へ追従させた。
- `tests/test_session/fetch/unified.rs` / `fill.rs` に、範囲外 Location の拒否、FETCH_OK の End Location 超過の
  拒否、End of Range の受理、fill の effective range 外の拒否を追加した。
- `docs/moqt.md` と `skills/shiguredo-moqt/SKILL.md` を新しい API と検証内容に更新した。Range Filters (§3.3.2)
  の評価は未対応であることを明記した。

## 完了条件の検証

- 要求範囲外の拒否: `fetch_object_outside_requested_range_is_rejected`
- FETCH_OK の End Location 超過の拒否: `fetch_object_beyond_ok_end_location_is_rejected`
- fill の effective range 外の拒否: `fill_object_outside_effective_range_is_rejected`
- End of Range の受理: `fetch_end_of_range_is_accepted_without_location_validation`
- 既存 API の呼び出し側 (`examples/tokio-moq`) を追従し、`cargo test --workspace` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功
