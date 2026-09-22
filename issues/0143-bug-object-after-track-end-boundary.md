# 終端宣言と同じ位置の Object を Malformed にしない

- Created: 2026-09-22
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-after-track-end-boundary
- Polished: {YYYY-MM-DD}

## 目的

同一内容の重複 Object は draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6 が許容している。しかし終端を宣言した Object 自身を再受信すると、`Session::object_after_track_end` が「終端後の Object」として Malformed Track にし、該当 subscription を cancel する。同一内容の重複で subscription を落とさないようにする。

## 現状

- `Session::object_after_track_end` (`src/session/data.rs`) は `subscription.ended_groups` の値に対して `location.object_id >= end_object_id` で判定する。`ended_groups` には End of Group status を宣言した位置そのものが「存在しない最小の Object ID」として記録される
- そのため End of Group status の Object を同一内容で 2 回受信すると、2 回目は `"object received after End of Group"` で Malformed Track になり、`SessionEvent::RequestTerminated { reason: TerminationReason::MalformedTrack }` と STOP_SENDING / RESET_STREAM が発行される
- datagram 経路は重複が起こりやすく、subgroup 経路でも同一 Object が 2 本の Subgroup stream で届きうる
- draft-ietf-moq-transport-21 §12.1 条件 4 は "An Object is received in a Group whose Object ID is larger than the final Object in the Group. The final Object in a Group is the Object with Status END_OF_GROUP, ..." と定め、終端を宣言した Object 自身を final Object として扱う。条件 5 (End of Track) も同じく
  "larger than" である。条件 6 は「同一 Object を異なる Payload / immutable properties で受信した場合」だけを Malformed とする
- 一方 draft-ietf-moq-transport-21 §11.1.2 (Object Status) は End of Group を "no objects with the specified Group ID and the Object ID that is greater than or equal to the one specified exist"、End of Track を "no objects with the location that is equal to or greater than the one
  specified exist" と定める。この文言だけを読むと宣言位置の Object は存在しないことになり、`>=` が導かれる
- 0123 で重複 Object の内容比較 (`ObjectFieldTracker::observe_object_fields_with_content`) を入れたため、同じ位置の重複が「内容が同じなら正当・異なれば Malformed」という条件 6 の扱いを受けられるようになった

## 設計方針

- §11.1.2 の宣言文言と §12.1 条件 4/5 の "larger than" のどちらを採るかを決め、実装と doc を揃える
- 条件 4/5 に合わせる場合、`object_after_track_end` の判定を「宣言位置より大きい」に変える。同じ位置の重複は条件 6 の比較 (subgroup / datagram 経路) に委ね、内容が異なる重複は引き続き Malformed にする
- 同じ位置に「重複ではない Object」が先に到着してから終端宣言が届く順序も考慮する。この場合は条件 6 の比較対象が無いため、検出できるかどうかを整理する
- §11.1.2 の文言を優先して `>=` を維持する場合は、その理由 (宣言位置の Object は存在しないという解釈) を `object_after_track_end` の doc に明記し、同一内容の重複を Malformed にする挙動をテストで固定する

## 完了条件

- 同一内容の End of Group status Object を 2 回受信しても subscription が cancel されないこと (または `>=` を維持する場合はその挙動と根拠が doc とテストで固定されていること)
- 終端宣言より後ろの Object は従来どおり Malformed Track になること (非退行)
- End of Track status でも同じ扱いになっていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

未着手。
