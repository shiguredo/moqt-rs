# publisher example が Start Location 由来のスキップでも RESET する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-start-location-subgroup-reset
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) は FIN と RESET_STREAM の使い分けを MUST で規定する。

> If a sender has delivered all objects in a Subgroup to the QUIC
> stream, except any Objects with Locations smaller than the
> subscription's Start Location, it MUST close the stream with a FIN.

```text
If a sender closes the stream before delivering all such objects to
the QUIC stream, it MUST reset the stream.  This includes, but is not
limited to:

*  A REQUEST_UPDATE moving the subscription's End Group to a smaller
   Group or the Start Location to a larger Location

*  Omitting a Subgroup Object due to the subscriber's Forward State
```

購読の初期 Start Location より前の Object をスキップした場合は FIN、REQUEST_UPDATE による範囲変更や Forward State 0 によるスキップの場合は RESET である。現状の publisher example はスキップ理由を区別せず常に RESET するため、Start Location 由来のスキップで FIN すべきケースが MUST 違反になる。

## 現状

- `examples/moqt-publisher/src/stream_writer.rs` の `SubgroupObjectState::needs_reset` は `!self.header_sent || self.has_skipped` を返す。`has_skipped` は `SubgroupObjectState::on_skip` がフィルタ不通過のたびに立てるだけで、理由を持たない。
- 同ファイルの `SubgroupWriter::finish` が `needs_reset` で FIN と RESET を選ぶ。doc コメントも「Start Location 未満の Object のみを省略した場合は FIN 側だが、本 example はフィルタ不通過による省略を reset として統一する」と逸脱を認識している。
- `examples/moqt-publisher/src/datagram_writer.rs` の `DatagramWriter` も同じ `ObjectFilterOutcome::Skip` を受けるが、datagram には閉じ方の選択が無いため本 issue の対象外である。
- library 本体の `src/session/data.rs` の `Session::send_subgroup_object` は `SendRequestError::LocalFilterMismatch` を返してアプリに判断を委ね、FIN と RESET を決めない。
- `SendRequestError::LocalFilterMismatch` は理由を持たず、`examples/moqt-transport/src/moqt_client.rs` の `DataPlaneHandle::send_subgroup_object` が `ObjectFilterOutcome::Skip` に畳むため、example 側に理由が伝わらない。
  フィルタ判定は `src/session/subscription/validation.rs` の `object_passes_filters` が Forward State / Location Filter / Range Filter をまとめて評価し bool のみを返す。
- `Session::subscription` から `Subscription::filter_start` を参照できるが、`SubgroupWriter` は `request_id` しか受け取らず購読の Start Location を保持していない。

## 設計方針

- スキップ理由 (購読の初期 Start Location 由来か、REQUEST_UPDATE / Forward State 0 による範囲変更由来か) を writer まで伝え、`SubgroupWriter::finish` が理由に応じて FIN と RESET を選ぶ。
- library API で理由が得られない場合の手段を決めて明記する。候補は次の 2 つである。
  - library 側で理由を返せるようにする (`SendRequestError::LocalFilterMismatch` に理由を持たせる、または `ObjectFilterOutcome` を理由付きにする)。この場合は `DataPlaneHandle` まで通す変更が要る。
  - example 側で購読の初期 Start Location を保持する。`IncomingRequest` の SUBSCRIBE パラメータと `IncomingRequestUpdate` のパラメータから example が Start Location を解決し、REQUEST_UPDATE で範囲が変わったかどうかを区別する。
- 一度も Pass していない場合 (`!header_sent`) の扱いを決める。全 Object が Start Location より前でスキップされた場合は FIN 側になりうるが、SUBGROUP_HEADER を 1 度も送っていないストリームを FIN で閉じることの是非を含めて判断し、テストで固定する。
- Range Filter (OBJECT_PROPERTY_FILTER 等) の不通過をどちらに分類するかを決める。§11.3.2 は Forward State と REQUEST_UPDATE を reset 側に明示列挙するのみで、Range Filter 不通過は明示していない。判断結果を doc コメントに書く。
- library の `Session::send_subgroup_object` が FIN と RESET を決めない現状の責務分担は変えない。

## 完了条件

- 購読の初期 Start Location 由来のスキップだけで終わった Subgroup が FIN で閉じられ、REQUEST_UPDATE の範囲変更または Forward State 0 由来のスキップがある Subgroup が RESET で閉じられることが、example の判定を固定するテストで確認できること (example のテストは `cargo test --workspace` の対象である)。
- スキップ理由の分類 (Range Filter 不通過と `!header_sent` の扱い) が doc コメントに明記されていること。
- `cargo test --workspace` が通ること。
