# Malformed Track の検出を購読単位の cancel として扱えるようにする

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-malformed-track-cancel-classification
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) は、subscriber が Malformed Track を検出したとき「該当する subscription または fetch を cancel し (MUST)、アプリへエラーを届ける (SHOULD)」と定める。cancel の手段は §6.4.2.3 (Request Cancellation and Rejection) が「送信中の方向は RESET_STREAM、受信中の方向は STOP_SENDING で打ち切る」と規定しており、影響は該当 Track に閉じ、セッションは維持される。

現状は、この条件で返るエラーがセッション終了コード `SESSION_PROTOCOL_VIOLATION` に写されるため、アプリは「セッションを閉じるべきか、購読だけ cancel すべきか」をエラー種別から判別できない。`MessageError::ProtocolViolation` はフレーミング違反などセッションを閉じるべき検証でも使われており、同じ値に 2 つの意味が載っている。

## 現状

- `src/stream/decoder.rs` の `FetchStreamDecoder::validate_fetch_object` は、同一 Subgroup 内の Publisher Priority 変更・確定済み最終 Object を超える Object ID・昇順でない Object ID・Group 順序違反を `MessageError::ProtocolViolation` で返す (いずれも §12.1 の条件 1 / 2 / 3 相当)
- `src/object_properties.rs` の `ObjectPropertyTracker::observe_decoded_object` / `observe_object` も「malformed track」を理由に `MessageError::ProtocolViolation` を返す
- `src/subgroup_tracker.rs` の `SubgroupTracker::record_priority` / `mark_fin` は `SessionError { code: SESSION_PROTOCOL_VIOLATION }` を返す。
  `SubgroupTracker::check_object_after_fin` は `Option<&'static str>` (理由文字列) を返し、`src/session/data.rs` の `Session::attribute_subgroup_object` が `SessionError::new(SESSION_PROTOCOL_VIOLATION, reason)` に包む
- §12.1 条件 6 相当 (同一 Object の重複受信で Forwarding Preference / Subgroup ID / Priority が異なる) を検出するのは `ObjectFieldTracker::observe_object_fields` で、返る型は専用の `ObjectFieldMismatch` である。`Session` はそれを `SessionError::new(SESSION_PROTOCOL_VIOLATION, ...)` に写す
- `src/error.rs` の `MessageError` に Malformed Track を表す variant が無い
- `src/session/data.rs` の `Session::terminate_malformed_track` は該当する subscription のみを終了させているが、返る `SessionError` のコードはセッション終了コードのままである
- `src/subgroup_tracker.rs` は `pub mod subgroup_tracker` として公開されており、戻り値型の変更は破壊的変更になる

## 設計方針

- `src/error.rs` の `MessageError` に Malformed Track を表す variant を追加し、`src/stream/decoder.rs` と `src/object_properties.rs` の該当箇所がそれを返すようにする
- `src/subgroup_tracker.rs` の Malformed Track 条件 (条件 1 / 2 / 3) も同じ分類で返す。公開 API の戻り値型を変えるため、`CHANGES.md` の `## develop` に `[CHANGE]` を記載する
- `ObjectFieldMismatch` も同じ分類に寄せ、`Session` が「セッション終了コードへ写す」か「購読単位の cancel にする」かを選べるようにする
- Session 層では §12.1 の MUST に従い、該当 subscription / fetch の cancel (`SessionEvent::StopSendingRequestStream` → `SessionEvent::ResetRequestStream`) とセッション維持を既定にする
- フレーミング違反など、§12.1 の条件ではない既存の `MessageError::ProtocolViolation` の用途は変えない

## 完了条件

- §12.1 の条件 1 / 2 / 3 / 6 相当の入力に対し、返るエラー種別から「購読単位の cancel」であることが判別できること
- 該当 subscription / fetch だけが cancel され、セッションが閉じないことを固定するテストが `src/session/tests.rs` または `tests/test_session/` に追加されていること
- 既存のフレーミング違反 (セッションを閉じるべき検証) の挙動が変わっていないこと
- `SubgroupTracker` の公開 API を変更した場合は `CHANGES.md` の `## develop` に `[CHANGE]` が追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
