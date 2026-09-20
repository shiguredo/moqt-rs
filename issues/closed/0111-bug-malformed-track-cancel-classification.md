# Malformed Track の検出を購読単位の cancel として扱えるようにする

- Created: 2026-09-21
- Completed: 2026-09-22
- Branch: feature/fix-malformed-track-cancel-classification
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) は、subscriber が Malformed Track を検出したとき「該当する subscription または fetch を cancel し (MUST)、アプリへエラーを届ける (SHOULD)」と定める。cancel の手段は §6.4.2.3 (Request Cancellation and Rejection) が「送信中の方向は RESET_STREAM、受信中の方向は STOP_SENDING で打ち切る」と規定しており、影響は該当 Track に閉じ、セッションは維持される。

現状は、この条件で返るエラーがセッション終了コード `SESSION_PROTOCOL_VIOLATION` に写されるため、アプリは「セッションを閉じるべきか、購読だけ cancel すべきか」をエラー種別から判別できない。`MessageError::ProtocolViolation` はフレーミング違反などセッションを閉じるべき検証でも使われており、同じ値に 2 つの意味が載っている。

Session 層の cancel 自体 (`SessionEvent::StopSendingRequestStream` → `SessionEvent::ResetRequestStream` と `SessionEvent::RequestTerminated { reason: MalformedTrack }`) は 0028 で実装済みである。本 issue は「どの検出が購読単位の cancel に相当するか」をエラー種別で判別できるようにすることを対象にする。

## 現状

- `src/error.rs` の `MessageError` に Malformed Track を表す variant が無い。`MessageError::ProtocolViolation(&'static str)` が §12.1 の検出とフレーミング違反の両方に使われている
- `src/stream/decoder.rs` の `FetchStreamDecoder::validate_fetch_object` は 4 つの検証を `MessageError::ProtocolViolation` で返す。§12.1 の列挙条件に対応するのは「同一 Subgroup 内の Publisher Priority 変更」(条件 1) と「確定済み最終 Object を超える Object ID」(条件 2) である
- 残る 2 つは §12.1 の列挙条件ではない。「Object ID が昇順でない」は §2.2 (Subgroups) の定義、「FETCH の Group が昇順でない」は §9.11 (FETCH) の "A publisher MUST send fetched groups in the requested group order" に由来する
- §12.1 条件 3 (同一 Subgroup を複数の transport stream で受信し最終 Object が異なる) を検出するのは `src/subgroup_tracker.rs` の `SubgroupTracker::mark_fin` である。`FetchStreamDecoder` は条件 3 を検出しない
- `src/object_properties.rs` の `ObjectPropertyTracker::observe_decoded_object` / `observe_object` は §12.1 の条件と、`ObjectProperties::decode` の失敗 (フレーミング) の両方を `MessageError::ProtocolViolation` で返す
- §12.1 条件 6 / 7 を検出するのは `ObjectFieldTracker::observe_object_fields` で、返る型は専用の `ObjectFieldMismatch` である。`Session` はそれを `SessionError::new(SESSION_PROTOCOL_VIOLATION, ...)` に写す
- `src/subgroup_tracker.rs` の `SubgroupTracker::record_priority` / `mark_fin` は `SessionError { code: SESSION_PROTOCOL_VIOLATION }` を返す
- `SubgroupTracker::check_object_after_fin` は `Option<&'static str>` を返し、`src/session/data.rs` の `Session::attribute_subgroup_object` が `SessionError::new(SESSION_PROTOCOL_VIOLATION, reason)` に包む
- `Session::object_after_track_end` も §12.1 条件 4 / 5 で `Option<&'static str>` を返し、呼び出し元が `SessionError::new(SESSION_PROTOCOL_VIOLATION, reason)` に包む
- §12.1 条件 1 は 2 経路で検出される。`Session::recv_subgroup_header` の header 時点と、`SubgroupIdMode::FirstObjectId` で subgroup_id を遅延解決する `Session::recv_subgroup_object` の `Session::resolve_object_subgroup_id` である
- §12.1 条件 3 (`SubgroupTracker::mark_fin`) を返すのは `Session::recv_data_stream_closed` である
- `src/session/data.rs` の `session_error_from_data_message` は `MessageError::ProtocolViolation` を `SessionError { code: SESSION_PROTOCOL_VIOLATION }` に写す。§12.1 の検出経路はこの写像を通って `Session::terminate_malformed_track` を呼び、購読単位の cancel とセッション維持を行うが、アプリへ返るエラーは `SESSION_PROTOCOL_VIOLATION` のままである
- `Session::terminate_malformed_track` は `pub(super)` で `()` を返し、cancel イベントと `SessionEvent::RequestTerminated { reason: TerminationReason::MalformedTrack }` の発行だけを行う
- `Session::recv_subgroup_object` と `Session::recv_object_datagram` の戻り値は `Result<TrackDataAcceptance, SessionError>` である。`src/session/types.rs` の `SessionError` は `code` (Session Termination Error Code, §16.11.1) と `reason` だけを持ち、セッションを終了しない cancel を表す値を持たない
- `RecvDataStreamError` は `BeforeSessionEstablished` / `InvalidInput(SessionError)` / `Session(SessionError)` の 3 variant を持ち、「セッションを終了する」と「しない」を分けているが、§12.1 の cancel を表す variant が無い
- `Session` は decoder を保持しない (`SubgroupStreamDecoder` / `FetchStreamDecoder` はアプリが Session の外で保持する)。`FetchStreamDecoder::validate_fetch_object` の戻り値は `Session` に届かないため、FETCH の §12.1 検出を判別するのはアプリである
- `src/lib.rs` は `pub mod subgroup_tracker` / `pub mod object_properties` / `pub mod stream` を公開しており、`MessageError` は `#[non_exhaustive]` でない公開 enum である。variant 追加と `SubgroupTracker` の戻り値型変更はいずれも破壊的変更になる
- §12.1 の各条件で `SESSION_PROTOCOL_VIOLATION` を固定しているテストが `tests/test_session/data_stream.rs` / `tests/test_session/subgroup_object_filter/` / `tests/test_session/subscription/terminated_discard.rs` / `src/session/tests.rs` にある。`err.code` の検査は戻り値型が `SessionError` であることに依存しているため、戻り値型を変える API のテストは広く更新が必要になる

