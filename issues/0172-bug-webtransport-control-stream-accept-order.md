# WebTransport 経路で制御ストリームの受信が take_uni_receiver の後に accept するため確立できない

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-control-stream-accept-order
- Polished: 2026-09-27

## 目的

`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::establish_wt` が WebTransport 経路で必ず失敗する問題を解消する。

`issues/pending/0094` が解消して `WtClient::connect` が成功するようになると、この失敗が接続直後の `Fatal: WebTransport: stream closed` として表面化する。`issues/pending/0063` は「確立前後の datagram を失わない」と「`feed_datagram` の drain 分離」を残課題として挙げているが、本 issue の受信順序は含まれていない。

## 現状

- `MoqtClient::establish_wt` は `WtSession::take_uni_receiver` で単方向受信ストリームの receiver を取り出した後に `WtSession::accept_uni_stream` を呼ぶ。
- `WtSession::accept_uni_stream` は `self.uni_rx.as_mut().ok_or(TransportError::StreamClosed)?` で receiver を取るため、`take_uni_receiver` の後は必ず `Err(StreamClosed)` になる。
- `establish_wt` はこの戻り値を `ControlStream::new` に渡して MOQT の制御ストリームを読む想定だが、`accept_uni_stream` の `?` が先に `TransportError::StreamClosed` を返すため、確立処理がこの段階で終了する。
- `examples/moqt-transport/src/webtransport.rs` の `route_uni_stream` は自セッションの WebTransport 単方向ストリームを `ForwardToMoqt` で acceptor 側の receiver へ流す。relay の MOQT 制御ストリームはこの経路で届く。
- `examples/moqt-subscriber/src/pipeline.rs` の受信ループは acceptor の receiver から届いたストリームを `peek_stream_type` で判別し、`classify_data_stream_type` が `None` を返す type (`SETUP_STREAM_TYPE` を含む) を
  `StreamType::Subgroup` にフォールバックする。制御ストリームがこの receiver に混ざると Subgroup (データストリーム) として扱われ、`recv_data_stream_type` が未知型として `PROTOCOL_VIOLATION` でセッションを閉じる。

## 設計方針

- 制御ストリームを session 側で受け取ってから、単方向受信ストリームの receiver を acceptor へ渡す順序に直す。`uni_rx` は MOQT の制御ストリームとデータストリームを区別せずに運ぶため、受け取る最初のストリームを制御ストリームとみなす識別は到着順に依存する。draft-ietf-moq-transport-21 §6.3 はデータストリームが制御ストリームより先に届き得ることを述べるが (その場合の事前データのバッファリングは未対応の既知の制限)、対象とする接続順では先に制御ストリームが届くため到着順依存で扱う。
- acceptor 側の receiver に制御ストリームを流さない (流れた場合はデータストリームとして解釈しない) ようにする。
- WebTransport セッションの確立自体は `issues/pending/0094` (s2n-quic の RESET_STREAM_AT 非対応) の解消待ちである。本 issue は受信順序の修正のみを対象とし、確立の実機確認は 0094 の解消後に `https://` の relay (draft-16 相当) への接続で行う (「完了条件」参照)。0094 解消までは draft-15 以前を広告する peer との組み合わせでのみ接続できるが、現状の relay は draft-16 相当を広告するため実機確認できない。

## 完了条件

- `MoqtClient::establish_wt` が制御ストリームを `take_uni_receiver` より前に受け取る順序になっていること
- 制御ストリームが acceptor 側の receiver に流れず、データストリームとして解釈されないこと
- 制御ストリームの判別 (先頭の stream type が `SETUP_STREAM_TYPE` か) は既存の純関数と単体テスト (`Session::recv_control_stream_type` と `tests/test_session/timeout_api.rs`) にあり、新規に導入される判定があれば単体テストで固定すること。変更される take / accept の順序と待機中のロックの扱いは I/O ハンドルが必要な範囲であり、レビューで確認すること
- 0094 の解消後に `https://` の relay へ接続して MOQT の SETUP が成立することを実機で確認すること
- `make test` / `make clippy` / `make fmt` が通ること
