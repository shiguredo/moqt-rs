# DATA_STREAM_TIMEOUT でセッション全体を閉じる挙動の扱いを決める

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/change-data-stream-timeout-scope
- Polished: 2026-10-13

## 目的

`Session::tick` は `data_stream_timeout_ms` の期限を超えて activity が更新されていない Data Stream が 1 本でもあると、
`SESSION_DATA_STREAM_TIMEOUT` (0x12) でセッション全体を閉じる。moqt-js の publisher が catalog の Subgroup を
FIN せずに開いたままにする相手と組み合わせると、moq-sub は catalog を 1 つ受信した約 30 秒後に映像・音声を
含むセッションごと切れる。映像は直前まで届いており (利用者からの報告では 1200 フレームまで描画)、1 本の
stream の待ち状態を理由に配信全体が終わる。

draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) の DATA_STREAM_TIMEOUT は
「The session was closed because the peer took too long to send data expected on an open Data Stream」と
定義しつつ、open な subgroup stream で新しい object header を待って timeout した場合の措置を
"it MAY send a STOP_SENDING on that stream or terminate the subscription" とする。この文面が挙げる措置に
セッション全体のクローズは無い。ただし 0x12 自体がセッション終了コードであり、§12.2 の導入も
"When terminating the Session (Section 6.6), the application MAY use any error message and SHOULD use a
relevant code" と述べる。さらに §6.6 (Termination) は "An endpoint MAY choose to treat a subscription or
request specific error as a session error under certain circumstances, closing the entire session in response
to a condition with a single subscription or message. Implementations need to consider the impact on other
outstanding subscriptions before making this choice." と述べ、単一購読の条件をセッションエラーに昇格させる
ことを実装裁量として認めている。draft-ietf-moq-transport-10 までは 0x12 の説明に
"or close the session with an error" が併記されていたが、moq-transport の issue 838 (2025-03-24 起票 /
2025-04-04 クローズ) の議論を経て draft-11 で削除され、同じ変更で §6.6 の上記の文が追加された。これは
RFC 9114 §8 の "An endpoint MAY choose to treat a stream error as a connection error under certain
circumstances, closing the entire connection in response to a condition on a single stream. Implementations
need to consider the impact on outstanding requests before making this choice." と同じ整理である。

moqt-js 側の Subgroup を FIN しない不備は moqt-js で直す。本 issue は、その不備が直っても
「相手が 1 本の stream を開いたままにする」だけで配信全体が落ちる現状の挙動を、このままでよいかを決めるために起票する。

## 現状

- `src/session/goaway.rs` の `Session::tick` は `data_stream_last_activity_ms` の全エントリを走査し、
  `now_ms - last_activity >= data_stream_timeout_ms` のエントリが 1 つでもあれば `Session::fail` を呼ぶ。
  `Session::fail` は state を `Closing` にし、`SessionEvent::CloseSession` を積む。stream 単位・購読単位で
  救う分岐は無い。
- activity を更新するのは Data Stream 種別の受信 (`recv_data_stream_type`。Padding を除く) /
  `recv_subgroup_header` / `recv_subgroup_object` / fetch header / fetch entry の受信であり、activity の
  起点は「最後に stream header か object header を受け取った時刻」である。payload の受信では更新しない。
- stream は FIN / RESET (`recv_data_stream_closed`) / `send_data_stream_stop_sending` で
  `data_stream_last_activity_ms` から外れ、timeout の対象外になる。
- 既に stream 単位で会計から外す API はある。`Session::send_data_stream_stop_sending` は
  `data_streams.incoming` から stream を除去し `data_stream_last_activity_ms` から外す。subgroup stream では
  `SessionEvent` を積まず、wire の STOP_SENDING も送らない (Session 内の会計と `peer_subgroups` の
  `mark_stop_sending` だけを更新する)。FETCH stream では、通常の FETCH 応答 stream は
  `send_fetch_stop_sending` に dispatch して bidi request stream 用の `SessionEvent::ResetRequestStream` を
  積み、`fetches` に entry を持たない fill fetch stream は `close_incoming_stream_or_fail` に落ちてイベントを
  積まない (`Terminated` の fetch は `Ok(())`)。0247 では、catalog の読み取りに失敗した stream を例側が
  この API で会計から外すようにした。
- `send_data_stream_stop_sending` の subgroup 経路は tombstone を残さない。除去後に同じ stream へ
  FIN / RESET が届くと `recv_data_stream_closed` が、object が届くと `recv_subgroup_object` が
  `SESSION_PROTOCOL_VIOLATION` ("unknown stream id") を返す。遅延到着を no-op で吸収する既存機構として
  `register_discarded_stream` (stream を `IncomingDataStream::Discarded` に置き換えて
  `data_stream_last_activity_ms` から外し、以後のデータと終端を吸収する) と、終端後に id を保持する
  `retain_discarded_stream_id` がある。
