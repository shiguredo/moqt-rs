# moq-sub が data stream の FIN で decoder の途中終了を検査する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §11.3 (Subgroup Streams) は "If a stream ends gracefully (i.e., the stream terminates with a FIN) in
the middle of a serialized Object, the session SHOULD be closed with a PROTOCOL_VIOLATION." と SHOULD を課す。ライブラリは
`SubgroupStreamDecoder::finish` / `FetchStreamDecoder::finish` と `Session::report_mid_object_fin` を提供しているが、moq-sub は FIN
を観測してもこれらを呼ばないため、シリアライズ途中の FIN を検出できない。

## 現状

- `src/stream/decoder.rs` の `SubgroupStreamDecoder::finish` と `FetchStreamDecoder::finish` は、header 未受信、Object header 途中、
  payload 未消費のいずれでも `MessageError::UnexpectedEof` を返す。
- `src/session/data.rs` の `Session::report_mid_object_fin` は、FIN が Object のシリアライズ途中で届いたことを Session に報告し、
  PROTOCOL_VIOLATION でセッションを fail する。破棄対象の stream では no-op で受理する。RESET_STREAM による途中終了は
  draft §11.3.2 (Closing Subgroup Streams) の正常経路であり本 API の対象ではない。`recv_data_stream_closed` との呼び出し順序は
  問わない契約になっている。
- `examples/moq-sub/src/pipeline.rs` の `receive_registered_stream_data` は `RecvChunk::End(end)` を受けると
  `recv_data_stream_closed(stream_id, end)` を呼び、FIN / RESET_STREAM / Session からの STOP_SENDING 指示のいずれでも
  `Ok(None)` を返す。呼び出し側は終端種別を受け取れない。
- video / audio / catalog (subgroup / fetch) の各 Object ループは `Ok(None)` で処理を終えるが、decoder の `finish()` を呼ばない。
  header 待ちループも同じ `Ok(None)` で終わるが、header 未受信は §11.3 の「シリアライズ途中の Object」ではない。
- payload 読み出しループは、payload 途中の終端で `read_catalog_payload` / `read_fetch_entry` が
  `Error::Other("stream closed before ... payload")` を返し、video / audio の payload ループは `Ok(None)` で
  `return frames` / `return chunks` する。いずれも decoder の `finish()` を呼ばないため、`Ok(None)` だけを見る設計では
  payload 途中の FIN を検出できない。
- `examples/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle` は `Session::report_mid_object_fin` のラッパーを持たない。
- ライブラリ側の `report_mid_object_fin` と decoder の `finish` は tests で固定されているが、examples からの呼び出しは 0 件である。

## 設計方針

- `receive_registered_stream_data` の戻り値を、データあり / 終端 (終端種別付き) を区別できる小さな型に変更する。
  呼び出し側が `RequestStreamEnd::Fin` を判別できるようにし、RESET_STREAM と STOP_SENDING 指示は報告対象外とする。
- `examples/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle` に `report_mid_object_fin(stream_id)` ラッパーを追加する。
- `examples/moq-sub/src/pipeline.rs` に純関数 `mid_object_fin_should_report(end: &RequestStreamEnd, finish: &Result<(), MessageError>) -> bool`
  を追加する。`end` が `Fin` かつ `finish` が `Err(UnexpectedEof)` のときだけ true を返す。header 待ちループでは呼ばず、
  header 受理後の Object ループと payload ループ (video / audio / catalog subgroup / catalog fetch / media fetch、
  `read_catalog_payload` / `read_fetch_entry` を含む) でのみ、終端時に decoder の `finish()` を呼んだ結果を本関数へ渡し、
  true のとき `DataPlaneHandle::report_mid_object_fin` を呼ぶ。payload ループが途中終端で `Error::Other` を返す経路と、
  video / audio の payload ループが `Ok(None)` で返る経路も、返す前に `finish()` と本関数を通す。
