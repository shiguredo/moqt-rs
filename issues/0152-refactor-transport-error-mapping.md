# トランスポートエラーの表示振り分けを網羅 match にする

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-transport-error-mapping
- Polished: 2026-11-14
- Updated: 2026-11-14

## 目的

publisher / subscriber のエラー表示がトランスポート種別と一致し続けるようにする。
`From<TransportError>` の catch-all が新しい variant を黙って WebTransport 表示に流すため、
QUIC 由来の variant を追加したときに誤表示へ気づけない。また、複数の transport が生成する
`Transport` / `Unsupported` が catch-all で `WebTransport:` になり、実際の transport と異なる
表示になる。

## 現状

`crates/moq-pub/src/error.rs` と `crates/moq-sub/src/error.rs` の
`From<tokio_moq::error::TransportError> for Error` は `Quic` / `Internal` / `InvalidAuthority` /
`ResolutionFailed` / `ConnectionClosed` / `StreamReset` / `Moqt` / `Qmux` を明示し、残り 9 variant を
`other => Self::WebTransport(other.to_string())` で受ける。

`TransportError` (`crates/tokio-moq/src/error.rs`) の variant は `Quic` / `InvalidAuthority` /
`ResolutionFailed` / `Transport` / `Http3` / `Http2` / `WtH2` / `Qmux` / `ConnectionClosed` /
`StreamReset` / `Moqt` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` /
`InvalidState` / `Unsupported` / `Internal` の 17 個である。catch-all に落ちるのは `Transport` /
`Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` /
`InvalidState` / `Unsupported` の 9 個である。

- QUIC 経路 (`crates/tokio-moq/src/quic.rs`、`crates/tokio-moq/src/transport.rs` の
  `StreamHandle::Quic`、`crates/tokio-moq/src/moqt_client.rs` の `establish_quic`) は
  QUIC 固有の接続 / ストリーム失敗を `TransportError::Quic` に畳むが、接続終了は
  `TransportError::ConnectionClosed`、STOP_SENDING は `TransportError::StreamReset`、
  `RESET_STREAM_AT` 非対応は `TransportError::Unsupported` を返す
  (`crates/tokio-moq/src/transport.rs` の `SendStream::reset` / `reject_reliable_size`)。
- QMux 経路 (`crates/tokio-moq/src/qmux.rs` の `connect`、
  `crates/tokio-moq/src/transport.rs` の `StreamHandle::Qmux`、
  `crates/tokio-moq/src/moqt_client.rs` の `establish_qmux`) は接続確立前の TCP / TLS 失敗を
  `TransportError::Transport`、接続終了を `ConnectionClosed`、STOP_SENDING は `StreamReset`、
  `RESET_STREAM_AT` 非対応を `Unsupported`、それ以外を `Qmux` にする
  (`crates/tokio-moq/src/error.rs` の `TransportError::from_qmux`)。
- WebTransport 経路 (`crates/tokio-moq/src/webtransport_h3.rs` /
  `webtransport_h2.rs`、`crates/tokio-moq/src/transport.rs` の `StreamHandle::WtH3` /
  `WtH2`) は `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` /
  `StreamClosed` / `InvalidState` を生成し、下位 I/O の失敗は `Transport`、接続終了は
  `ConnectionClosed`、STOP_SENDING は `StreamReset`、`RESET_STREAM_AT` 非対応は `Unsupported`
  を返す。
- `Moqt` は全経路の encode / decode 失敗、`Internal` は全経路の内部エラー、
  `InvalidAuthority` / `ResolutionFailed` はトランスポート種別に依存しない URL / 名前解決の
  失敗で生成される。

そのため catch-all は `Transport` と `Unsupported` を `WebTransport:` 表示にしているが、
`Transport` は QMux、`Unsupported` は QMux / QUIC でも生成される。QMux の接続確立時の
TCP / TLS 失敗は `WebTransport: transport error: ...`、QUIC 経路で `RESET_STREAM_AT` を
要求した場合の `Unsupported` は `WebTransport: unsupported: ...` になり、トランスポート種別と
一致しない。`Qmux` は明示的に `Error::Other(format!("QMux: {e}"))` へ振り分けている。

`ConnectionClosed` は WebTransport 経路だけでなく QUIC / QMux の接続終了でも生成されるが、
いずれも `Error::ConnectionClosed` に振り分けられている。publisher / subscriber の
`is_transport_session_end` (`crates/moq-pub/src/pipeline.rs` / `crates/moq-sub/src/pipeline.rs`)
がこの variant でセッション終了を判定しているため、表示文字列には依存しない。

両 error.rs には `transport_error_mapping_*` テストが 5 本ずつあり、`Quic` / `InvalidAuthority` /
`ResolutionFailed` / `ConnectionClosed` / `StreamReset` / `Moqt` の写像は個別に固定しているが、
`Internal` / `Qmux` と catch-all に落ちる 9 variant の写像は 1 つずつ列挙していないため、
catch-all を残したままでも通る。そのため QUIC 経路で `Transport` や `StreamClosed` を返す実装を
追加しても、表示は `WebTransport:` のままでコンパイルもテストも通る。

## 設計方針

- `TransportError` の全 17 variant を明示的に列挙し、catch-all を置かない。variant を追加したときに
  コンパイルエラーで写像の検討を強制する
- トランスポート種別に依存しない失敗 (`InvalidAuthority` / `ResolutionFailed` / `Internal`) は
  `Other` にして接頭辞を付けない現状の扱いを維持する
- 複数の transport が生成する `Transport` / `Unsupported` は、特定の transport 名を付けず
  `Self::Other` に `TransportError` の `Display` 文字列をそのまま入れる。これらの variant は
  transport 種別を保持しないためアプリ側で経路を判別できず、現状は catch-all で `WebTransport:` に
  なっているが `Transport` は QMux、`Unsupported` は QMux / QUIC でも生成されるため表示が誤る。
  この 2 つは表示が変わる
- `ConnectionClosed` は `Error::ConnectionClosed` へ振り分けて `WebTransport:` を付けない現状の
  扱いを維持する (`is_transport_session_end` が variant で判定しているため)
- `StreamReset` は `Error::StreamReset` へ振り分ける現状の扱いを維持する (peer の reset を
  セッション終了と区別する)
- `Moqt` は `Error::Moqt` へ振り分ける現状の扱いを維持する
- `Qmux` は `Error::Other(format!("QMux: {e}"))` へ振り分ける現状の扱いを維持する
- `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` /
  `InvalidState` は WebTransport 経路だけが生成するため `WebTransport` 表示にする。QUIC 経路が
  生成する variant は `Quic` / `ConnectionClosed` / `StreamReset` / `Unsupported`、QMux 経路が
  生成する variant は `Qmux` / `ConnectionClosed` / `StreamReset` / `Transport` / `Unsupported`
  であり、その写像の根拠をコメントに残す
- `Transport` / `Unsupported` を `Other` に変えるため、`Error::WebTransport` に畳まれると書いた
  既存コメント (`crates/moq-pub/src/pipeline.rs` の `session_ended` 付近など) を実態に合わせて
  更新する
- publisher / subscriber は同じ写像を持ち続ける。両方に同じ変更を入れる

## 完了条件

- 両バイナリの `From<TransportError>` が全 17 variant を網羅し、catch-all が無いこと
- `TransportError` に variant を足すと両バイナリがコンパイルエラーになること
- 次の表示が変わらないこと
  - `Quic` は `QUIC:`
  - `InvalidAuthority` / `ResolutionFailed` / `Internal` は接頭辞なし
  - `ConnectionClosed` は `session closed`
  - `StreamReset` は `stream reset by peer (error code ...)`
  - `Moqt` は `MOQT:`
  - `Qmux` は `Error::Other(format!("QMux: {e}"))` による現状の表示
  - `Http3` / `Http2` / `WtH2` / `ConnectFailed` / `ProtocolNegotiationFailed` / `StreamClosed` /
    `InvalidState` は `WebTransport:`
- `Transport` / `Unsupported` は接頭辞なしの `Other` になり、`WebTransport:` が付かないこと
  (現状の catch-all から変わる)
- 全 17 variant の写像先と表示文字列を列挙して固定するテストが両バイナリにあり、variant の追加時に
  `From` の網羅 match がコンパイルエラーになってテストの更新が必要になること
- `Error::WebTransport` に畳まれると書いた既存コメントが、変更後の写像と一致していること
