# FETCH_OK を REQUEST_OK に統合して応答範囲を導出する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.2、§3.2.2、§9.3、§9.11、§9.19.17](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` は Type `0x18` の `FetchOk` に `end_of_track` / `end_location` を持たせる。
`src/session/fetch.rs` の `send_fetch_ok` / `handle_peer_fetch_ok` と `SessionEvent::FetchOkReceived`、`crates/tokio-moq/src/moqt_client.rs` / `crates/moq-sub/src/pipeline.rs` もこの専用応答を前提にしている。

## 設計方針

FETCH 初回成功を Type `0x07` の REQUEST_OK として扱い、FETCH の REQUEST_UPDATE_OK とは状態で区別する。
EXPIRES / LARGEST_OBJECT と Track Properties を処理し、旧 End Location / End Of Track のフィールドを廃止する。
LARGEST_OBJECT が要求範囲内なら応答に必須とし、応答の終点をそこへ切り詰める。
それ以外は要求 End Location を用いる。
要求省略時の範囲も含め、FETCH_OK の送信時点で終点を判断できるようにする。
Track 終端はデータ側の新しい指示で受け取る。

## 完了条件

- 初回 FETCH_OK と FETCH の REQUEST_UPDATE_OK を正しく dispatch し、Track Properties を初回応答だけで受理すること。
- LARGEST_OBJECT の範囲内・範囲外・省略と、応答前のデータ到着・空ストリーム・降順の終端範囲を検証すること。
- 旧 Type `0x18` を現行 FETCH_OK として受理せず、利用側イベントと送信 API が新しい範囲モデルに一致すること。

## 依存・関連

- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)

## 解決方法

`src/message.rs` から Type `0x18` の `FetchOk` を削除し、FETCH の初回成功応答を Type `0x07` の `RequestOk` に統合した。
draft-ietf-moq-transport-23 §9 Table 5 は `0x18` を "RESERVED (FETCH_OK in <= 22)" と定めるため、`0x18` は未知のメッセージ型として `InvalidMessageType` で拒否し、
セッションは §9 の MUST に従って UNEXPECTED_MESSAGE で閉じる。`FETCH_OK_ALLOWED_PARAMS` は §9.3 の "FETCH_OK: EXPIRES, LARGEST_OBJECT" に合わせて 2 型へ広げた。

`src/message_parameter.rs` に `LocationFilter::explicit_end_location` を追加し、要求が明示した End (Type 0x03 / 0x04) と
「省略時は Largest Object になる End」を区別できるようにした。`MessageParameters::remove_largest_object` は、応答範囲と矛盾する
LARGEST_OBJECT を取り除くために追加した。

`src/session/types.rs` の `Fetch` から `end_of_track` を削除し、`request_end` を「要求が明示した終端 (省略時は `None`)」に変えた。
`end_location` は応答範囲の終端であり、FETCH_OK の送受信時に確定する。`SessionEvent::FetchOkReceived` からも `end_of_track` を削除した。
Track の終端は FETCH 応答ストリームの End of Group (0x80) / End of Track (0x81) で受け取る (§11.4.2.2 (Prior Object Fields))。

`src/session/fetch.rs` の `send_fetch_ok` は `end_location` / `end_of_track` 引数を廃止し、`(request_id, parameters, track_properties)` にした。
応答範囲の終端は呼び出し時点で決める。Largest Object (利用者指定値と Session の観測値の大きい方) が要求範囲内ならそこへ切り詰めて
LARGEST_OBJECT を必ず載せ、範囲外なら要求の End Location を使って LARGEST_OBJECT を取り除く (§3.2 (Fetch) / §3.2.2 (Gaps in a Fetch Stream))。
要求が End を省略している場合の End は Largest Object であり、Largest Object も不明なら応答範囲を確定できないためエラーにする。
受信側は `handle_ok_for_fetch_initial` が初回応答を、`handle_ok_for_fetch_update` が REQUEST_UPDATE_OK を扱う。