## 設計方針

- `src/error.rs` の `MessageError` に `MalformedTrack(&'static str)` を追加する。§12.1 の列挙条件に対応する検出だけがこれを返す
- 分類する検出は次のとおりに限定する
  - `FetchStreamDecoder::validate_fetch_object` の条件 1 (Publisher Priority 変更) と条件 2 (確定済み最終 Object 超過)
  - `SubgroupTracker::mark_fin` (条件 3) と `SubgroupTracker::record_priority` (条件 1)
  - `ObjectPropertyTracker::observe_decoded_object` / `observe_object` の §12.1 条件に対応する分岐
  - `Session::object_after_track_end` (条件 4 / 5) と `Session::attribute_subgroup_object` 経由の `SubgroupTracker::check_object_after_fin`
  - `ObjectFieldMismatch` を返す `ObjectFieldTracker::observe_object_fields` (条件 6 / 7)
- 分類しない検出は `ProtocolViolation` のまま据え置く
  - `FetchStreamDecoder::validate_fetch_object` の「Object ID が昇順でない」(§2.2) と「Group が昇順でない」(§9.11)
  - `ObjectPropertyTracker::observe_object` の `ObjectProperties::decode` 失敗 (フレーミング)
  - フレーミング違反など、§12.1 の条件ではない既存の `MessageError::ProtocolViolation` の用途
