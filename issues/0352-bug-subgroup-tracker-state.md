# SubgroupTracker の状態不変条件を守る

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subgroup-tracker-state
- Polished: 2026-10-21

## 目的

`SubgroupTracker` は Subgroup ストリームの状態を追跡し、FIN 後の再オープンを禁止する。現状は RESET / STOP_SENDING の記録が正常終了 (FIN) の記録を上書きするため、公開 API 経由で FIN 後の Subgroup を再オープンできてしまう。`mark_fin` の doc が「未知キーは無視」とする一方で内部に記録する点も実装と doc が食い違う。

## 現状

- `src/subgroup_tracker.rs` の `mark_reset` / `mark_stop_sending` は既存の `ClosedFin` を上書きできる。`can_reopen` は `ClosedFin` に対して false を返すが、上書き後は `Reset` / `StoppedByPeer` になり true を返すため、「FIN 後は再オープン不可」という契約に反する。
- Session の wire 終端経路では、FIN 受信後の RESET_STREAM は `Session::recv_data_stream_closed` の `fin_closed` 判定で吸収され、
  FIN 済みの outgoing stream への STOP_SENDING は `Session::recv_data_stream_stop_sending` で unknown outgoing stream として拒否される。
  したがって Session の wire 終端経路では `mark_reset` / `mark_stop_sending` が `ClosedFin` を上書きする状態には到達しない。
  ただし `mark_fin` 条件 3 失敗後の破棄分岐は別途 recovery が必要である (設計方針参照)。
  公開 `SubgroupTracker` API の利用者や将来の経路では到達し得る。
- `mark_fin` は doc で「未知キーに対しては無視」としながら、`entries` にキーが無くても `fin_last_object_ids` に挿入する。これは draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) 条件 2 の検出に必要な記録であり、実装の意図は「状態エントリは作らないが、最終 Object ID は記録する」である。doc と `tests/test_subgroup_tracker.rs` の `mark_without_open_is_noop` のコメントがこの意図と一致していない。
- `ClosedFin` を持つキーでは、`ClosedFin.last_object_id` と `fin_last_object_ids` が同じ値を二重に保持している。`fin_last_object_ids` は未知キーの FIN も記録するため、`ClosedFin` より広い範囲を保持する。
- 影響: FIN 済み Subgroup の再オープンを許し、同じ Object ID の重複受信や状態不整合を招く。たとえば `open(key)` → `mark_fin(key, Some(5))` → `mark_reset(key, None)` → `open(key)` の順で呼ぶと、最後の `open(key)` が成功する。`check_object_after_fin` は `object_id > 5` のみを Malformed とするため、Object ID 5 以下の Object が再度受理され得る。
- 根拠: `refs/moq/draft-ietf-moq-transport-22.txt` §2.2 (Subgroups) は
  「Objects from the same Subgroup MUST NOT be sent on different streams, unless one of the streams was reset prematurely」とし、
  §11.3.2 (Closing Subgroup Streams) は
  「A sender might send all objects in a Subgroup and the FIN on a QUIC stream, and then reset the stream. ...
  If the application receives all data on the stream and the FIN, it can ignore any subsequent reset.」とする。
  FIN を受けた後に RESET が届いても、正常終了の事実を失ってはならない。

## 設計方針

- 終端理由は FIN / RESET_STREAM / STOP_SENDING の 3 種があり得る。公開 `SubgroupStreamState` は単一の代表状態を返すため、FIN を最優先の終端理由として扱う。`ClosedFin` を記録済みのキーでは `mark_reset` / `mark_stop_sending` を no-op にし、状態を `ClosedFin` のまま維持する。`can_reopen` は `ClosedFin` に対して false のままとする。
- `Reset` と `StoppedByPeer` の相互上書きは従来どおり後勝ちでよい。どちらも `can_reopen` は true で、STOP_SENDING 後の Forward State 0→1 条件は `Session` が `stopped_outgoing_subgroups` で別途管理している。
- `mark_fin` が条件 3 (最終 Object ID 不一致) で `Err` を返した場合は状態も `fin_last_object_ids` も更新しない。このとき `entries` は直前の状態のまま (`Open` / `ClosedFin` / `Reset` / `StoppedByPeer` のいずれか、またはエントリなし) である。
- `Session::recv_data_stream_closed` の破棄分岐は、`mark_fin` が `Err` を返した場合に `mark_reset` で `Reset` にして再オープンを許す既存挙動
  (`discarded_fin_marks_tracker_terminal_and_allows_reopen`) を持つ。
  この遷移は wire の RESET_STREAM ではないため、`mark_reset` を `ClosedFin` に対して no-op にすると既存テストが壊れる。
  そこで `pub(crate)` の recovery メソッド (`mark_reset_after_fin_failure` (仮称)) を追加し、
  指定キーが `entries` にある場合に `Reset { reliable_size: None }` にする (無い場合は no-op)。
  `fin_last_object_ids` は変更しない。破棄分岐は `mark_fin` 失敗時にこのメソッドを呼ぶ。
  非破棄分岐の `mark_fin` 失敗は従来どおり `terminate_malformed_track` で subscription を終端し、recovery メソッドは呼ばない。
  doc には wire の RESET_STREAM ではなく、`mark_fin` 条件 3 失敗後の破棄分岐専用であることを書く。