- RESET_STREAM 終端と STOP_SENDING 指示 (timeout) は session が既に破棄対象として扱うため、報告しない。header 未受信の
  `finish()` も `AwaitingHeader` として `recv_data_stream_closed` が no-op で吸収する既存契約に合わせ、報告しない。
- `finish()` は現状 `UnexpectedEof` 以外を返さないため、それ以外の `MessageError` は防御的に「報告しない」とし、
  復号エラーとしてのセッション終了要求 (`request_session_termination_for_decode_error`) には寄せない
  (本 issue の対象外。将来 decoder が別エラーを返すようになったら再検討する)。
- `tests` は純関数の unit test (Fin + UnexpectedEof → true、Fin + Ok → false、Reset + UnexpectedEof → false) と、
  `DataPlaneHandle` のラッパー追加に対するライブラリ側既存テスト (`report_mid_object_fin` の no-op 含む) の維持で担保する。
  `DataPlaneHandle` はテストから構築できないため、example の結合テストは追加しない (検出と報告を純関数で分離している)。

## 完了条件

- `mid_object_fin_should_report` が FIN + `Err(UnexpectedEof)` でのみ true を返すこと。
- video / audio / catalog (subgroup / fetch) / media fetch の Object ループと payload ループが、header 受理後の終端で decoder の
  `finish()` を呼び、true のとき `DataPlaneHandle::report_mid_object_fin` を呼ぶこと (コードレビューで確認できる形にし、
  純関数のテストで固定する)。payload 途中の終端で `Error::Other` を返す経路も同じ処理を通ること。
- RESET_STREAM 終端、STOP_SENDING 指示、header 未受信の終端では `report_mid_object_fin` を呼ばないこと。
- `DataPlaneHandle::report_mid_object_fin` を追加し、`Session::report_mid_object_fin` の既存契約 (破棄対象は no-op) を変えないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 補足

- `DataPlaneHandle` は `fn new` が private で Session のアクセサも無いため、moq-sub の `#[cfg(test)]` からは構築できない。
  終端種別と `finish()` 結果から報告要否を決める純関数を切り出し、そちらを unit test の対象にする。

## 解決方法

- `examples/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle` に `report_mid_object_fin(stream_id)` を追加し、
  `Session::report_mid_object_fin` を呼べるようにした。
- `examples/moq-sub/src/pipeline.rs` に `RegisteredStreamData` (Data / End / StoppedBySession) と
  `receive_registered_stream_data_outcome` を追加し、`receive_registered_stream_data` は終端種別を捨てる
  既存 API として残した (header 待ちループは従来どおり)。FIN / RESET_STREAM / STOP_SENDING 指示を区別できる。
- `mid_object_fin_should_report(end, finish)` を追加し、`RequestStreamEnd::Fin` かつ
  `Err(MessageError::UnexpectedEof)` のときだけ true を返すようにした。`report_mid_object_fin_if_needed` が
  true のとき `DataPlaneHandle::report_mid_object_fin` を呼ぶ。
- video / audio の Object ループと payload ループ、catalog subgroup の Object ループと payload ループ、
  catalog fetch の entry ループと payload ループで、終端時に decoder の `finish()` を呼び、必要なときだけ
  mid-object FIN を報告するようにした。RESET_STREAM と STOP_SENDING 指示、header 未受信の終端では報告しない。
- `examples/moq-sub/src/pipeline.rs` の unit test に、FIN + UnexpectedEof のみ報告し、Object 境界の FIN
  (`Ok(())`) と RESET_STREAM では報告しないことを固定するテストを追加した。

## 完了条件の検証

- FIN + UnexpectedEof のみ報告: `mid_object_fin_reports_only_fin_with_unexpected_eof`
- Object ループ / payload ループからの報告: コードレビューと上記の純関数テストで担保する
  (`DataPlaneHandle` はテストから構築できないため、検出と報告を分離した)。
- RESET_STREAM / STOP_SENDING / header 未受信で報告しないこと: 純関数テストと分岐で担保する。
- `DataPlaneHandle::report_mid_object_fin` を追加し、`Session::report_mid_object_fin` の既存契約は変更していない。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功