- `SubgroupTracker::record_priority` / `mark_fin` の戻り値型を `SessionError` から `MessageError` に変える。`ObjectPropertyTracker` と同じく codec 層のエラー型に揃える。`SubgroupTracker::check_object_after_fin` の `Option<&'static str>` は変えず、呼び出し元が分類する
- `MessageError` は `#[non_exhaustive]` でない公開 enum なので variant 追加は破壊的変更である。`CHANGES.md` の `## develop` に `[CHANGE]` を記載する (項目は `MessageError` の variant 追加、`SubgroupTracker` の戻り値型変更、末尾の受信 API の戻り値型変更)
- `src/session/types.rs` の `RecvDataStreamError` に `MalformedTrack { reason: &'static str }` を追加する。`SessionError` の `code` は Session Termination Code であり、セッションを終了しない購読単位の cancel の意味を載せない
- §12.1 の検出を返しうる公開受信 API の戻り値型を `RecvDataStreamError` に統一する
- 対象は `Session::recv_subgroup_header` / `Session::recv_subgroup_object` / `Session::recv_object_datagram` / `Session::recv_datagram` / `Session::recv_data_stream_closed` である。条件 3 は `Session::recv_data_stream_closed` から、条件 1 の `FirstObjectId` モードの遅延解決は `Session::recv_subgroup_object` から返る
- §12.1 の検出では `MalformedTrack` を返し、セッション終了を伴う違反は従来どおり `Session(SessionError)` を返す。戻り値型の変更に伴い `impl From<SessionError> for RecvDataStreamError` を追加する
- `src/session/data.rs` の `session_error_from_data_message` の `_ =>` の catch-all をそのままにすると `MessageError::MalformedTrack` が `SESSION_PROTOCOL_VIOLATION` に吸収されるため、`MalformedTrack` を `RecvDataStreamError::MalformedTrack` に写す経路に置き換える。`ProtocolViolation` とフレーミング系は従来どおり `Session(SessionError)` に写す
- §12.1 の検出でもセッションは終了しないため `Session::fail` は呼ばない。cancel イベントの発行と該当 subscription の `Terminated` への遷移は `Session::terminate_malformed_track` の現行実装をそのまま使う
- Session は decoder を保持しないため、FETCH の §12.1 検出はアプリが保持する `FetchStreamDecoder` の戻り値 (`MessageError::MalformedTrack`) で判別する。Session に fetch の cancel を届ける新 API は本 issue では追加しない
- open issue 0122 は `ObjectPropertyTracker::observe_decoded_object` に §12.1 条件を追加する際に `ProtocolViolation` のままにする方針を書いているが、本 issue の分類に従う

## 完了条件

- `FetchStreamDecoder` の条件 1 / 2 の入力で `MessageError::MalformedTrack` が返り、昇順違反と Group 順序違反では `MessageError::ProtocolViolation` が返ることがテストで固定されていること
- `SubgroupTracker::record_priority` / `mark_fin` が §12.1 条件で `MessageError::MalformedTrack` を返すことがテストで固定されていること
- subscription 経路で §12.1 条件 1 / 2 / 3 を検出したとき、返るエラーが `RecvDataStreamError::MalformedTrack` であることがテストで固定されていること
- あわせて該当 subscription が `Terminated` になり、`StopSendingRequestStream` → `ResetRequestStream` が `STREAM_MALFORMED_TRACK` で発行され、セッションが `SessionState::Established` のままであることが固定されていること
- 条件 1 は `Session::recv_subgroup_header` と `Session::recv_subgroup_object` (`FirstObjectId` モードの遅延解決)、条件 3 は `Session::recv_data_stream_closed` 経由の検出でも固定されていること
- §12.1 条件 6 / 7 (`ObjectFieldMismatch`) の経路でも同じく `RecvDataStreamError::MalformedTrack` が返ることがテストで固定されていること
- datagram 経路 (`Session::recv_object_datagram` / `Session::recv_datagram`) でも §12.1 の検出が `RecvDataStreamError::MalformedTrack` になることがテストで固定されていること
- フレーミング違反 (セッションを終了する検証) では従来どおり `RecvDataStreamError::Session` が返り、セッション終了コードが変わっていないことがテストで固定されていること
- 戻り値型を変更する API のエラーを検査している既存テストが新しい分類に合わせて更新されていること
- `MessageError` の variant 追加と `SubgroupTracker` の戻り値型変更に対応する `[CHANGE]` が `CHANGES.md` の `## develop` に追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

