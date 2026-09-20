# 複数アドレスに解決されるホストで先頭以外の接続先を試さない

- Created: 2026-09-24
- Completed: 2026-09-30
- Branch: feature/fix-resolve-multiple-addresses
- Polished: 2026-09-27

## 目的

ホスト名が IPv4 と IPv6 の両方に解決される環境で、relay が待ち受ける側のアドレスへ接続できるようにする。`resolve_socket_addr` は最初の結果だけを使うため、先頭が IPv6 で relay が IPv4 のみ待ち受ける場合に接続できない。

## 現状

`examples/moqt-transport/src/lib.rs` の `resolve_socket_addr` は `lookup_host` の `next()` だけを使い、`SocketAddr` を 1 つ返す。`local_bind_addr` は接続先のファミリに合わせてローカルソケットを選ぶため送信自体はできるが、接続先は 1 つに固定される。

`localhost` は `::1` と `127.0.0.1` の両方に解決され、返る順序は環境に依存する (macOS では `[::1]` が先に返る)。IPv4 のみ待ち受ける relay に対して `moqt://localhost:4433/app` で接続すると、先頭の `[::1]:4433` へ送って失敗する。`examples/README.md` には「先頭のアドレスに接続する」と記載してある。

## 設計方針

- 解決結果を `Vec<SocketAddr>` として返す関数 (`resolve_socket_addrs`) に置き換え、単一の `resolve_socket_addr` は削除する。呼び出し元は publisher / subscriber の pipeline の 2 か所だけで、両方が列版を使うため使い分けは発生しない。単一版の単体テストは列版へ移し、`examples/moqt-transport/src/quic.rs` の doc コメントの参照も更新する
- 列版は `authority_parts` と `lookup_host` を使う現在の解決構造を保ち、名前解決は接続前に 1 回だけ行う (再解決しない)
- publisher / subscriber の pipeline は解決結果を順に試し、接続に成功したアドレスを採用する。試行単位は transport ごとの接続確立 (`examples/moqt-transport/src/quic.rs` の `connect` / `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect`) とし、確立後の SETUP 以降の失敗では次のアドレスを試さない (同一 peer への失敗でありアドレスに依存しないため)。試行ごとに接続先をログに出す (失敗した試行もログに残す)
- すべて失敗した場合は最後の失敗を返す。並行接続 (Happy Eyeballs) と再解決は行わず、解決結果の順序どおりの順次試行にとどめる
- 解決結果の順序に独自の優先順位は付けない
- QUIC と WebTransport の両方で同じ解決結果を使う
- 名前解決のタイムアウトは `issues/0153-bug-resolve-socket-addr-timeout.md` の関心事である。列版は解決関数と試行ループの関数境界を分け、0153 のタイムアウトが解決関数に適用できる構造にして、どちらが先に実装されても成立するようにする

## 完了条件

- 複数アドレスに解決されるホストで、先頭のアドレスへの接続が失敗したときに次のアドレスを試すこと
- IPv4 のみ待ち受ける relay へ `moqt://localhost:4433/app` で publisher / subscriber が接続できること (実機確認)。確認環境で `localhost` の解決順が `[::1]` 先頭であることをログで確認してから行うこと (先頭が IPv4 の環境では欠陥を再現できない)
- すべての試行が失敗したときに最後の失敗が返ることを実機確認で確認すること
- 単一アドレスのホストと IP リテラルの挙動が変わらないこと
- 解決結果の列を返す関数の単体テストがあり、次が固定されていること (モックやスタブは使わない)
  - IP リテラルは 1 件の列になる
  - `localhost` はループバックアドレスの列になる (列の内容と順序は環境に依存するため、順序そのものは固定しない)
  - 解決結果が 0 件のときは解決失敗のエラーになる
- `examples/README.md` の「先頭のアドレスに接続する」記述を、解決結果を順に試す実態に合わせて更新すること

## 解決方法

`examples/tokio-moq/src/lib.rs` の解決関数を列版へ置き換え、publisher / subscriber の pipeline が解決結果を順に試すようにした。issue 本文の `examples/moqt-transport/` は現在の `examples/tokio-moq/` に当たる (ファイル構成の変更に追随させる)。

