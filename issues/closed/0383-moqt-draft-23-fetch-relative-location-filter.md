# 相対 Location Filter を持つ FETCH を INVALID_RANGE で拒否する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.2、§9.19.9](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/fetch.rs` の `handle_peer_fetch` は `location_filter_typed` の結果を `effective_start_location` / `effective_end_location` で解決するため、`RelativeGroup` / `NextObject` を絶対 FETCH と同じように処理する。
`send_fetch` も相対形式自体を禁止していない。

## 設計方針

単独 FETCH の相対 Location Filter を、フィルタの形式に基づいて早期に判別する。
受信時はセッションを維持して REQUEST_ERROR (INVALID_RANGE) を返し、Fetch state やデータ配送を開始しない。
送信 API は不正な単独 FETCH を送らない。
FILL_PARAMETERS の相対フィルタは正当なので、この禁止を流用しない。

## 完了条件

- 相対開始と Next Object の単独 FETCH が INVALID_RANGE で拒否されること。
- 絶対範囲・NoFilter・省略は従来どおり判定されること。
- fill の相対フィルタが引き続き利用でき、拒否時に request / data state が残らないこと。

## 解決方法

- `src/message_parameter.rs` の `LocationFilter` に `is_relative` を追加した。
  draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter) の Table 6 で
  Largest Object を基準に開始位置が定まるのは Type 0x01 (Relative Start) と
  Type 0x05 (Next Object) だけであり、この 2 形式を相対として判定する。
  判定を解決後の Location ではなくフィルタの形式で行うのは、Largest Object 未観測の
  相対フィルタが `{0, 0}` に解決され、絶対指定と区別できなくなるためである。
- 受信経路 `Session::handle_peer_fetch` は、LOCATION_FILTER を typed にデコードした直後
  (範囲解決と Fetch state 作成より前) に相対フィルタを検出し、`emit_request_error` で
  理由句 `relative location filter is not allowed in FETCH` を持つ REQUEST_ERROR
  (INVALID_RANGE) を返して `Ok(())` で抜ける (§3.2 (Fetch) の MUST)。セッションは維持し、
  `Fetch` と `request_streams` を登録しない。拒否済み Request ID は
  `emit_request_error` が `rejected_request_ids` に記録するため、その後届く bidi
  request stream close は no-op で吸収される。
- 送信経路 `Session::send_fetch` は、`location_filter_typed` で得たフィルタが相対なら
  範囲解決と Request ID 発行より前に `SendRequestError::Session`
  (SESSION_PROTOCOL_VIOLATION) を返す。peer が必ず INVALID_RANGE で拒否するメッセージを
  送出せず、Request ID の欠番も作らない。範囲解決は `location_filter_typed` の結果を使う
  形に整理し、省略 / Type 0x00 / 絶対形式の解決結果は変えていない。
- FILL_PARAMETERS の内側の Location Filter は `src/session/subscription/fill.rs` の
  `should_open_fill_stream` が §3.4 (Fill Semantics) の fill range 解決規則で扱うため、
  `is_relative` による禁止を流用していない。内側の相対フィルタは従来どおり fill fetch
  stream の開始位置解決に使われる。
- テスト: 受信経路は相対 Start と Next Object の拒否、拒否後の request / data state
  非残存、絶対形式・Type 0x00・省略の従来どおりの範囲判定を追加した。送信経路は 3 種の
  相対フィルタでエラーになり Request ID を消費しないことを追加した。PBT では相対
  フィルタが Largest Object の観測有無と形式によらず拒否されること、絶対形式が範囲内で
  受理されること、送信 API の拒否が副作用を残さないことを検証する。

## 完了条件の検証

- 相対開始と Next Object の単独 FETCH が INVALID_RANGE で拒否されること:
  `tests/test_session/fetch/unified.rs` の
  `fetch_with_relative_start_filter_rejected_with_invalid_range` (範囲内の `{10, 0}` に
  解決される StartGroup=1 でも拒否) と
  `fetch_with_next_object_filter_rejected_without_published_objects` (Largest Object
  未観測でも拒否)。REQUEST_ERROR の error_code と理由句は共通ヘルパ
  `assert_relative_filter_rejected` が検証する。PBT は
  `pbt/tests/prop_session/fetch.rs` の
  `fetch_relative_filter_always_rejected_with_invalid_range` が形式
  (RelativeGroup / NextObject) と Largest Object の観測有無の組み合わせで固定する。
  実装は `LocationFilter::is_relative` と `Session::handle_peer_fetch` の早期拒否。
- 送信 API が不正な単独 FETCH を送らないこと:
  `tests/test_session/fetch/unified.rs` の
  `send_fetch_with_relative_location_filter_rejected_without_consuming_request_id` と、
  PBT の `send_fetch_relative_filter_rejected_without_side_effects`。エラーコード、
  SendRequest / CloseSession 非発行、Request ID 非消費を検証する。実装は
  `Session::send_fetch` の request_id 発行前検証。
- 絶対範囲・NoFilter・省略は従来どおり判定されること:
  `tests/test_session/fetch/unified.rs` の
  `fetch_with_non_relative_location_filter_keeps_range_judgement` が 8 ケース (省略 /
  Type 0x00 / Type 0x02 / 0x03 / 0x04 の受理と、Largest 超過・空 range の拒否) を表で
  固定する。PBT の `fetch_non_relative_filter_accepted_within_largest` が Largest Object
  以下の絶対形式を生成して受理を検証する。`tests/test_message_parameter.rs` の
  `is_relative_matches_the_relative_filter_forms` が 6 形式の相対判定と、非相対形式の
  開始位置が Largest Object に依存しないことを固定する。
- fill の相対フィルタが引き続き利用できること:
  `tests/test_session/fetch/fill.rs` の `fill_stream_event_carries_the_resolved_range`
  (内側 Relative Start で開始位置が解決される) と
  `fill_with_next_object_overflow_opens_saturated_range` (内側 Next Object) が相対
  フィルタで fill fetch stream が開くことを検証しており、今回の変更後も通過する。実装は
  `src/session/subscription/fill.rs` の `should_open_fill_stream` に変更を入れず、単独
  FETCH の拒否を流用していない。
- 拒否時に request / data state が残らないこと:
  `fetch_with_relative_start_filter_rejected_with_invalid_range` が `server.fetch()` 不在、
  拒否済み Request ID の bidi close が no-op で吸収されること、拒否した Request ID では
  `send_fetch_header` が SESSION_PROTOCOL_VIOLATION になること、続く絶対範囲の FETCH が
  受理されることを検証する。
- 検証コマンド: `cargo fmt --all -- --check` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` が
  すべて成功する。