- `mark_fin` の未知キー扱いを「`entries` に状態エントリを作らないが、`fin_last_object_ids` には最終 Object ID を記録する」と確定する。過去の FIN 記録と `last_object_id` が不一致なら、キーが `entries` に無くても `Err` を返す。doc と `mark_without_open_is_noop` のコメントをこの挙動に合わせる。
- `mark_reset` / `mark_stop_sending` の doc に、`ClosedFin` の場合は状態を変更しないことを書く。
- `ClosedFin` の `last_object_id` を削除してフィールド無しの variant にし、最終 Object ID は `fin_last_object_ids` に一本化する。`SubgroupStreamState::ClosedFin` と `fin_last_object_ids` の doc も更新する。`SubgroupStreamState::ClosedFin` は公開 variant のため、この削除は公開 API の破壊的変更である。`CODEBASE.md` の「破壊的変更は積極的に行う」に従い許容する。
- `tests/test_subgroup_tracker.rs` に FIN→RESET / FIN→STOP_SENDING の回帰テストと、未知キーの `mark_fin` のテストを追加する。
  `pbt/tests/prop_subgroup_tracker.rs` の `Model::ClosedFin` (`Op::Fin` の更新を含む) と `Op::Reset` / `Op::Stop` の上書き規則を新しい規則に合わせる。
  `tests/test_subgroup_tracker.rs` の既存 `ClosedFin { last_object_id: ... }` 構築箇所も更新する。

## 完了条件

- FIN 後に RESET を受けても `get` が `ClosedFin` を返し、`open` が `MessageError::ProtocolViolation` を返すこと。
- FIN 後に STOP_SENDING を受けても `get` が `ClosedFin` を返し、`open` が `MessageError::ProtocolViolation` を返すこと。
- `mark_reset` は `ClosedFin` を上書きせず、recovery メソッドは `ClosedFin` を `Reset` にできること。
- `mark_fin` の doc と実装が一致していること。未知キーでは状態エントリを作らず、`fin_last_object_ids` には記録し、過去の FIN と不一致なら `Err` を返して何も更新しないこと。
- `mark_without_open_is_noop` のコメントが、状態エントリを作らないが `fin_last_object_ids` には記録する挙動と一致していること。
- `mark_reset` / `mark_stop_sending` の doc に、`ClosedFin` の場合は状態を変更しないことが書かれていること。
- 未知キーで `mark_fin(key, Some(5))` を呼んでも `get(key)` は `None` のままで、`check_object_after_fin(key, 6)` が Malformed の理由を返し、`open(key)` は成功すること。
- `ClosedFin` が `last_object_id` を持たず、最終 Object ID が `fin_last_object_ids` に一本化されていること。
- `SubgroupStreamState::ClosedFin` と `fin_last_object_ids` の doc が、最終 Object ID を `fin_last_object_ids` に一本化した実装と一致していること。
- `tests/test_subgroup_tracker.rs` に上記の状態遷移テストと未知キーのテストが追加され、`pbt/tests/prop_subgroup_tracker.rs` のモデルが新しい規則に追従していること。
- 既存の `discarded_fin_marks_tracker_terminal_and_allows_reopen` が通り、破棄分岐が recovery メソッドで `Reset` にすること (`mark_fin` 失敗後の `ClosedFin` → `Reset` を含む)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
