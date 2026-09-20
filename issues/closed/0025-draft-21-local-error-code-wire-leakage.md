# ローカル専用エラーコードの wire 流出を API で防ぐ

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/change-local-error-code-wire-leakage
- Polished: 2026-09-10

## 目的

`SESSION_LOCAL_FILTER_MISMATCH` と `SESSION_LOCAL_DATAGRAM_TIMEOUT` が wire へ流出する footgun をなくす。これらは実装内部の値であり、draft-ietf-moq-transport-21 のどのエラーコードレジストリにも存在しない。

## 現状

- `src/error.rs` の `SESSION_LOCAL_FILTER_MISMATCH` (`0xFFFF_FFFF_FFFF_FF01`) と `SESSION_LOCAL_DATAGRAM_TIMEOUT` (`0xFFFF_FFFF_FFFF_FF02`) は doc で「wire に送出してはならない」と明記されているが、送信 API が `SessionError` として返すため、アプリが `err.code` をそのまま wire コードを取る公開 API に渡すと未定義コードが載る。
- 返却箇所:
  - `Session::send_subgroup_object` (`src/session/data.rs`): 購読フィルタ不通過時に `SESSION_LOCAL_FILTER_MISMATCH`
  - `Session::send_object_datagram` (`src/session/data.rs`): 購読フィルタ不通過時に `SESSION_LOCAL_FILTER_MISMATCH`、datagram delivery timeout 超過時に `SESSION_LOCAL_DATAGRAM_TIMEOUT`
  - `Session::send_publish` (`src/session/subscription/send.rs`): peer の TRACK_PROPERTY_FILTER 不通過時に `SESSION_LOCAL_FILTER_MISMATCH` (`SendRequestError::Session` に包んで返す)
- wire のエラー / ステータスコードを `u64` で受ける公開 API は次の 5 つ。本 crate の varint は 9 バイト (u64::MAX) まで符号化するため、ローカル専用コードをこれらに渡すと未登録値がそのまま wire に出る。
  - `Session::close(code, reason)`
  - `Session::send_request_error(request_id, error_code, ...)`
  - `Session::send_publish_done(request_id, status_code, ...)`
  - `Session::reset_outgoing_data_stream_with_code(stream_id, error_code)`
  - `Session::reset_outgoing_data_stream_at_with_code(stream_id, error_code, reliable_size)`
  - `Session::fail(err: SessionError)` は crate 内専用 (`pub(super)`) でアプリからは呼べない。
- 同梱 example の QUIC 直結経路は `s2n-quic` の application error が QUIC varint 範囲 (最大 2^62-1) 外を `UNKNOWN` に落とすが、WebTransport 経路は `as u32` に切詰めて未登録値が載りうる。`examples/moqt-transport` は `e.code == SESSION_LOCAL_FILTER_MISMATCH` で `Skip` を判定しており、API 変更の追従が必要。
- 既存テストが `err.code` でローカルコードを検証している (`tests/test_session/object_filter_pass.rs` / `data_stream.rs` の datagram timeout / `track_property_filter.rs`)。`send_subgroup_object` の戻り値型変更で `tests/test_session/subscription/request_update.rs` も追従が必要。

## 設計方針

ローカル拒否を `SessionError` から分離し、wire に出さないことを型で表す。既存 `SendRequestError::PeerGoawayReceived` (wire に出さないローカルエラーの前例) と同じ形に揃える。

- `SendRequestError` (`src/session/types.rs`) にコードを持たない variant を追加する:
  - `LocalFilterMismatch`: 送信 Object / Track Property が購読フィルタを通らないローカル拒否 (draft §3.3.3)
  - `LocalDatagramTimeout`: datagram の delivery timeout 超過によるローカルドロップ (draft §5.2)
  - `SendRequestError::as_session_error()` はこれらと `PeerGoawayReceived` に対して `None` を返す。
- ローカルコードを返す送信 API の戻り値型を `Result<(), SendRequestError>` に変更する:
  - `Session::send_subgroup_object` / `Session::send_object_datagram` (`src/session/data.rs`)
  - `Session::send_publish` (`src/session/subscription/send.rs`) は既に `Result<u64, SendRequestError>` のため、`SESSION_LOCAL_FILTER_MISMATCH` を `.into()` で `SendRequestError::Session` に包む代わりに `SendRequestError::LocalFilterMismatch` を返す。
  - ローカルコードを返さない他の送信 API (`send_subgroup_header` 等) は戻り値型を変えない。
