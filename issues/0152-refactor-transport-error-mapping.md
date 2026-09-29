# トランスポートエラーの表示振り分けを網羅 match にする

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-transport-error-mapping
- Polished: 2026-09-28

## 目的

publisher / subscriber のエラー表示がトランスポート種別と一致し続けるようにする。`From<TransportError>` の catch-all が新しい variant を黙って WebTransport 表示に流すため、QUIC 由来の variant を追加したときに誤表示へ気づけない。

## 現状

`examples/moq-pub/src/error.rs` と `examples/moq-sub/src/error.rs` の `From<tokio_moq::error::TransportError> for Error` は `Quic` / `Internal` / `InvalidAuthority` / `ResolutionFailed` / `ConnectionClosed` を明示し、残りを `other => Self::WebTransport(other.to_string())` で受ける。

`TransportError` (`examples/tokio-moq/src/error.rs`) の variant は `Quic` / `InvalidAuthority` / `ResolutionFailed` /
`Internal` / `ConnectionClosed` / `Transport` / `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` /
`StreamClosed` / `InvalidState` の 13 個である。catch-all に落ちる `Transport` / `Http3` / `Http2` / `WtH2` /
`ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` / `InvalidState` は WebTransport 経路
(`examples/tokio-moq/src/webtransport_h3.rs` / `webtransport_h2.rs` / `transport.rs` の `WtH3` / `WtH2` arm) が生成するため、
現在は `WebTransport:` 表示になって偶然一致している。QUIC 経路 (`examples/tokio-moq/src/quic.rs`、`transport.rs` の `Quic` arm、
`moqt_client.rs` の `establish_quic`) は QUIC 固有の失敗を `TransportError::Quic` に畳む実装になっている。

なお `ConnectionClosed` は WebTransport 経路のみが生成し、app 側の `Error::ConnectionClosed` へ明示的に振り分けられている。publisher / subscriber の `is_transport_session_end` (`examples/moq-pub/src/pipeline.rs` / `examples/moq-sub/src/pipeline.rs`) がこの variant でセッション終了を判定しているため、表示文字列には依存しない。

両 error.rs には `transport_error_mapping_*` テストがあるが、個別の variant の写像を 1 つずつ列挙していないため、catch-all を残したままでも通る。そのため QUIC 経路で `Transport` や `StreamClosed` を返す実装を追加しても、表示は `WebTransport:` のままでコンパイルもテストも通る。

## 設計方針

- `TransportError` の全 variant を明示的に列挙し、catch-all を置かない。variant を追加したときにコンパイルエラーで写像の検討を強制する
- トランスポート種別に依存しない失敗 (`InvalidAuthority` / `ResolutionFailed` / `Internal`) は `Other` にして接頭辞を付けない現状の扱いを維持する
- `ConnectionClosed` は `Error::ConnectionClosed` へ振り分けて `WebTransport:` を付けない現状の扱いを維持する (`is_transport_session_end` が variant で判定しているため)
- `Transport` / `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` / `InvalidState` は WebTransport 経路の表示にする。QUIC 経路だけが生成する variant は `Quic` の 1 つであり、その写像の根拠をコメントに残す
- publisher / subscriber は同じ写像を持ち続ける。両方に同じ変更を入れる

## 完了条件

- 両バイナリの `From<TransportError>` が全 variant を網羅し、catch-all が無いこと
- `TransportError` に variant を足すと両バイナリがコンパイルエラーになること
- 現在の表示 (`Quic` は `QUIC:`、`InvalidAuthority` / `ResolutionFailed` / `Internal` は接頭辞なし、`ConnectionClosed` は `session closed`、`Transport` / `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` / `InvalidState` は `WebTransport:`) が変わらないこと
- 全 variant の写像先を列挙して固定するテストが両バイナリにあり、variant の追加時にテストの更新が必要になること
