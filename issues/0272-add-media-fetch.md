# メディアの Object を FETCH で取得できるようにする

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/add-media-fetch
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.2 (Fetch) の FETCH は、Start Location から End Location までの
既存 Object を取り出す唯一の要求側メッセージである。§3.5 (Joining an Ongoing Track) の途中参加、
§3.2.2 (Gaps in a Fetch Stream) の欠落回収、MSF draft-01 §5.6.7 (Time-aligned VOD) の
任意位置再生はいずれも FETCH を前提にする。

現状は catalog 以外の FETCH に応答できず、subscriber から FETCH を発行する API も無いため、
シーク / 巻き戻し / 欠落回収 / VOD が成立しない。ライブラリは要求側と応答側の双方を実装済みで、
足りていないのは transport 層の API と example の保持・配線である。

## 現状

- ライブラリは両側を実装済み: `Session::send_fetch`、`SessionEvent::FetchOkReceived`
  (`end_location` / `end_of_track`)、`FetchStreamDecoder` / `FetchStreamEncoder`、
  §3.2.2 の End of Non-Existent / Unknown / Timed-Out Range の encode / decode
- `examples/tokio-moq` の `MoqtClient` に FETCH を発行する API が無い。0224 で catalog 取得を
  `FILL_PARAMETERS` 付き SUBSCRIBE に寄せたあと、到達不能な公開 API として 0235 で削除している。
  認可トークンを付与する `with_auth_parameters` の対象にも FETCH は含まれていない
- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は catalog の FETCH だけに応答し、
  メディアの FETCH は `REQUEST_DOES_NOT_EXIST` で拒否する。`run` の `OpenFillFetchStream` 処理も
  `reject_fill_fetch` で catalog 以外を拒否する
- moq-pub が保持するのは最後の catalog JSON と Location (`catalog_json` / `catalog_location`) だけで、
  メディアの Object を保持しない
- `examples/moq-sub/src/pipeline.rs` は FETCH を発行しない。`handle_incoming_stream` は届いた
  fill fetch stream を読み捨てる
- MSF の `PROP_MAX_CACHE_DURATION` は library にあるが、publisher は告知していない

## 設計方針

- publisher 側: moq-pub にメディアの保持領域を追加する。GROUP 単位で直近 N Group をメモリに保持し
  (`--cache-groups` で指定、既定値は実装時に決める)、`serve_peer_request` の Fetch 分岐と
  `run` の `OpenFillFetchStream` 処理で保持範囲の Object を返す。範囲外は FETCH では
  `INVALID_RANGE`、fill fetch では `reject_fill_fetch` で伝える。返すには LOC Properties と
  payload の両方が要るため、`Location` をキーにした順序付きの保持構造を example に置く。
  告知する `MAX_CACHE_DURATION` と保持時間を整合させる
- transport 層: `MoqtClient` に FETCH 発行 API を戻す。`Session::send_fetch` で request を出し、
  `SessionEvent::FetchOkReceived` を待って `FetchStreamDecoder` で応答 Object を読む経路を作る。
  `with_auth_parameters` の付与対象に FETCH を含める
- subscriber 側: 取得した Object を通常の再生経路 (`handle_stream_body` など) に流す。
  最初の実装では「購読開始時に Largest Object までの直近 Group を FETCH し、キーフレームから
  再生を始める」経路に限定する。任意位置シークの操作 UI は別 issue とする
- fill fetch をメディアにも広げるかは FETCH 発行経路と役割が重なるため、どちらを既定にするかを
  実装時に決める。fill fetch は publisher 主導で購読確立時にしか走らない点が FETCH と異なる
- キャッシュを持たない publisher (リレーを含む) に対しては、従来どおり `INVALID_RANGE` が返る
  経路をそのまま使う

## 完了条件

- moq-sub が FETCH を発行し、`FetchOkReceived` の End Location と End of Range の 3 種を解釈して
  Object を受信できること
- moq-pub が保持範囲のメディア Object を FETCH と fill fetch で返し、範囲外を `INVALID_RANGE` /
  reset で伝えること
- 途中参加時に、FETCH で取得した過去 Group からキーフレームで再生を開始できること
- 上記が `examples/tokio-moq/src/peer_harness.rs` または example のテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