`src/session/core.rs` の `handle_peer_request_ok` は、同じ REQUEST_OK で届く初回 FETCH_OK と FETCH の REQUEST_UPDATE_OK を
`Fetch::response_received` で dispatch する。Track Properties は TRACK_STATUS_OK と初回 FETCH_OK だけで受理し、
REQUEST_UPDATE_OK で非空なら INVALID_FIELD で閉じる (§9.3 (REQUEST_OK))。パラメータスコープも初回は `FETCH_OK_ALLOWED_PARAMS`、
2 回目以降は `REQUEST_UPDATE_OK_ALLOWED_PARAMS` で検証する。

`crates/tokio-moq/src/moqt_client.rs` / `peer_harness.rs` の `send_fetch_ok` も新しい引数に合わせ、`FetchOkReceived` の `end_of_track` 参照を外した。
`crates/moq-sub/src/pipeline.rs` は元々 `end_of_track` を読んでおらず、応答範囲の終端だけをログに残す。
`crates/moq-pub/src/pipeline.rs` は Session を経由せず Object を書くため、自前のキャッシュが把握している Largest Object を
要求範囲内のときだけ LARGEST_OBJECT として渡す。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 初回 FETCH_OK と FETCH の REQUEST_UPDATE_OK の dispatch | `fetch::unified::second_request_ok_is_dispatched_as_request_update_ok` が 1 回目を `FetchOkReceived`、2 回目を `RequestOkReceived { request_kind: Fetch }` として通知することを検証する |
| Track Properties を初回応答だけで受理 | `fetch::unified::track_properties_are_accepted_only_in_the_initial_fetch_ok` が初回の受理と REQUEST_UPDATE_OK の INVALID_FIELD を、`fetch::after_fin::initial_request_ok_in_pending_is_accepted_as_fetch_ok` が `Pending` での受理を検証する |
| LARGEST_OBJECT が要求範囲内 | `fetch::unified::fetch_ok_largest_object_within_range_truncates_end` と `prop_session::fetch::fetch_ok_end_follows_largest_object_within_range` が、送受信の双方と任意の範囲で終端が Largest Object になることを検証する |
| LARGEST_OBJECT が要求範囲外 | `fetch::unified::fetch_ok_largest_object_outside_range_uses_requested_end` が終端を要求の End Location にし、LARGEST_OBJECT を応答に載せないことを検証する |
| LARGEST_OBJECT の省略 | `fetch::unified::fetch_ok_without_largest_object_uses_requested_end` が終端を要求の End Location にすることを検証する |
| End 省略時の LARGEST_OBJECT | `fetch::unified::fetch_ok_largest_object_resolves_open_ended_request` が終端を Largest Object にすることを、`fetch::unified::fetch_ok_without_largest_object_for_open_ended_request_closes_session` が Largest Object 不明時の拒否を検証する |
| 応答前のデータ到着 | `fetch::unified::fetch_ok_uses_objects_published_before_the_response` が応答前の publish から FETCH_OK 送信時点で終端を決めることを検証する |
| 応答前のデータ到着 (FIN 後) | `fetch::after_fin::initial_request_ok_after_fin_is_accepted_as_fetch_ok` が FIN 後の初回応答の受理と終端解決を検証する |
| 空ストリーム | `fetch::unified::empty_fetch_response_still_resolves_the_end` が FETCH_HEADER だけの stream でも応答範囲の終端が確定することを検証する |
| 降順の終端範囲 | `fetch::unified::descending_fetch_truncates_the_end_to_largest_object` が GROUP_ORDER = Descending でも LARGEST_OBJECT が終端になることを検証する |
| 旧 Type `0x18` を受理しない | `test_message` の `fetch::legacy_fetch_ok_message_type_is_rejected` が `InvalidMessageType(0x18)` を検証する |
| Start より小さい LARGEST_OBJECT | `fetch::unified::fetch_ok_end_before_start_closes_session` が INVALID_FIELD を検証する |
| 利用側イベント | `SessionEvent::FetchOkReceived` から `end_of_track` を削除し、`end_location` だけにした |
| 送信 API | `send_fetch_ok` を `(request_id, parameters, track_properties)` に変更し、moq-sub / moq-pub / peer_harness を追随させた (`rg end_of_track crates/moq-sub/src crates/moq-pub/src` が 0 件) |

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、
`RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` を実行し、すべて成功した。
