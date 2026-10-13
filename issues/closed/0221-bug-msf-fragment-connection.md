# MSF fragment の connection を接続種別に反映する

- Created: 2026-10-09
- Completed: 2026-10-13
- Branch: feature/fix-msf-fragment-connection
- Polished: 2026-10-10
- Updated: 2026-10-11

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
- `src/msf/uri.rs` の `MsfFragment::connection_types` は `connection` を
  `MsfConnectionType` へ変換する実装が既にあり、`q` / `wt` 以外の値をエラーにする。
  `examples/tokio-moq/src/lib.rs` の `ServerUrl::msf` がこの `MsfFragment` を保持するが、
  example から `connection_types` は呼ばれておらず、`--url` の `connection` は捨てられる。
- `examples/tokio-moq/src/lib.rs` には接続経路を URL ではなく `Transport` で選ぶ方針のコメントが
  あり、意図的に未対応のままになっている。
- 結果として `#msf:ns--catalog&connection=wt` を渡しても native QUIC で接続し、`connection=q` を
  渡しても `--transport wt-h3` で接続する。

## 設計方針

- `--url` の `connection` を `--transport` の既定として反映する。`q` は native QUIC、`wt` は
  WebTransport (`wt-h3` / `wt-h2` のどちらを既定にするかを決める) に対応させる。
- `--transport` を明示した場合の優先規則を決めて help と doc に書く。`connection` は接続種別を
  MUST で指定するため「明示指定を優先する (fragment を無視する)」は選べない。fragment を優先するか、
  矛盾したらエラーにするかのいずれかにする。
- §11.1.1 は同じパラメータの複数指定について「the client MUST process the union of those ranges」を
  定めるが、この規則は値域 (range) を対象としたもので `connection` のような単一値の指定には
  そのままは当てはまらない。`q` と `wt` が同時に指定された場合は 2 つの MUST が両立しないため、
  エラーにするのが唯一の整合解である。この扱いを決めてテストで固定する。
- 値の検証 (q / wt 以外の拒否) は `MsfFragment::connection_types` に任せ、example 側で再実装しない。

## 完了条件

- `--transport` を指定しないとき、`#msf:ns--catalog&connection=wt` で WebTransport、
  `connection=q` で native QUIC が選ばれること。
- `connection=wt` の既定が `wt-h3` / `wt-h2` のどちらかに決まり、doc・help・テストで固定されていること。
- `--transport` との優先規則と q / wt 併記時の扱いが doc・help・テストで固定されていること。
- moq-pub と moq-sub の両方で同じ規則になっていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `examples/tokio-moq/src/lib.rs` に `resolve_transport(explicit, url)` を追加し、`--transport` と
  `--url` の `msf` fragment の予約パラメータ `connection` から接続種別を解決するようにした。
  - `connection` が無ければ `--transport` の指定を使い、それも無ければ native QUIC
  - `connection=q` は native QUIC、`connection=wt` は WebTransport。`wt` は実装 (HTTP/3 / HTTP/2) を
    指定しないため既定を `wt-h3` にし、`wt-h2` は `--transport wt-h2` を明示したときだけ使う
    (WebTransport の主経路は HTTP/3 である。draft-ietf-moq-transport-22 §6.2 (Session
    establishment) は HTTP/2 上の WebTransport も許容する)
  - `connection` と `--transport` が整合する場合 (`q` + quic / `wt` + wt-h3 / `wt` + wt-h2) は明示を使う
  - 矛盾する場合はエラーにする。`connection` は接続種別を MUST で指定するため `--transport` で
    上書きできない
  - `connection=q` と `connection=wt` の同時指定もエラーにする。§11.1.1 は `q` に Native QUIC、
    `wt` に WebTransport を MUST で対応づけており、同じ `connection` が両方を要求すると 2 つの MUST が
    両立しない。§11.1.1 の union 規則は値域 (range) を対象としたもので `connection` には適用しない
  - 値の検証 (`q` / `wt` 以外の拒否) は `MsfFragment::connection_types` に任せ、example では
    再実装しない
- `Transport::as_str` を追加し (`Transport::parse` の逆写像)、矛盾時のエラーメッセージで使う。
- `examples/moq-pub/src/cli.rs` と `examples/moq-sub/src/cli.rs` の `--transport` を
  `present_and_then` で受け取り「未指定」を区別するようにした (noargs の既定値は「指定あり」と
  区別できないため `.default()` を外した)。解決は `tokio_moq::resolve_transport` に任せ、moq-pub と
  moq-sub で同じ規則にした。解決のエラーは `noargs::Error::other` に通し、help の案内を付ける。
- doc / help / README を更新した。`README.md` と `examples/README.md` は「接続経路は `--transport` で
  選び、省略時は `--url` の `msf` fragment の `connection`、それも無ければ `quic`」に直し、
  `connection` と矛盾する `--transport` と `q` / `wt` の併記がエラーになることを書いた。
  `examples/README.md` の「example が `msf` fragment から使う予約パラメータ」の列挙に `connection` を
  足した。`examples/tokio-moq/src/lib.rs` の crate doc / `ServerUrl` / `parse_url`、両 example の
  `main.rs` と `Config.transport` の doc も解決の規則に合わせた。
- `src/msf/uri.rs` の `connection_types` の doc を「複数指定は出現順にすべて返す。§11.1.1 の union
  規則は値域 (range) を対象としたもので `connection` には適用しないため、両立しない組み合わせの
  扱いは呼び出し側が決める」に直し、example が依存する契約を固定した。
- テストは 18 本追加した。`resolve_transport` の解決規則 6 本 (fragment なし、`q` と `wt` の反映と
  既定の `wt-h3`、整合する明示、矛盾、重複、不正値)、`Transport::as_str` の逆写像 1 本、
  moq-pub と moq-sub の CLI 各 5 本 (fragment の反映、整合する明示、矛盾 2 種、q / wt 併記、不正値)、
  ライブラリの `connection_types` の複数指定 1 本。CLI テストは `noargs::Error` の `Debug` 出力で
  失敗理由も固定する。変異テストで、fragment の無視、既定の取り違え、矛盾の許容、q / wt 併記の
  許容、値の誤対応、CLI の配線の誤りが検出されることを確認した。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。
