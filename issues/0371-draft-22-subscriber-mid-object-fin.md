# moq-sub が data stream の FIN で decoder の途中終了を検査する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §11.3 (Subgroup Streams) は "If a stream ends gracefully (i.e., the stream terminates with a FIN) in
the middle of a serialized Object, the session SHOULD be closed with a PROTOCOL_VIOLATION." と SHOULD を課す。ライブラリは
`SubgroupStreamDecoder::finish` / `FetchStreamDecoder::finish` と `Session::report_mid_object_fin` を提供しているが、moq-sub は FIN
を観測してもこれらを呼ばないため、シリアライズ途中の FIN を検出できない。

## 現状

- `src/stream/decoder.rs` の `SubgroupStreamDecoder::finish` と `FetchStreamDecoder::finish` は、途中終了なら `MessageError::UnexpectedEof`
  を返す。
- `src/session/data.rs` の `Session::report_mid_object_fin` は、FIN が Object のシリアライズ途中で届いたことを Session に報告し、PROTOCOL_VIOLATION
  でセッションを fail する。破棄対象の stream では no-op で受理する。`recv_data_stream_closed` との呼び出し順序は問わない契約になっている。
- `examples/moq-sub/src/pipeline.rs` の video / audio / catalog / fetch の各ループは、`receive_registered_stream_data` が `Ok(None)` (FIN)
  を返すと decoder の `finish()` を呼ばずに処理を終える。
- ライブラリ側の `report_mid_object_fin` と decoder の `finish` は tests で固定されているが、examples からの呼び出しは 0 件である。

## 設計方針

- data stream の FIN を観測した時点で decoder の `finish()` を呼び、`Err(MessageError::UnexpectedEof)` なら `DataPlaneHandle` 経由で
  `Session::report_mid_object_fin` を呼ぶ。
- `finish()` の `Err` のうち `UnexpectedEof` は「シリアライズ途中の FIN」を意味するため、そのまま報告対象とする。それ以外の `MessageError` は decoder
  の実装不整合として扱い、`report_mid_object_fin` ではなく復号エラーとしてのセッション終了要求 (別 issue の経路) に寄せる。
- video / audio / catalog (subgroup / fetch) の全ループに適用する。`receive_registered_stream_data` が FIN 時に `recv_data_stream_closed`
  を通知する既存経路との順序は問わない契約に従う。
- 途中終了を検出して報告する処理を小さなヘルパに切り出し、`UnexpectedEof` でのみ報告することを unit test で固定する。

## 完了条件

- moq-sub が Object のシリアライズ途中で FIN を受けたとき、`report_mid_object_fin` を経由して `PROTOCOL_VIOLATION` でのセッション終了が起きること。
- 正常終了した stream と、Object 境界で FIN した stream ではセッションを閉じないこと。
- 破棄対象の stream では従来どおり no-op で吸収されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
