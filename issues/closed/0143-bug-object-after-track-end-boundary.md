# 終端宣言と同じ位置の Object を Malformed にしない

- Created: 2026-09-22
- Completed: 2026-09-25
- Branch: feature/fix-object-after-track-end-boundary
- Polished: 2026-09-25

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 4/5 は「final Object より **larger than**」な Object だけを
Malformed とする (final Object は End of Group / End of Track を宣言した Object 自身)。しかし終端を宣言した Object 自身を
再受信すると、`Session::object_after_track_end` が「終端後の Object」として Malformed Track にし、該当 subscription を
cancel する。宣言した Object 自身は final Object として存在するものとして扱い、同一位置の重複は条件 6 の内容比較に委ねる。

## 現状

- `Session::object_after_track_end` (`src/session/data.rs`) は `subscription.ended_groups` の値に対して `location.object_id >= end_object_id` で判定する。`ended_groups` には End of Group status を宣言した位置そのものが「存在しない最小の Object ID」として記録される
- そのため End of Group status の Object を同一内容で 2 回受信すると、2 回目は `"object received after End of Group"` で Malformed Track になり、`SessionEvent::RequestTerminated { reason: TerminationReason::MalformedTrack }` と STOP_SENDING / RESET_STREAM が発行される
- datagram 経路は重複が起こりやすく、subgroup 経路でも同一 Object が 2 本の Subgroup stream で届きうる
- draft-ietf-moq-transport-21 §12.1 条件 4 は "An Object is received in a Group whose Object ID is larger than the final Object in the Group. The final Object in a Group is the Object with Status END_OF_GROUP, ..." と定め、終端を宣言した Object 自身を final Object として扱う。条件 5 (End of Track) も同じく
  "larger than" である。条件 6 は「同一 Object を異なる Payload / immutable properties で受信した場合」だけを Malformed とする
- 一方 draft-ietf-moq-transport-21 §11.1.2 (Object Status) は End of Group を "no objects with the specified Group ID and the Object ID that is greater than or equal to the one specified exist"、End of Track を "no objects with the location that is equal to or greater than the one
  specified exist" と定める。この文言だけを読むと宣言位置の Object は存在しないことになり、`>=` が導かれる
- 0123 で重複 Object の内容比較 (`ObjectFieldTracker::observe_object_fields_with_content`) を入れたため、同じ位置の重複が「内容が同じなら正当・異なれば Malformed」という条件 6 の扱いを受けられるようになった
- `ended_groups` の値は「存在しない最小の Object ID」に正規化されている。END_OF_GROUP bit (datagram の bit / subgroup header の bit + FIN) は「bit を立てた Object 自身は存在する」ため `last_object_id + 1` を記録し、Object 0 個の Subgroup は空 Group として 0 を記録する (`src/session/data.rs` の `record_group_end_after` と空 Group の分岐)
- `end_of_track` は OBJECT_STATUS_END_OF_TRACK の Location そのもの (正規化なし) を保持し、`location >= end` で比較する
- `object_after_track_end` は `record_object_status_end` と `ObjectFieldTracker` の観測より前に呼ばれる

## 設計方針

§12.1 条件 4/5 の "larger than the final Object" を採る。終端を宣言した Object 自身は final Object として存在するものとして扱い、**その位置より後ろ** だけを Malformed Track にする。同じ位置の重複は §12.1 条件 6 と §7.1 の内容比較 (`ObjectFieldTracker`) に委ねる。

- 根拠: §12.1 条件 4 は "An Object is received in a Group whose Object ID is larger than the final Object in the Group.
  The final Object in a Group is the Object with Status END_OF_GROUP, or the last Object before a FIN in a Subgroup
  which has the END_OF_GROUP bit set." と定め、終端を宣言した Object を Group の final Object として扱う。条件 5 も
  "whose Group and Object ID are larger than the final Object in the Track. The final Object in a Track is the Object
  with Status END_OF_TRACK" と同じ "larger than" である
