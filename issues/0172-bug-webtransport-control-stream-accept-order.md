# WebTransport 経路で制御ストリームの受信が take_uni_receiver の後に accept するため確立できない

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-control-stream-accept-order
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::establish_wt` が WebTransport 経路で必ず失敗する問題を解消する。

`issues/pending/0094` が解消して `WtClient::connect` が成功するようになると、この失敗が接続直後の `Fatal: WebTransport: stream closed` として表面化する。`issues/pending/0063` は「確立前後の datagram を失わない」と「`feed_datagram` の drain 分離」を残課題として挙げているが、本 issue の受信順序は含まれていない。

## 現状

- `MoqtClient::establish_wt` は `WtSession::take_uni_receiver` で単方向受信ストリームの receiver を取り出した後に `WtSession::accept_uni_stream` を呼ぶ。
- `WtSession::accept_uni_stream` は `self.uni_rx.as_mut().ok_or(TransportError::StreamClosed)?` で receiver を取るため、`take_uni_receiver` の後は必ず `Err(StreamClosed)` になる。
- `establish_wt` はこの戻り値を `ControlStream::new` に渡して MOQT の制御ストリームを読むため、確立処理全体が失敗する。
- `examples/moqt-transport/src/webtransport.rs` の `route_uni_stream` は自セッションの WebTransport 単方向ストリームを `ForwardToMoqt` で acceptor 側の receiver へ流す。relay の MOQT 制御ストリームはこの経路で届く。
- `examples/moqt-subscriber/src/pipeline.rs` の受信ループは acceptor の receiver から届いたストリームを `peek_stream_type` で判別し、未知の stream type を Subgroup (データストリーム) として扱う。制御ストリームがこの receiver に混ざると誤解釈する。

## 設計方針

- 制御ストリームを session 側で受け取ってから、単方向受信ストリームの receiver を acceptor へ渡す順序に直す。
- acceptor 側の receiver に制御ストリームを流さない (流れた場合はデータストリームとして解釈しない) ようにする。
- WebTransport セッションの確立自体は `issues/pending/0094` (s2n-quic の RESET_STREAM_AT 非対応) の解消待ちである。本 issue は受信順序の修正のみを対象とし、確立の実機確認は 0094 の解消後に draft-15 相当の peer で行う。

## 完了条件

- `MoqtClient::establish_wt` が制御ストリームを `take_uni_receiver` より前に受け取る順序になっていること
- 制御ストリームが acceptor 側の receiver に流れず、データストリームとして解釈されないこと
- 順序の判断を純関数に切り出せる範囲は単体テストで固定し、I/O ハンドルが必要な範囲はレビューで確認すること
- 0094 の解消後に `https://` の relay へ接続して MOQT の SETUP が成立することを実機で確認すること
- `make test` / `make clippy` / `make fmt` が通ること
