# MSF fragment の connection を接続種別に反映する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-fragment-connection
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters) の予約パラメータ `connection` は
「mandates the client to use a particular connection type when connecting to the server. There are
two allowed values - "q" or "wt". "q" indicates that a Native QUIC connection MUST be used. "wt"
indicates that a WebTransport connection MUST be used.」と定める。fragment のパラメータは
client 側で処理する指定であり (§11.1)、example はこれを無視して `--transport` の既定で接続して
いるため MUST を満たしていない。

## 現状

- `examples/moq-pub/src/cli.rs` と `examples/moq-sub/src/cli.rs` は `--url` を
  `tokio_moq::parse_url` で解釈し、接続経路は `--transport` (既定 quic) だけで決める。
- `src/msf/uri.rs` の `MsfUri::connection_types` は `connection` を
  `MsfConnectionType` へ変換する実装が既にあり、`q` / `wt` 以外の値をエラーにする。しかし
  example からは呼ばれておらず、`--url` の `connection` は捨てられる。
- `examples/tokio-moq/src/lib.rs` には接続経路を URL ではなく `Transport` で選ぶ方針のコメントが
  あり、意図的に未対応のままになっている。
- 結果として `#msf:ns--catalog&connection=wt` を渡しても native QUIC で接続し、`connection=q` を
  渡しても `--transport wt-h3` で接続する。

## 設計方針

- `--url` の `connection` を `--transport` の既定として反映する。`q` は native QUIC、`wt` は
  WebTransport (`wt-h3` / `wt-h2` のどちらを既定にするかを決める) に対応させる。
- `--transport` を明示した場合の優先順位を決めて help と doc に書く (明示指定を優先するか、
  fragment と矛盾したらエラーにするか)。
- §11.1.1 は同じパラメータの複数指定について「the client MUST process the union of those ranges」
  を定めるが、接続は 1 つに定まる必要がある。`q` と `wt` が同時に指定された場合の扱いを決めて
  テストで固定する。
- 値の検証 (q / wt 以外の拒否) は `MsfUri::connection_types` に任せ、example 側で再実装しない。

## 完了条件

- `#msf:ns--catalog&connection=wt` で WebTransport、`connection=q` で native QUIC が選ばれること。
- `--transport` との優先規則と q / wt 併記時の扱いが doc・help・テストで固定されていること。
- moq-pub と moq-sub の両方で同じ規則になっていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