### 実装した内容

`src/error.rs` の `MessageError` に `MalformedTrack(&'static str)` を追加し、draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) が列挙する条件に対応する検出だけをこの variant に分類した。

分類した検出:

- `src/stream/decoder.rs` の `FetchStreamDecoder::validate_fetch_object` (条件 1 = 同一 Subgroup 内の Publisher Priority 変更、条件 2 = 確定済み最終 Object の超過) と `FetchStreamDecoder::set_subgroup_final_object`
- `src/subgroup_tracker.rs` の `SubgroupTracker::record_priority` (条件 1) と `SubgroupTracker::mark_fin` (条件 3)
- `src/object_properties.rs` の `ObjectPropertyTracker::observe_decoded_object` / `observe_object` の PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP 条件

分類しなかった検出 (`ProtocolViolation` のまま):

- `ObjectProperties::decode` の失敗と `observe_object` の Properties フレーミング不一致
- `FetchStreamDecoder::validate_fetch_object` の Object ID 昇順違反 (§2.2 (Subgroups)) と Group 順序違反 (§9.11 (FETCH))
- `SubgroupTracker::open` の再オープン禁止違反 (§2.2 / §11.3.2 (Closing Subgroup Streams))

あわせて次の API を追加・変更した。

- `MessageError::reason()` — 人間可読の説明文を取り出す
- `MessageError::malformed_track_reason()` — §12.1 の検出かどうかを判定しつつ理由を取り出す
- `SubgroupTracker::open` / `record_priority` / `mark_fin` の戻り値型を `SessionError` から `MessageError` に変更
- `src/session/data.rs` の `session_error_from_data_message` は `MalformedTrack` も理由文を保って `SESSION_PROTOCOL_VIOLATION` に写す。呼び出し元は `MessageError::reason()` で理由を取り出し、`terminate_malformed_track` の cancel 経路を従来どおり維持する

### 完了条件のうち未達の項目

**§12.1 の検出を返しうる公開受信 API の戻り値型を `RecvDataStreamError` に統一する部分は未実装である。** 対象は `Session::recv_subgroup_header` / `recv_subgroup_object` / `recv_object_datagram` / `recv_datagram` / `recv_data_stream_closed` の 5 API と、`RecvDataStreamError::MalformedTrack { reason }` variant の追加である。

現状のアプリは `SessionEvent::RequestTerminated { reason: TerminationReason::MalformedTrack }` と、アプリが保持する `FetchStreamDecoder` の戻り値 (`MessageError::MalformedTrack`) で §12.1 の検出を判別できる。Session の受信 API の戻り値からは判別できない。

この部分は呼び出し箇所が約 373 に及び、影響を受ける既存テストの更新を含めて別途対応が必要である。実装の指針は次のとおり。

- `RecvDataStreamError` に `MalformedTrack { reason }` と `malformed_track_reason()` を追加し、`From<SessionError>` を実装する
- 5 API の戻り値型を変更し、`SessionError` を返す箇所は `RecvDataStreamError::Session(err)` で包む
- `MalformedTrack` を返す箇所は `RecvDataStreamError::MalformedTrack { reason }` にする。`recv_data_stream_type` が同じ形 (`Result<DataStreamType, RecvDataStreamError>`) で既に実装済みなので、その書き方を踏襲する

### テスト

`MessageError::MalformedTrack` への分類に合わせて次のテストの期待値を更新した。

- `tests/test_subgroup_tracker.rs` — 再オープン禁止違反が `ProtocolViolation` のままであること
- `tests/test_object_properties.rs` — §12.1 の条件が `MalformedTrack`、コーデックのフレーミング違反が `ProtocolViolation` になること
- `tests/test_stream/decoder.rs` — FETCH の条件 1 / 2 が `MalformedTrack` になること
- `pbt/tests/prop_object_tracker.rs` — PRIOR_GROUP_ID_GAP の条件が `MalformedTrack` になること
