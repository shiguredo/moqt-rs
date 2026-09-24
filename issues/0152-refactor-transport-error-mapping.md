# トランスポートエラーの表示振り分けを網羅 match にする

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-transport-error-mapping
- Polished: {YYYY-MM-DD}

## 目的

publisher / subscriber のエラー表示がトランスポート種別と一致し続けるようにする。`From<TransportError>` の catch-all が新しい variant を黙って WebTransport 表示に流すため、QUIC 由来の variant を追加したときに誤表示へ気づけない。

## 現状

`examples/moqt-publisher/src/error.rs` と `examples/moqt-subscriber/src/error.rs` の `From<moqt_example_transport::error::TransportError> for Error` は `Quic` / `Internal` / `InvalidAuthority` / `ResolutionFailed` を明示し、残りを `other => Self::WebTransport(other.to_string())` で受ける。

`TransportError` の残りの variant は `Transport` / `Http3` / `ConnectionClosed` / `ConnectFailed` / `StreamClosed` / `InvalidState` で、現時点ではすべて `examples/moqt-transport/src/webtransport.rs` が生成するため表示は偶然一致している。QUIC 経路 (`examples/moqt-transport/src/transport.rs`) は失敗を `TransportError::Quic` に畳む実装になっている。

そのため QUIC 経路で `Transport` や `ConnectionClosed` を返す実装を追加しても、表示は `WebTransport:` のままでコンパイルもテストも通る。

## 設計方針

- `TransportError` の全 variant を明示的に列挙し、catch-all を置かない。variant を追加したときにコンパイルエラーで写像の検討を強制する
- トランスポート種別に依存しない失敗 (`InvalidAuthority` / `ResolutionFailed` / `Internal`) は `Other` にして接頭辞を付けない現状の扱いを維持する
- `Transport` / `Http3` / `ConnectionClosed` / `ConnectFailed` / `StreamClosed` / `InvalidState` は WebTransport 経路の表示にする。QUIC 経路から返り得る variant は写像の根拠をコメントに残す
- publisher / subscriber は同じ写像を持ち続ける。両方に同じ変更を入れる

## 完了条件

- 両バイナリの `From<TransportError>` が全 variant を網羅し、catch-all が無いこと
- `TransportError` に variant を足すと両バイナリがコンパイルエラーになること
- 現在の表示 (`Quic` は `QUIC:`、`InvalidAuthority` / `ResolutionFailed` / `Internal` は接頭辞なし、`Http3` などは `WebTransport:`) が変わらないこと
- 写像を固定するテストが両バイナリにあり、variant の追加時にテストの更新が必要になること
