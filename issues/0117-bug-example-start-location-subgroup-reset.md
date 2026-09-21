# publisher example が Start Location 由来のスキップでも RESET する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-start-location-subgroup-reset
- Polished: 2026-09-21

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

購読の Start Location より前の Object をスキップした場合は FIN、REQUEST_UPDATE による範囲変更や Forward State 0 によるスキップの場合は RESET である。現状の publisher example はスキップ理由を区別せず常に RESET するため、Start Location 由来のスキップで FIN すべきケースが MUST 違反になる。

## 現状

- `examples/moqt-publisher/src/stream_writer.rs` の `SubgroupObjectState::needs_reset` は `!self.header_sent || self.has_skipped` を返す。`has_skipped` は `SubgroupObjectState::on_skip` がフィルタ不通過のたびに立てるだけで、理由も Location も持たない
- 同ファイルの `SubgroupWriter::finish` が `needs_reset` で FIN と RESET を選ぶ。doc コメントも「Start Location 未満の Object のみを省略した場合は FIN 側だが、本 example はフィルタ不通過による省略を reset として統一する」と逸脱を認識している
- `SubgroupWriter::new` は `handle` / `data_plane` / `request_id` / `track_alias` / `group_id` / `publisher_priority` / `has_properties` を受け取るが、購読の Start Location も Forward State も保持しない。判定は `SubgroupWriter::finish` の中にあり、`transport::SendStream` を要求するため example の単体テストから FIN / RESET の選択を観測できない
- `examples/moqt-transport/src/moqt_client.rs` の `MoqtClient` は `Session` を private に保持し、購読情報を読む公開 accessor を持たない。`examples/moqt-publisher/src/pipeline.rs` は REQUEST_UPDATE を `Session::send_request_ok` に渡すのみで、購読の範囲が変わったかどうかを保持していない
- `examples/moqt-publisher/src/datagram_writer.rs` の `DatagramWriter` も同じ `ObjectFilterOutcome::Skip` を受けるが、datagram には閉じ方の選択が無いため本 issue の対象外である
- library 本体の `src/session/data.rs` の `Session::send_subgroup_object` は `SendRequestError::LocalFilterMismatch` を返してアプリに判断を委ね、FIN と RESET を決めない
- `SendRequestError::LocalFilterMismatch` は理由を持たず、`examples/moqt-transport/src/moqt_client.rs` の `DataPlaneHandle::send_subgroup_object` が `ObjectFilterOutcome::Skip` に畳むため、example 側に理由が伝わらない
- `src/session/subscription/validation.rs` の `object_passes_filters` は Forward State / Location Filter / Range Filter をまとめて評価し bool のみを返す
- `Session::subscription` から `Subscription::filter_start` と `Subscription::forward_state` を参照できる。`Subscription::filter_start` は REQUEST_UPDATE の適用時に `src/session/subscription/dispatch.rs` で現在値へ上書きされるため、「初期値」ではなく「その時点の値」である

## 設計方針

- library の公開 API は変えない。`SendRequestError::LocalFilterMismatch` に理由を持たせる案は、公開 enum の unit variant を struct variant に変える破壊的変更 (`matches!` を使う既存テストが壊れる) になるため採らない。スキップ理由の判定は example 側で行う
- example が購読の Start Location を「Subgroup 開始時点の値」として snapshot する。`examples/moqt-transport/src/moqt_client.rs` に `Session` の購読情報を読む read-only accessor を追加し、publisher example が Subgroup を開始する時点の `Subscription::filter_start` を取得する
- 終端の判定を sans-I/O な関数に分離する (closed 0034 が `first_object` の決定で採った方針と同じ)。`SubgroupWriter::finish` は判定結果 (`SubgroupTermination::Fin` / `SubgroupTermination::Reset`) に従って `finish` / `reset` を呼ぶだけにし、判定は `transport::SendStream` に依存しない形にする
- 判定規則は次のとおりにする。いずれかに該当すれば RESET、すべて該当しなければ FIN
  - `SUBGROUP_HEADER` を一度も送っていない (`!header_sent`)
  - スキップした Object のうち 1 つでも、Subgroup 開始時点の Start Location 以降の Location を持つ。Subgroup 開始時点の Start Location は `Subscription::filter_start` を Subgroup 開始時に snapshot した値であり、`None` (Location Filter 無し) の場合はすべての Object が該当する
