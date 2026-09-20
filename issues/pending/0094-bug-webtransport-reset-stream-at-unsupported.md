# s2n-quic が RESET_STREAM_AT 非対応で WebTransport over HTTP/3 経路が draft-16 に準拠できない

- Created: 2026-09-16
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-settings-order
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 は WebTransport over HTTP/3 を実装する端点に対し、
`reset_stream_at` transport parameter (draft-ietf-quic-reliable-stream-reset) の広告と
RESET_STREAM_AT の使用を要求する。

> WebTransport over HTTP/3 relies on the RESET_STREAM_AT frame defined in
> [RESET-STREAM-AT]. To indicate support, both the client and the server enable the
> extension by sending an empty reset_stream_at transport parameter as described in
> Section 3 of [RESET-STREAM-AT].

> To prevent this, WebTransport implementations MUST use the RESET_STREAM_AT frame
> [RESET-STREAM-AT] with a Reliable Size set at least the size of the WebTransport header
> when resetting a WebTransport data stream.

`examples/moqt-transport` の WebTransport 経路は s2n-quic を使うが、s2n-quic は
RESET_STREAM_AT を実装していないため、draft-16 の WebTransport 実装として準拠できない。

## 現状

- `examples/moqt-transport` の QUIC 実装は s2n-quic 1.88.0 (2026-09 時点の最新) である。
- s2n-quic / s2n-quic-core / s2n-quic-transport のいずれにも RESET_STREAM_AT の実装が無い
  (フレーム種別 0x24 も `reset_stream_at` transport parameter も存在しない)。
- `shiguredo_http3` の `Connection::set_webtransport_transport_verified` の第 2 引数は
  「peer が reset_stream_at を広告しているか」であり、peer が広告していても s2n-quic 側で
  transport parameter を解析できないため、アプリケーションからは検証できない。
- 相手の MOQT relay は `wt_enabled` (draft-16 相当) を広告するため
  `DraftVersion::requires_reset_stream_at()` が真になり、`false` を注入すると
  `WtSetupError::ResetStreamAtNotSupported` で CONNECT が拒否される。
- 上流 (aws/s2n-quic) に RESET_STREAM_AT 対応の issue / PR は存在しない
  (2026-09-16 時点で検索して 0 件)。

## 設計方針

- s2n-quic が RESET_STREAM_AT に対応するまでは、WebTransport over HTTP/3 経路は
  「SETTINGS と transport parameter の検証を正しく行ったうえで、draft-16 の
  reset_stream_at 要件で停止する」状態にする。対応を偽って主張しない
  (`set_webtransport_transport_verified(true, true)` は peer の capability を未検証のまま
  主張することになるため行わない)。
- 解消の選択肢は次のいずれかである。着手時に再評価する。
  1. s2n-quic が RESET_STREAM_AT を実装したら追随し、transport parameter の検証を
     s2n-quic の API から行う
  2. WebTransport 経路だけ別の QUIC 実装 (RESET_STREAM_AT 対応) に差し替える
  3. 相手が draft-15 以前の WebTransport を広告する場合は `reset_stream_at` が必須ではないため、
     その組み合わせでは接続できる (現状の relay は draft-16 相当を広告する)
- 生 QUIC 経路 (`moqt://`) は RESET_STREAM_AT を要求しないため影響を受けない。

## 完了条件

- WebTransport over HTTP/3 経路で MOQT セッションが確立し、catalog の FETCH と映像受信が
  成立すること

## pending にする理由

s2n-quic が RESET_STREAM_AT (draft-ietf-quic-reliable-stream-reset) を実装するまで、
アプリケーション側では解消できない。依存先の実装待ちであるため pending とする。
s2n-quic の新バージョンで対応した場合、または WebTransport 経路の QUIC 実装を差し替える
判断をした場合に reopened にする。
