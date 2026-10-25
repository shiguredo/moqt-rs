# moq-sub が data stream の復号エラーで PROTOCOL_VIOLATION のセッション終了を要求する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header) は SUBGROUP_ID_MODE が 0b11 の値、bit 4 が立っていない値、128 以上の Type
Flags を受信した endpoint に PROTOCOL_VIOLATION でのセッション終了を MUST で要求する。同節は Object ID が 2^64-1 を超える場合も
PROTOCOL_VIOLATION を MUST とする。§11.1.2 (Object Properties) も非 Normal status への Properties 付与にセッション終了を MUST
とする。moq-sub は decoder が返すエラーを warn して stream を drain するだけで、セッションを閉じない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `decode_video_stream` / `decode_audio_stream` は `SubgroupStreamDecoder::try_decode_object`
  の `Err` を warn して stream を drain し、`request_session_termination` を呼ばない。
- 同じファイルの `handle_stream_body` は `try_decode_header` の `Err` を warn して drain する。
- `read_catalog_objects` / `read_catalog_fetch_objects` の decoder エラーは呼び出し側で warn して STOP_SENDING するだけで
  セッションを閉じない。
- 一方、LOC プロパティの不正では `request_session_termination` を呼んでセッション終了を要求しており、扱いが非対称である。
- `examples/tokio-moq/src/error.rs` の `session_error_code` は `MessageError` から終了コードへの写像を持ち、
  `KeyValueFormattingError` を 0x6、`MalformedAuthToken` を 0x16、それ以外を 0x3 に写す。`UnexpectedEof` も 0x3 になる。
- `src/error.rs` の `MessageError::MalformedTrack` は draft §12.1 の malformed Track 検出用であり、「セッションは終了しない」
  と明記されている。同じ decoder エラーでも `ProtocolViolation` / `KeyValueFormattingError` と `MalformedTrack` は扱いが異なる。

## 設計方針

- data stream の decoder が返した `MessageError` を終了要求の対象 / 対象外に分類する小さな関数を
  `examples/tokio-moq/src/error.rs` に追加する (`data_stream_termination_code(error: &MessageError) -> Option<u64>`)。
  - `MessageError::UnexpectedEof` は `None`。`try_decode_object` / `try_decode_header` はデータ不足を `Ok(None)` に吸収するため
    通常は届かないが、届いてもシリアライズ途中の終了とは別の文脈として扱う。`finish()` の `UnexpectedEof` は 0371 の
    `report_mid_object_fin` の対象であり、本関数は `finish()` 呼び出し経路には適用しない。
  - `MessageError::MalformedTrack` は `None`。draft §12.1 は該当 Track の subscription / FETCH の cancel を MUST とし、
    セッション終了を要求しない。ライブラリの Session が既に cancel を発行する経路であり、examples が追加でセッションを
    閉じると §12.1 の意図に反する。
  - それ以外 (`ProtocolViolation` / `KeyValueFormattingError` / `MalformedAuthToken` など) は既存の `session_error_code` で
    終了コードへ写した `Some(code)` を返す。
- moq-sub の video / audio / catalog / fetch / header の全経路で、decoder エラーを上記関数で分類し、`Some(code)` のとき
  `request_session_termination(code, ...)` を呼ぶ。`None` は従来どおり warn + drain / STOP_SENDING とする。
- `request_session_termination` の doc とログを「LOC Properties の decode 失敗」専用から「data stream の復号失敗」へ一般化する。
- 終了要求後は既存どおり stream を drain / STOP_SENDING して後始末し、他の stream task の処理を妨げない。
- `data_stream_termination_code` の unit test で `ProtocolViolation` → `Some(0x3)`、`KeyValueFormattingError` → `Some(0x6)`、
  `MalformedAuthToken` → `Some(0x16)`、`UnexpectedEof` → `None`、`MalformedTrack` → `None` を固定する。

## 完了条件

- 不正な Subgroup Header (SUBGROUP_ID_MODE 0b11 など) を moq-sub が受信したとき、`PROTOCOL_VIOLATION` (0x3) での
  セッション終了要求が出ること。
- Object ID overflow と Object Properties の不正でも同様に `PROTOCOL_VIOLATION` (0x3) の終了要求が出ること。
- `KeyValueFormattingError` では KEY_VALUE_FORMATTING_ERROR (0x6) の終了要求が出ること。
- `UnexpectedEof` と `MalformedTrack` では終了要求を出さないこと。
- `request_session_termination` の doc とログが data stream の復号失敗全般を表す文言になっていること。
- `docs/missing-features.md` 等に本件の未対応記述が残っていないこと (存在する場合)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 対象外

- decoder の `finish()` が返す `UnexpectedEof` の扱い (0371 の mid-object FIN 対応で扱う)。
- peer publisher の malformed Track に対する subscription / FETCH の cancel。example が decoder を直接使う media FETCH
  応答経路は Session に Object を渡さないため、Session の cancel は発行されない。example 側の cancel 発行は
  本 issue では未対応のままとする (既知の制約。必要なら別 issue で扱う)。

## 補足

- moq-sub の pipeline は transport から受け取ったヘッダを直接 decoder に渡す純関数の集合であり、不正ヘッダの受信から
  終了要求までの結合テストは既存の e2e では作れない。`data_stream_termination_code` の unit test と、各呼び出し経路が
  関数を呼ぶことのコードレビューで担保する。