- wire コードを受ける公開 API では、ローカル専用コードを指定レジストリの `*_INTERNAL_ERROR` に置換して渡す (`REQUEST_INTERNAL_ERROR` / `PUBLISH_DONE_INTERNAL_ERROR` / `STREAM_INTERNAL_ERROR` は 0x0、`SESSION_INTERNAL_ERROR` のみ 0x1)。置換判定は `error.rs` の共通ヘルパ (`is_local_error_code`) に集約する:
  - `Session::close` → `SESSION_INTERNAL_ERROR`
  - `Session::send_request_error` → `REQUEST_INTERNAL_ERROR`
  - `Session::send_publish_done` → `PUBLISH_DONE_INTERNAL_ERROR`
  - `Session::reset_outgoing_data_stream_with_code` / `reset_outgoing_data_stream_at_with_code` → `STREAM_INTERNAL_ERROR`
  - crate 内専用の `Session::fail` にも同じ検査を防御として入れる。
- `SESSION_LOCAL_*` 定数は互換のため `pub` のまま残し、doc を「これらの値は `SendRequestError::LocalFilterMismatch` / `LocalDatagramTimeout` として返り、`SessionError` には載らない。wire コードを取る公開 API に渡してもレジストリの `*_INTERNAL_ERROR` に置換される」に更新する。`src/error.rs` の「`Session::fail` (crate 内専用)」記述は実態どおりなので、参照先を新 variant に合わせる。
- 公開 API の後方互換のない変更のため、ブランチは `feature/change-...`、`CHANGES.md` は `[CHANGE]` に記載する。
- `examples/moqt-transport` の `Skip` 判定を `SendRequestError::LocalFilterMismatch` のマッチに変更する。`examples/moqt-publisher` の doc コメントを新 variant 名に更新する。`skills/shiguredo-moqt/SKILL.md` の対象 API 署名・`SendRequestError` enum 一覧・`as_session_error` 注記を追従する。

## 完了条件

- `send_subgroup_object` / `send_object_datagram` / `send_publish` がローカル拒否時に `SendRequestError::LocalFilterMismatch` / `SendRequestError::LocalDatagramTimeout` を返し、`as_session_error()` が `None` を返すこと
- `close` / `send_request_error` / `send_publish_done` / `reset_outgoing_data_stream_with_code` / `reset_outgoing_data_stream_at_with_code` にローカル専用コードを渡すと、それぞれの `*_INTERNAL_ERROR` に置換されて wire に未登録値が出ないこと (`src/session/tests.rs` の crate 内テスト)
- 既存テスト (`tests/test_session/object_filter_pass.rs` / `data_stream.rs` / `track_property_filter.rs` / `subscription/request_update.rs`) と `examples/` を新 API に追従し、`cargo test --workspace` と PBT が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` として記載されていること

## 解決方法

ローカル専用エラーコードを `SessionError` から分離し、wire 流出を API で防いだ。公開 API の後方互換のない変更のため `[CHANGE]`。

- `SendRequestError` にコードを持たない `LocalFilterMismatch` / `LocalDatagramTimeout` を追加し、`as_session_error()` はこれらと `PeerGoawayReceived` に `None` を返す。
- `send_subgroup_object` / `send_object_datagram` の戻り値型を `Result<(), SendRequestError>` に変更し、ローカル拒否を新 variant で返す。`send_publish` の TRACK_PROPERTY_FILTER 不通過も `SendRequestError::LocalFilterMismatch` に変更した。
- `close` / `fail` / `send_request_error` / `send_publish_done` / `reset_outgoing_data_stream_with_code` / `reset_outgoing_data_stream_at_with_code` で、ローカル専用コードを各レジストリの `*_INTERNAL_ERROR` に置換する (`is_local_error_code` に集約)。`emit_request_error` にも最終防御として同検査を入れた。
- `src/error.rs` / `src/session/types.rs` / `SKILL.md` の doc を新設計に合わせ、examples の `Skip` 判定と doc、既存テストを新 API に追従させた。
- テスト: `src/session/tests.rs` に 5 API の置換 (`close` / `fail` / `send_request_error` / `send_publish_done` / `reset_outgoing_data_stream_with_code` / `_at_with_code`) と `as_session_error()` が `None` を返す検証を追加した。
- `CHANGES.md` の `[CHANGE]` に記載した。

見送った改善: `SendRequestError` / `SessionError` への `Copy` derive は、既存の `SessionError::clone()` が多数あり clippy の `clone_on_copy` を誘発するため見送った。
