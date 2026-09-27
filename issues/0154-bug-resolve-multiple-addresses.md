# 複数アドレスに解決されるホストで先頭以外の接続先を試さない

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
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
