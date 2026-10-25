# moq-sub が data stream の復号エラーで PROTOCOL_VIOLATION のセッション終了を要求する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header) は SUBGROUP_ID_MODE が 0b11 の値、bit 4 が立っていない値、128 以上の Type Flags を受信した
endpoint に PROTOCOL_VIOLATION でのセッション終了を MUST で要求する。同節は Object ID が 2^64-1 を超える場合も PROTOCOL_VIOLATION を MUST とする。§11.1.2 (Object
Properties) も非 Normal status への Properties 付与にセッション終了を MUST とする。moq-sub は decoder が返すエラーを warn して stream を drain するだけで、セッションを閉じない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `decode_video_stream` / `decode_audio_stream` は `SubgroupStreamDecoder::try_decode_object`
  の `Err` を warn して stream を drain し、`request_session_termination` を呼ばない。
- 同じファイルの `handle_stream_body` は `try_decode_header` の `Err` を warn して drain する。
- `read_catalog_objects` / `read_catalog_fetch_objects` の decoder エラーは呼び出し側で warn して STOP_SENDING するだけでセッションを閉じない。
- 一方、LOC プロパティの不正では `request_session_termination` を呼んでセッション終了を要求しており、扱いが非対称である。
- `examples/tokio-moq/src/error.rs` の `session_error_code` が `MessageError` から終了コードへの写像を持ち、`KeyValueFormattingError` は 0x6、それ以外は
  0x3 に写す。

## 設計方針

- data stream の decoder が返した `MessageError` を `session_error_code` で終了コードに写し、`request_session_termination` を呼ぶ。moq-sub の video /
  audio / catalog / fetch の全経路に適用する。
- `UnexpectedEof` は decoder が `Ok(None)` (データ不足) に吸収するため通常は届かないが、届いた場合はシリアライズ途中の FIN
  とは別の文脈であるため、終了要求の対象外として扱う。`ProtocolViolation` と `MalformedTrack`、`KeyValueFormattingError` は終了対象とする。
- 終了要求後は既存どおり stream を drain / STOP_SENDING して後始末し、他の stream task の処理を妨げない。
- 終了判定を小さな関数に切り出し、`MessageError::ProtocolViolation` で `Some(0x3)`、`MessageError::UnexpectedEof` で `None` を返すことを unit test で固定する。

## 完了条件

- 不正な Subgroup Header (SUBGROUP_ID_MODE 0b11 など) を moq-sub が受信したとき、`PROTOCOL_VIOLATION` (0x3) でのセッション終了要求が出ること。
- Object ID overflow と Object Properties の不正でも同様に終了要求が出ること。
- `UnexpectedEof` では終了要求を出さないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