- `SessionEvent` に data stream 用の STOP_SENDING イベントは無い (`StopSendingRequestStream` は bidi
  request stream 用)。wire の STOP_SENDING は example の I/O 層 (`examples/tokio-moq/src/transport.rs` の
  `RecvStream::stop_sending`) で送れるが、library に `transport` モジュールは無く、data stream の経路からは
  呼んでいない。
- ただし、相手が正しく動作していて「FIN も次の object header も送らないだけ」の場合、アプリは待ち続ける
  (`examples/moq-sub/src/pipeline.rs` の `read_catalog_objects` は `receive_registered_stream_data` の
  `stream.receive_chunk()` で止まる)。この購読経路の待ちには期限が無く、`send_data_stream_stop_sending` を
  呼ぶ契機も無い (起動時のカタログ取得は `receive_catalog` が `CATALOG_TIMEOUT` (5 秒) で包み、期限切れで
  同 API を呼ぶ)。Session の timeout だけが発火する。
- `examples/moq-sub/src/pipeline.rs` と `examples/moq-pub/src/pipeline.rs` は
  `set_data_stream_timeout_ms(Some(30000))` を固定値で設定し、CLI からは変更できない。
- subscription 単位の timeout (`tick_subscription_timeouts`) は delivery timeout と PUBLISH_DONE の drain を
  扱う経路であり、DATA_STREAM_TIMEOUT とは別である。`Subscription::delivery_timeouts` の期限到達は stream
  単位の `SessionEvent::ResetDataStream` を積み、`Subscription::expires` は `DeadlineTimer` を進めるだけで
  期限到達時の処理を持たない。DATA_STREAM_TIMEOUT に相当する購読単位の設定は無い。
- CONTROL_MESSAGE_TIMEOUT の deadline は request_id 単位で制御メッセージの応答待ちにだけ張られ、応答で
  解除される (`Session::start_control_message_deadline` / `Session::clear_control_message_deadline`)。
  PUBLISH_DONE は publisher が送る終端通知であり、§5.2 (Delivery Timeouts and Data Reliability) の delivery
  timeout は publisher 側の送信時検査である。いずれも Established 購読のデータ途絶を検知する手段では
  なく、DATA_STREAM_TIMEOUT が「相手が data stream を送るのをやめた」ことの唯一の検知手段になっている。
- 参考: `examples/moq-pub/src/catalog.rs` の `publish_catalog_object` は `SubgroupWriter::finish` で FIN を送る
  ため、moqt-rs の publisher はこの症状を起こさない。

## 設計方針

次のいずれかを選び、選んだ方針と根拠を issue の解決方法に残す。根拠には、現行挙動が §6.6 の MAY の範囲内で
あることと、§12.2 が挙げる措置が STOP_SENDING と購読終了であることを含める。

- 案 A: timeout した stream を会計から外し、セッションは閉じない。購読そのものは生かし、次の Group の
  stream が来れば受信を続ける。
  - 変更対象は `Session::tick` と、破棄対象 stream を吸収する既存機構 (`register_discarded_stream` /
    `IncomingDataStream::Discarded` / `retain_discarded_stream_id`)。
  - `send_data_stream_stop_sending` と同じ「`data_streams.incoming` から除去して activity を消す」だけの
    後始末は使わない。tombstone が残らず、timeout 後に同じ subgroup stream へ届く header / object /
    FIN / RESET が `SESSION_PROTOCOL_VIOLATION` になるため、遅延到着を吸収できる経路を使う。
  - `data_stream_last_activity_ms` には subgroup stream と FETCH stream (fill fetch を含む) の両方が入る。
    `IncomingDataStream::Discarded` が吸収するのは subgroup の遅延到着だけであり、FETCH stream では
    `recv_fetch_header` / `recv_fetch_entry` が `SESSION_PROTOCOL_VIOLATION` を返す。次のいずれかを決めて
    解決方法に記録すること。
    - 対象を subgroup stream に限定する。tick の走査で stream 種別を判別して FETCH stream を除外し、
      FETCH stream は現行どおり 0x12 でセッションを閉じる。
    - FETCH stream も会計から外す。この場合は FETCH stream の遅延到着を no-op で吸収する経路を追加する。
  - wire の STOP_SENDING を送るかは実装時に決め、解決方法に記録する。送る場合は `SessionEvent` の追加
    (公開 enum の破壊的変更) と `examples/tokio-moq/src/moqt_client.rs` の I/O 配線、載せる reset error code
    の決定が必要で、変更対象が example まで広がる (§12.5 に DATA_STREAM_TIMEOUT 相当のコードは無く、最も
    近い DELIVERY_TIMEOUT は §5.2 の別機構である)。送らない場合は相手が stream を開いたままになり、
    待ち続けているアプリ (例: `read_catalog_objects`) は自前の期限で抜ける必要がある。
  - DATA_STREAM_TIMEOUT を外すと「相手が data stream を送るのを完全にやめた」ことを Session が検知しなく
    なる。この検知を代替する仕組みは現状無い (上の「現状」を参照)。この損失を許容する根拠を解決方法に
    書くこと。
