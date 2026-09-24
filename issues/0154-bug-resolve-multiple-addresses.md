# 複数アドレスに解決されるホストで先頭以外の接続先を試さない

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-resolve-multiple-addresses
- Polished: {YYYY-MM-DD}

## 目的

ホスト名が IPv4 と IPv6 の両方に解決される環境で、relay が待ち受ける側のアドレスへ接続できるようにする。`resolve_socket_addr` は最初の結果だけを使うため、先頭が IPv6 で relay が IPv4 のみ待ち受ける場合に接続できない。

## 現状

`examples/moqt-transport/src/lib.rs` の `resolve_socket_addr` は `lookup_host` の `next()` だけを使い、`SocketAddr` を 1 つ返す。`local_bind_addr` は接続先のファミリに合わせてローカルソケットを選ぶため送信自体はできるが、接続先は 1 つに固定される。

macOS の `localhost` は `[::1]` を先に返す。IPv4 のみ待ち受ける relay に対して `moqt://localhost:4433/app` で接続すると、先頭の `[::1]:4433` へ送って失敗する。`examples/README.md` には「先頭のアドレスに接続する」と記載してある。

## 設計方針

- 解決結果を `Vec<SocketAddr>` として返す関数を追加し、単一の `resolve_socket_addr` との使い分けを明確にする
- publisher / subscriber の pipeline は解決結果を順に試し、接続に成功したアドレスを採用する。試行ごとに接続先をログに出す
- すべて失敗した場合は最後の失敗を返す。並行接続 (Happy Eyeballs) と再解決は行わず、解決結果の順序どおりの順次試行にとどめる
- 解決結果の順序に独自の優先順位は付けない
- QUIC と WebTransport の両方で同じ結果を使う

## 完了条件

- 複数アドレスに解決されるホストで、先頭のアドレスへの接続が失敗したときに次のアドレスを試すこと
- IPv4 のみ待ち受ける relay へ `moqt://localhost:4433/app` で publisher / subscriber が接続できること (実機確認)
- 単一アドレスのホストと IP リテラルの挙動が変わらないこと
- 解決結果の列を返す関数の単体テストがあり、順序と全滅時のエラーが固定されていること
