# WebTransport 経由の datagram 受信を機能させる

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-datagram-receive
- Polished: 2026-09-11

## 目的

`examples/README.md` が謳う「datagram 受信対応」を WebTransport 接続でも成立させる。現状は WebTransport 経由の Object Datagram が subscriber に届かない。

## 現状

`examples/moqt-transport/src/webtransport.rs` の `ClientConnectionState::feed_datagram` が `h3_conn.feed_datagram` の直後に `h3_conn.drain_events()` を呼び、H3 イベントキューを空にする。背景受信タスク (`webtransport.rs`) は返ってきたイベントを `tracing::debug!` するだけで破棄する。後から `WtSession::take_buffered_datagrams` が再度
`drain_events` してもキューは空で、`StreamHandle::recv_datagrams` (`examples/moqt-transport/src/transport.rs`) は WebTransport 分岐で常に空 `Vec` を返す。

QUIC 直結 (`moqt://`) は `s2n-quic` の datagram receiver を直接ポーリングするため正常。欠陥は WebTransport 経路のみ。

再現手順:

1. subscriber を `https://` (WebTransport over HTTP/3) で起動する
2. publisher を `--use-datagram` で起動して Object Datagram を送る
3. subscriber の `data_plane.recv_datagram` が呼ばれない

根拠: 依存 `shiguredo_http3` の `drain_events` はイベントキューを空にする。`feed_datagram` 自体は push のみで drain しない。example 側のラッパーが drain を追加している点が誤り。

## 設計方針

背景タスクから `drain_events` を除去し、`feed_datagram` のみを呼ぶ。payload の取り出しは `take_buffered_datagrams` に一本化する。あるいは背景タスクが `Datagram { payload, .. }` を取り出して `tokio::sync::mpsc` で subscriber へ渡す。feed と drain を同一関数で行う現設計を分離する。

修正までの暫定として `examples/README.md` に「WebTransport の datagram 受信は未対応」を明記することも検討する。

## 完了条件

- WebTransport 接続の subscriber の `data_plane.recv_datagram` に Object Datagram の payload が届くこと
- QUIC 直結の既存挙動が壊れないこと
- 可能なら example の動作確認手順が `examples/README.md` に記載されていること

## 依存

- example publisher の `--use-datagram` は `ObjectDatagram.properties_data` に Properties Length を二重に前置する (0014)。Session はこの Properties を malformed track として扱い subscription を終端するため、Session まで含めた Object Datagram の受理確認は 0014 の修正後に行う。本 issue では datagram がアプリ層 (`data_plane.recv_datagram`) に届くことまでを確認する。

## pending にする理由

WebTransport セッションの確立自体が現在の依存クレート (shiguredo_http3 / s2n-quic) では成立しないことが判明した (0063)。CONNECT 前の SETTINGS 待ち、transport parameter 検証、確立前 datagram の保持が必要で、本 issue のスコープを超える。0063 の対応後に reopened して datagram 受信の確認を行う。