- §11.1.2 (Object Status) の "greater than or equal to" は終端宣言後に存在しない Object を述べたものであり、
  宣言した Object 自身の再受信を禁じるものではない。§2.1 も "an endpoint can receive an Object after it has already
  recorded that the Object does not exist ... This is not a protocol error and the Track is not malformed." と定める
- `Subscription::ended_groups` の意味は「存在しない最小の Object ID」のままとし、**終端を宣言した Object 自身は存在するものとして** 正規化する
  - `record_object_status_end` の OBJECT_STATUS_END_OF_GROUP は `object_id.saturating_add(1)` を記録する (現在は `object_id`)。END_OF_GROUP bit 経路 (`record_group_end_after`) は既に `last_object_id.saturating_add(1)` で同じ意味になる
  - Object 0 個の Subgroup (空 Group) は 0 のままでよい (存在する Object が無いため)
  - `object_id = u64::MAX` では境界を 1 つ先へ進められず、宣言した Object 自身の再受信も Malformed のままになる。§8.1 の varint は 64 bit を表現できるため wire 到達可能な極値だが、有効な Object ID 1 点だけの既知の限界として `Subscription::ended_groups` の doc に明記する
- `Subscription::end_of_track` は OBJECT_STATUS_END_OF_TRACK の Location をそのまま保持し (正規化しない)、比較を厳密な `location > *end` にする。`Location` は group_id → object_id の辞書順 (`derive(Ord)`) なので、これが条件 5 の "Group and Object ID are larger than" と一致し、桁溢れも起きない
  - 同一 group なら `object_id > end.object_id`、group が進めば無条件に Malformed になる
- 同じ位置に「重複ではない Object」が先に到着してから終端宣言が届く順序、および終端宣言の後に同じ位置へ別の Object が届く順序は、いずれも `ObjectFieldTracker` の同一 (group_id, object_id) 比較で `payload_key` (status の有無を含む) の違いとして検出され Malformed になる
  - **限界**: tracker は group の前進で過去 group の記録を破棄し (0123 の保持量対策)、記録数の上限 1,000 件でも破棄する。記録が消えた後の同一位置の重複は比較されず受理される (従来の `>=` 判定より検出範囲が狭くなる)。§2.1 に照らして仕様違反ではないが、known limitation として doc と `CHANGES.md` に明記する
  - 完了条件のテストは「記録が残っている間」の挙動を固定する
- `object_after_track_end` は `record_object_status_end` と tracker の観測より前に呼ばれる。順序は変えず、終端判定を通過した同一位置の Object が tracker で比較されることをテストで確認する (`record_largest_received_location` が判定前に走ることも現状のまま)
- §12.1 条件 5 の FETCH 側 (FETCH 応答内の Object) は対象外とする。`recv_fetch_entry` は位置検証を行わず、`Fetch::end_of_track` は位置を保持しない bool のため、必要になった時点で別 issue とする
- Malformed Track の通知 (該当 subscription のみの cancel と `TerminationReason::MalformedTrack`) は従来どおり
- 0123 の `ObjectFieldTracker` は同一内容の重複を受理する (§12.1 条件 6 の "different Payload or other immutable properties" のみを検出する) ため、本 issue の変更と矛盾しない

## 完了条件

- 同一内容の End of Group status Object を 2 回受信しても `SubscriptionState::Established` のままで、
  `RequestTerminated { reason: MalformedTrack }` / STOP_SENDING / RESET_STREAM が発行されないことを固定する
  テストが `tests/test_session/data_stream.rs` の End of Group / End of Track の意味論の節に追加されていること
  (セッション自体は Malformed Track で閉じないため `SessionState` ではなく subscription の状態とイベントで判定する)
- 同じ位置で内容が異なる重複が Malformed Track になることを、wire で到達可能な組で固定するテストが追加されていること
  (例: 同位置の data Object の後に End of Group status Object、status 0x0 の後に status 0x3、Subgroup ID の異なる重複)
