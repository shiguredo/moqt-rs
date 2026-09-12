# publisher_track_largest の配置と INVALID_RANGE テストの重複を整理する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-publisher-track-largest-placement
- Polished: {YYYY-MM-DD}

## 目的

`publisher_track_largest` は FETCH / SUBSCRIBE 処理 / TRACK_STATUS / fill stream の複数モジュールから使う Track 単位の Largest Object 集約ヘルパだが、FILL_PARAMETERS 専用モジュールに置かれている。所在を役割に合わせ、同ヘルパが関与する FETCH の INVALID_RANGE テストに残る同型の重複も解消する。挙動は変えない。

## 現状

- `publisher_track_largest` は `src/session/subscription/fill.rs` の `impl Session` にある。モジュール doc は FILL_PARAMETERS / fill fetch stream (draft-ietf-moq-transport-21 §3.4) 専用と読めるが、実際の利用箇所は `handle_peer_fetch` (`src/session/fetch.rs`)、`send_ok_for_track_status`
  (`src/session/namespace/track_status.rs`)、`handle_peer_subscribe` (`src/session/subscription/recv.rs`)、`maybe_open_fill_stream` (`src/session/subscription/fill.rs`) と広い。
- `tests/test_session/fetch/unified.rs` には `assert_fetch_rejected_with_invalid_range` / `assert_fetch_accepted` のヘルパがあるが、REQUEST_ERROR の error_code が `REQUEST_INVALID_RANGE` であることを検証する同型のインラインブロックが 4 テストに残っている。
  - `fetch_start_exceeds_largest_rejected_with_invalid_range`
  - `fetch_start_exceeds_received_largest_rejected`
  - `fetch_no_published_objects_rejected_with_invalid_range`
  - `fetch_with_empty_filter_range_rejected_with_invalid_range`
- 4 つ目のテストは空 range の `ControlMessage::Fetch` を直接 `recv_request` に渡すため、送信 API を使う `assert_fetch_rejected_with_invalid_range` をそのままは適用できない。

## 設計方針

- `publisher_track_largest` を `src/session/subscription/delivery.rs` へ移す。同ファイルには `effective_largest_object` / `update_largest_object_in_parameters` があり、LARGEST_OBJECT 関連のヘルパが 1 箇所に揃う。`pub(crate)` の公開範囲は変えず、呼び出し側はモジュールパスの変更のみとする。
- doc コメントは「同一 Track の全 publisher 役 subscription の `effective_largest_object` の最大値を返す Track 単位の集約」として FILL 固有の説明から独立させる。FETCH / TRACK_STATUS / SUBSCRIBE / fill の用途列挙は維持する。
- REQUEST_ERROR の error_code を検証する部分を共通ヘルパに切り出し、4 テストのインライン `match` を置き換える。空 range テスト (送信 API を使わない経路) でも使える形にする。
- テストの検証内容と期待値は変えない。

## 完了条件

- `publisher_track_largest` が `src/session/subscription/delivery.rs` に定義され、`src/session/subscription/fill.rs` に FILL 固有でない集約ロジックが残っていないこと
- INVALID_RANGE の応答検証が 1 つのヘルパに集約され、上記 4 テストが同ヘルパを使っていること
- `cargo test --workspace` と PBT が通ること
- 公開 API が変わらないこと
- `CHANGES.md` の `## develop` の `### misc` に `[UPDATE]` エントリが追加されていること