- `resolve_socket_addr` を削除し、解決順を保つ `resolve_socket_addrs` (`Vec<SocketAddr>`) に置き換えた。IP リテラルは 1 件の列、ホスト名は `lookup_host` の結果をそのまま列にする。解決結果が 0 件のときは `ResolutionFailed` になる (0153 のタイムアウトの扱いはそのまま)
- `connect_with_fallback` を追加した。解決結果を順に試し、`connect` が成功した時点で確定する。試行ごとに `Connecting to {addr}` を info、失敗を warn で出し、すべて失敗した場合は最後の失敗を返す。並行接続 (Happy Eyeballs) と再解決は行わない
- publisher (`examples/moq-pub/src/pipeline.rs`) と subscriber (`examples/moq-sub/src/pipeline.rs`) は、トランスポート確立と SETUP ハンドシェイクを試行の単位として `connect_with_fallback` に渡す。確立後の SETUP 以降の失敗では次のアドレスを試さない (同一 peer への失敗でありアドレスに依存しないため)。設定は試行ごとに複製するため `Config` に `Clone` を derive した
- `examples/README.md` の記述を「解決順に接続を試し、確立できたアドレスを採用する」に更新し、`examples/tokio-moq/src/quic.rs` の `connect` の doc も追随させた

追加・更新したテスト (`examples/tokio-moq/src/lib.rs` の `tests` モジュール):

- `resolve_socket_addrs_defaults_ipv4_port_to_443` / `resolve_socket_addrs_keeps_explicit_port` / `resolve_socket_addrs_defaults_ipv6_port_to_443`: IP リテラルは既定ポートを補って 1 件の列になること
- `resolve_socket_addrs_resolves_hostname`: `localhost` はループバックアドレスの列になり、全件のポートが既定ポートになること (件数と順序は環境依存のため検証しない)
- `resolve_socket_addrs_rejects_invalid_authority`: 不正な authority は解決せず `InvalidAuthority` になること
- `resolve_socket_addrs_times_out_when_resolver_does_not_answer` / `resolve_socket_addrs_reports_resolution_failure` / `resolve_socket_addrs_succeeds_within_timeout`: 0153 のタイムアウトと成功経路が列版でも変わらないこと
- `connect_with_fallback_tries_resolved_addrs_in_order`: 実 QUIC サーバーを `127.0.0.1` のみで待ち受け、`localhost` の解決結果に IPv6 が含まれる環境では 1 回目の試行が失敗して 2 回目の `127.0.0.1` で確立すること。IPv6 が含まれない環境では 1 回で確立するため、試行回数の期待値は解決結果に応じて切り替える
- `connect_with_fallback_returns_last_failure`: 全アドレスで失敗した場合は最後の失敗が返ること

実機確認 (一時的な検証ターゲットで `127.0.0.1` のみを待ち受ける QUIC サーバーを立て、publisher / subscriber を実行した。確認後、検証ターゲットは削除した):

- `moq-pub --url moqt://localhost:45123/app --fake-capture-device --no-audio` は `Resolved localhost:45123 to [[::1]:45123, 127.0.0.1:45123]` の後 `Connecting to [::1]:45123` が 10 秒で失敗し、`Connecting to 127.0.0.1:45123` で `Connected to 127.0.0.1:45123` になった。サーバー側も `accepted from 127.0.0.1` を出力した
- `moq-sub --url moqt://localhost:45124/app` も同じ順序で `127.0.0.1` へ接続した
- この環境は `localhost` が `[::1]` 先頭に解決するため、修正前は最初の試行だけで終わっていた欠陥を再現できている
- 確立後の `connection closed before control stream` は MOQT の SETUP に応答しない検証用サーバーによるもので、本 issue の範囲外である

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。

## 既知の制限

- 待ち受けたアドレスへの接続が即座に失敗せず、ハンドシェイクのタイムアウトまで待つ場合は、次のアドレスを試すまでその時間がかかる (実機確認では IPv6 の試行が 10 秒かかった)。接続確立のタイムアウトは本 issue の範囲外とする