- 案 B: timeout 時に当該購読を終える (`RequestTerminated` 相当) に留める。
  - stream が購読に紐づかない場合 (`IncomingDataStream::AwaitingHeader` は stream type だけ受信して header を
    待っている状態で、購読が未確定) の扱いを決めること。
  - 当該 stream を会計から外さないと次の tick で毎回 timeout するため、終了後の後始末 (破棄対象として
    吸収するか、購読ごと除去するか) を決めること。
  - peer に購読終了を伝える手段を決めること。既存の `Session::stop_sending` は state を `Terminated` に
    するだけでイベントを発行せず、QUIC の STOP_SENDING は I/O 層が実行する前提である。
    `SessionEvent::StopSendingRequestStream` を tick から発行するのか、`TerminationReason` をどうするのかを
    解決方法に記録すること。
- 案 C: `data_stream_timeout_ms` を購読単位の設定としても持たせ、アプリが「セッション全体を閉じる」を
  選べるようにする。
  - 設定の API 形状 (Session の setter か SUBSCRIBE parameter か。SUBSCRIBE parameter は draft に無い独自
    拡張になり、CODEBASE.md の「最新ドラフトに準拠すること」と衝突する) と既定値を決めること。
  - activity は stream 単位で保持され、stream は subscription 間を移管しうる。購読単位の timeout をどの
    activity で判定するかを決めること。
  - timeout した stream をアプリへ通知する経路 (案 A と同じ `SessionEvent` の追加判断) を決めること。
- 案 D: 現行の挙動を意図的な設計として維持する。
  - 根拠 (§12.2 の 0x12 の定義、§6.6 の MAY) を `src/session/goaway.rs` のコメントに明記すること。
  - 切れる理由をアプリが追えるようにすること。`SessionError::reason` は `&'static str` で、Session は
    ログを出さない (sans-I/O)。動的な stream id を載せるには公開型の破壊的変更か新しい通知経路
    (`SessionEvent` の追加、accessor など) が必要になるため、手段と変更対象を解決方法に記録すること。

## 完了条件

- 案 A / B / C / D のどれを採るかを決め、決定と根拠を issue の解決方法に記録すること。
- 採った案の挙動が `tests/test_session/timeout_api.rs` の単体テストで固定されていること。
  - 案 A / 案 B では、timeout した stream (または購読) が `data_stream_last_activity_ms` の監視から外れ、
    セッションが `Established` のままであること。
  - 案 A では、timeout 後に同じ stream へ header / object / FIN / RESET が届いても
    `SESSION_PROTOCOL_VIOLATION` にならないこと。FETCH stream を対象外にした場合は、その判断と理由を
    解決方法に記録し、FETCH stream が現行どおり 0x12 で閉じることを単体テストで固定すること。
  - 案 C では、購読単位の設定で「セッション全体を閉じる」を選んだ場合に 0x12 で閉じ、閉じない設定では
    timeout した stream が監視から外れてセッションが `Established` のままであることを固定すること。
  - 案 D では、どの stream の何の待ちが期限に達したかをアプリが特定できること。
- `SessionEvent` を追加する場合は、`examples/tokio-moq/src/moqt_client.rs` の配線を更新し、example が
  新しいイベントを取りこぼさないこと。
- 案 A / 案 B / 案 C (セッションを閉じない設定) を採る場合、moqt-js の publisher と moq-sub の組み合わせ
  (catalog を受信した約 30 秒後に `0x12 data stream timeout expired` でセッションが閉じないこと) は、
  relay と未修正の moqt-js publisher を用意できる場合にのみ実 relay で確認する。本リポジトリは relay を
  持たず (CODEBASE.md)、`e2e-tests` の publisher は moqt-rs 実装 (`publish_catalog_object` が FIN を送る)
  ため、この組み合わせは CI でも再現しない。
  実 relay で確認できなかった場合は、その旨と、単体テストと example の配線レビューで代替した範囲を
  解決方法に記録すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードのコメントに issue 番号や issue への言及を書かないこと。

## 解決方法

{対応時に記入}
