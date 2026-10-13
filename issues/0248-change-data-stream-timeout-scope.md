# DATA_STREAM_TIMEOUT でセッション全体を閉じる挙動の扱いを決める

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/change-data-stream-timeout-scope
- Polished: {YYYY-MM-DD}

## 目的

`Session::tick` は `data_stream_timeout_ms` を超えて activity が更新されていない Data Stream が 1 本でもあると、
`SESSION_DATA_STREAM_TIMEOUT` (0x12) でセッション全体を閉じる。moqt-js の publisher が catalog の Subgroup を
FIN せずに開いたままにする相手と組み合わせると、moq-sub は catalog を 1 つ受信した約 30 秒後に映像・音声を
含むセッションごと切れる。映像は直前まで届いており (実測で 1200 フレームまで描画)、1 本の stream の待ち状態を
理由に配信全体が終わる。

draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT は
「The session was closed because the peer took too long to send data expected on an open Data Stream」と
定義しつつ、open な subgroup stream で新しい object header を待って timeout した場合の措置を
"it MAY send a STOP_SENDING on that stream or terminate the subscription" とする。セッション全体の
クローズは現行 draft の文面には無い。draft-21 までは "or close the session with an error" が併記されて
いたが、moq-transport の issue 838 の議論で実装裁量 (RFC 9114 の "An endpoint MAY choose to treat a
stream error as a connection error under certain circumstances") に委ねる形で文面から外された経緯がある。

moqt-js 側の Subgroup を FIN しない不備は moqt-js で直す。本 issue は、その不備が直っても「相手が 1 本の stream を開いたままにする」だけで配信全体が落ちる現状の挙動を、このままでよいかを決めるために起票する。

## 現状

- `src/session/goaway.rs` の `Session::tick` は `data_stream_last_activity_ms` の全エントリを走査し、`now - last_activity >= data_stream_timeout_ms` のエントリが 1 つでもあれば `Session::fail` を呼ぶ。`Session::fail` は state を `Closing` にし、`SessionEvent::CloseSession` を積む。stream 単位・購読単位で救う分岐は無い。
- activity を更新するのは Data Stream 種別の受信 (`recv_data_stream_type`。Padding を除く) / `recv_subgroup_header` / `recv_subgroup_object` / fetch header / fetch entry の受信であり、activity の起点は「最後に header か object header を受け取った時刻」である。payload の受信では更新しない。
- stream は FIN / RESET (`recv_data_stream_closed`) / `send_data_stream_stop_sending` で `data_stream_last_activity_ms` から外れ、timeout の対象外になる。
- 既に stream 単位で会計から外す API はある。`Session::send_data_stream_stop_sending` は `data_streams.incoming` から stream を除去し `data_stream_last_activity_ms` から外す。ただし SessionEvent は積まず、wire の STOP_SENDING も送らない (Session 内の会計だけを閉じる)。0247 では、catalog の読み取りに失敗した stream を例側がこの API で会計から外すようにした。
- `SessionEvent` に data stream 用の STOP_SENDING イベントは無い (`StopSendingRequestStream` は bidi request stream 用)。wire の STOP_SENDING は `transport::RecvStream::stop_sending` で送れるが、data stream の経路からは呼んでいない。
- ただし、相手が正しく動作していて「FIN も次の object header も送らないだけ」の場合、アプリは待ち続ける (`examples/moq-sub/src/pipeline.rs` の `read_catalog_objects` は `receive_registered_stream_data` の `stream.receive_chunk()` で止まる)。この待ちには期限が無く、`send_data_stream_stop_sending` を呼ぶ契機も無い。Session の timeout だけが発火する。
- `examples/moq-sub/src/pipeline.rs` と `examples/moq-pub/src/pipeline.rs` は `set_data_stream_timeout_ms(Some(30000))` を固定値で設定し、CLI からは変更できない。
- subscription 単位の timeout (`tick_subscription_timeouts`) は delivery timeout と PUBLISH_DONE の drain を
  扱う経路であり、DATA_STREAM_TIMEOUT とは別である。`Subscription::delivery_timeouts` の期限到達は stream
  単位の `SessionEvent::ResetDataStream` を積み、`Subscription::expires` は `DeadlineTimer` を進めるだけで
  期限到達時の処理を持たない。DATA_STREAM_TIMEOUT に相当する購読単位の設定は無い。
- 参考: `examples/moq-pub/src/catalog.rs` の `publish_catalog_object` は `SubgroupWriter::finish` で FIN を送るため、moqt-rs の publisher はこの症状を起こさない。

## 設計方針

次のいずれかを選び、選んだ方針と根拠を issue の解決方法に残す。

- 案 A: timeout した stream を会計から外し、セッションは閉じない。`Session::send_data_stream_stop_sending` と同じ後始末を `Session::tick` から起動する。購読そのものは生かし、次の Group の stream が来れば受信を続ける。1 本の stream の timeout がセッション全体に波及しなくなる。
  - 注意: 現行の `send_data_stream_stop_sending` は wire の STOP_SENDING を送らず Session 内の会計だけを閉じる。draft §12.2 の MAY が示す「その stream へ STOP_SENDING を送る」まで行うなら、`transport::RecvStream::stop_sending` を呼ぶ経路 (`SessionEvent` の追加またはアプリからの呼び出し) が別途必要になる。wire を送らずに会計だけ外すと、相手は stream を開いたままになる。
- 案 B: timeout 時に当該購読を終える (`RequestTerminated` 相当) に留める。stream は購読に紐づくため、影響を購読の範囲に閉じ込める。
- 案 C: `data_stream_timeout_ms` を購読単位の設定としても持たせ、アプリが「セッション全体を閉じる」を選べるようにする。
- 案 D: 現行の挙動を意図的な設計として維持する。DATA_STREAM_TIMEOUT は「相手が data stream を送るのをやめた」ことの唯一の検知手段であり、moq-sub の例では他に検知できない。この場合は、根拠 (draft §12.2 の位置づけと RFC 9114 の実装裁量) を `src/session/goaway.rs` のコメントに明記し、moq-sub が切れる理由を利用者が追えるようにする (ログの文言を含む)。現状のタイムアウト理由文字列は "data stream timeout expired" のみで、どの stream のどの待ちが期限に達したかはログから分からない。

案 A は「相手が data stream を送るのを完全にやめた」場合の検知を失う点に注意する。購読単位の生存確認は CONTROL_MESSAGE_TIMEOUT と PUBLISH_DONE が担うため成立するが、この整理をコメントに残す。案 A と案 B は `tests/test_session/timeout_api.rs` の `stopped_subgroup_stream_is_excluded_from_data_stream_timeout` と同じ形で、timeout 後にセッションが `Established` のままであることをテストで固定できる。

## 完了条件

- 案 A / B / C / D のどれを採るかを決め、決定と根拠を issue の解決方法に記録すること。
- 採った案が `tests/test_session/timeout_api.rs` の単体テストで固定されていること (例: 案 A なら、timeout した stream が会計から外れ、セッションが `Established` のままであること)。
- 案 A / B / C の場合、moqt-js の publisher と moq-sub の組み合わせで、catalog を受信した約 30 秒後に `0x12 data stream timeout expired` でセッションが閉じないこと (実 relay での確認)。
- 案 D の場合、切れる理由 (どの stream の何の待ちが期限に達したか) がログから追えること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- ソースコードのコメントに issue 番号や issue への言及を書かないこと。

## 解決方法

{対応時に記入}