- 終端を宣言した位置より後ろの Object (`object_id > 宣言位置`) は従来どおり Malformed Track になることを固定するテストが追加されていること (非退行)
- 終端宣言と同じ位置に「重複ではない Object」が先に到着してから終端宣言が届く順序と、終端宣言の後に同じ位置へ別の Object が届く順序の両方で、tracker の記録が残っている間に Malformed Track になることを固定するテストが追加されていること
- End of Track status (OBJECT_STATUS_END_OF_TRACK) でも同じ扱いになることを固定するテストが追加されていること (同一内容の重複は受理、宣言位置より後ろは Malformed)
- END_OF_GROUP bit (datagram の bit / subgroup header の bit + FIN) と Object 0 個の Subgroup の挙動が変わらないことを固定するテストが追加されていること (非退行)
- `tests/test_session/data_stream.rs` の既存テストを新しい境界に更新すること (`ended_groups` を `Some(&5)` から `Some(&6)` に、`object_id: 7` を `8` に、同一位置の重複の期待を「終端後」から条件 6 の比較に変える等)。`tests/test_session/subscription/terminated_discard.rs` の条件 4/5 を述べるコメントも実態 (条件 6 経由) に合わせること
- `Subscription::ended_groups` / `Subscription::end_of_track` / `record_object_status_end` / `object_after_track_end` の doc が、正規化の意味 (終端を宣言した Object 自身は存在する)、`u64::MAX` の限界、tracker の保持窓による検出限界、§12.1 条件 4/5 と §2.1 の根拠を書いていること
- `CHANGES.md` の `## develop` に `[FIX]` を追加すること。`Subscription::end_of_track` が保持する値の意味 (宣言 Location のまま) と `ended_groups` の正規化が変わることを本文に書く (型は変わらないため `[FIX]` のまま)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 4/5 の "larger than the final Object" を採り、終端を宣言した Object 自身は存在するものとして扱うようにした。

- `src/session/data.rs`
  - `object_after_track_end` の End of Track 判定を `location >= end` から `location > *end` の厳密比較にした (`Location` は group_id → object_id の辞書順なので、これが条件 5 の "Group and Object ID are larger than" と一致し、桁溢れも起きない)
  - `record_object_status_end` の OBJECT_STATUS_END_OF_GROUP を `object_id.saturating_add(1)` に正規化した (`ended_groups` は「存在しない最小の Object ID」のまま、終端を宣言した Object 自身は存在するものとして扱う)。END_OF_GROUP bit 経路 (`record_group_end_after`) と Object 0 個の Subgroup (空 Group) は従来どおり
  - 2 つの判定の doc/コメントに、条件 4/5 の引用、§11.1.2 の "greater than or equal to" の解釈、§2.1 ("This is not a protocol error and the Track is not malformed.")、FETCH 側が対象外であることを書いた
- `src/session/types.rs`
  - `Subscription::ended_groups` / `Subscription::end_of_track` の doc を新しい正規化と比較規則に書き換え、Object ID が `u64::MAX` のときは境界を進められない既知の限界 (空 Subgroup は対象外) を明記した
- テスト
  - `tests/test_session/data_stream.rs`: `object_after_end_of_group_status_is_rejected` を新しい境界 (`Some(&6)`、object_id 6 の拒否) に更新し、End of Group / End of Track の宣言位置より後ろの拒否を条件 4/5 の "larger than" を根拠に書き換えた
  - 追加: 宣言位置の同一内容重複の受理 (End of Group / End of Track)、宣言位置の内容が異なる重複の拒否 (別 Subgroup の Subgroup ID 差異)、status と data Object が同一位置で交差する両順序の拒否
  - `tests/test_session/subscription/terminated_discard.rs`: 終端宣言位置の重複が条件 6 の内容比較で検出されることをコメントに反映
- `CHANGES.md` の `## develop` に `[FIX]` を追加した (公開フィールドの型は変わらないため `[FIX]` のまま、`end_of_track` の値の意味と `ended_groups` の正規化が変わることを本文に記載)

## 限界

- 記録が保持量の上限や group 前進の prune で破棄された後は、同一位置の重複を比較できず受理される (検出範囲が従来より狭くなる known limitation)
- FETCH 応答内の Object (条件 5 の後半) は位置検証の対象外のままである