- この規則は §11.3.2 の FIN 条件「Start Location より前の Object 以外をすべて配送した」をそのまま写したものである。REQUEST_UPDATE による Start Location の拡大や End Group の縮小で配送されなくなった Object は、いずれも「Subgroup 開始時点の Start Location 以降の Location を持ちながら配送されなかった Object」として RESET 側に落ちる
- 範囲が変わったかどうかは追跡しない。更新の有無ではなく「配送されなかった Object の Location」で決まるため、no-op の REQUEST_UPDATE 再適用や、全 Object を配送した後の範囲変更で RESET に倒れることはない
- `!header_sent` は RESET を維持する。Subgroup を開始したが配送可能な Object が 1 つも無かった場合であり、購読者へ空の Subgroup を通知する意味が無いため、現行どおり RESET で閉じる。この判断を doc コメントに書く
- Range Filter (OBJECT_PROPERTY_FILTER 等) の不通過は Location で分類する。§11.3.2 の FIN 条件は「Start Location より前の Object 以外をすべて配送した」ことなので、Range Filter で落ちた Object が Start Location 以降なら RESET 側になる。この判断を doc コメントに書く
- スキップ理由を伝えるための `ObjectFilterOutcome` / `SendRequestError` の変更は行わない。example は `ObjectFilterOutcome::Skip` と、保持している Object の Location だけで判定する
- library の `Session::send_subgroup_object` が FIN と RESET を決めない現状の責務分担は変えない
- 変更対象は次の 4 ファイルである
  - `examples/moqt-transport/src/moqt_client.rs`: `MoqtClient` に購読の `Subscription::filter_start` を読む read-only accessor を追加する
  - `examples/moqt-publisher/src/stream_writer.rs`: `SubgroupTermination` とその判定関数を追加し、`SubgroupObjectState` がスキップした Object の Location と Start Location の snapshot を保持する形にする。`SubgroupWriter::finish` は判定結果で `finish` / `reset` を呼び分ける
  - `examples/moqt-publisher/src/pipeline.rs`: Subgroup 開始時に accessor から snapshot を取得して `SubgroupWriter::new` に渡す (映像と音声の 2 箇所)
  - `examples/moqt-publisher/src/catalog.rs`: `SubgroupWriter::new` の 3 番目の呼び出し元である。`CatalogParams` は `MoqtClient` を持たないため snapshot に `None` を渡す。Location Filter 無しでは Start Location による除外が無いので、スキップがあれば RESET 側になる (カタログ track の購読フィルタで省略された場合は RESET が正しい)
- `examples/moqt-publisher/src/datagram_writer.rs` は対象外とする (datagram に閉じ方の選択が無い)

## 完了条件

- Subgroup 開始時点の Start Location より前の Object だけをスキップした Subgroup の判定が FIN になることが、example の sans-I/O な単体テストで固定されていること
- 次のそれぞれで判定が RESET になることが単体テストで固定されていること
  - Subgroup 開始時点の Start Location 以降の Location を持つ Object をスキップした場合 (Forward State 0 による省略と Range Filter 不通過を含む)
  - `SUBGROUP_HEADER` を一度も送っていない場合
- 判定規則 (Range Filter 不通過と `!header_sent` の扱い) が doc コメントに明記されていること
- 判定が `SubgroupWriter` の外へ分離され、`transport::SendStream` を構築せず単体テストできる形になっていること
- library の公開 API (`SendRequestError` / `ObjectFilterOutcome`) を変更していないこと
- `cargo test --workspace` が通ること
