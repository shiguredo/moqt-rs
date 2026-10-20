# example の TLS 証明書検証を既定で有効にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-example-tls-default
- Polished: 2026-10-21

## 目的

`--cert` を省略したときに証明書検証をスキップする既定は、C4M の bearer token を送る経路で MITM によるトークン漏洩を許す。example の既定を安全側に変える。

## 現状

- `examples/tokio-moq/src/tls.rs` の `build_client_config` は `disable_cert_validation || ca_cert_pem.is_none()` のとき検証を無効化する。この関数は QMux と WebTransport over HTTP/2 が共有する。
- `examples/tokio-moq/src/quic.rs` の `build_tls_client` は `--cert` が無いとき `build_insecure_tls_client` を呼ぶ。`examples/tokio-moq/src/lib.rs` の `apply_tls_config_h3` / `apply_tls_config_h2` も `--cert` が無いとき `ClientConfig::insecure()` を設定する。このため native QUIC と WebTransport over HTTP/3 も既定で検証をスキップする。
- `examples/moq-pub/src/cli.rs` / `examples/moq-sub/src/cli.rs` の `--cert` の help は「省略時は検証をスキップ」と案内している。
- `examples/tokio-moq/README.md` には `--cert` 省略時に検証をスキップする旨と、本番では `--cert` を指定する旨が書かれている。ルート `README.md` にも `--cert` の扱いの記述があり、`docs/missing-features.md` には `--cert` を省略すると検証をスキップする旨が書かれている。
- `--insecure` に相当する CLI フラグは無く、検証をスキップするには `--cert` を省略するしかない。
- 影響: 検証なし TLS で C4M トークンが流れ得る。

## 設計方針

- `--insecure` フラグを追加し、証明書検証のスキップはこのフラグを明示したときだけ行う。`--cert` と `--insecure` の同時指定は CLI でエラーにする。`ConnectParams` でも両方が指定された場合はエラーにする。
- `--cert` も `--insecure` も指定しない場合は、OS のシステムルートストアで検証する。`examples/tokio-moq` に `rustls-native-certs` を追加し、`load_native_certs()` で得たルートを `rustls::RootCertStore` に入れて使う。ルートを 1 つも読み込めない場合は接続をエラーにし、検証をスキップする経路へフォールバックしない。
- `s2n-quic-rustls` の `Client::builder()` はシステムルートを読まない。QUIC と WebTransport over HTTP/3 は `rustls::ClientConfig` を組み立て、`s2n_rustls::Client::from(...)` に渡す。QUIC は TLS 1.3 と ALPN `moqt-22`、WebTransport over HTTP/3 は ALPN `h3` を設定する。
- `ConnectParams` に `insecure: bool` を追加し、`moq-pub` / `moq-sub` の `Config` から初回接続と GOAWAY 後の移行の両方に渡す。`examples/tokio-moq/src/lib.rs` の `connect` はこの値を `quic::connect` / `qmux::connect` / `apply_tls_config_h3` / `apply_tls_config_h2` に渡す。
- `examples/tokio-moq/src/webtransport_h3.rs` の `connect` は、CA 証明書も `insecure` も指定されていないときにシステムルートで検証する。`examples/tokio-moq/src/webtransport_h2.rs` は `tls.rs` の変更で既定がシステムルートになる。`examples/tokio-moq/src/qmux.rs` は `connect` に `insecure` を追加し、`tls.rs` の変更に追随する。
- ローカル開発の利便性を維持するため、`--insecure` の利用例をルート `README.md` と `examples/tokio-moq/README.md`、`examples/moq-pub/README.md`、`examples/moq-sub/README.md` に書く。
- e2e テストは `MOQT_E2E_INSECURE=1` のときだけ `--insecure` を渡す。CI の relay が自己署名証明書を使う場合は `.github/workflows/e2e-test.yml` でこの環境変数を設定する。公開 CA の証明書を使う場合は設定せず、既定の検証のまま実行する。
- 破壊的変更として扱う。`CHANGES.md` にはエントリを追加しない (CODEBASE.md の指示に従う)。

## 変更対象

- `examples/tokio-moq/Cargo.toml`: `rustls-native-certs` を追加する。
- `examples/tokio-moq/src/tls.rs`: `build_client_config` を、`insecure` のときだけ検証を無効化し、CA 証明書が無いときはシステムルートを使うように変更する。
- `examples/tokio-moq/src/quic.rs`: `build_tls_client` / `connect` に `insecure` を追加し、システムルートの検証経路を追加する。
- `examples/tokio-moq/src/webtransport_h3.rs`: `connect` に、CA 証明書も `insecure` も無い場合のシステムルート検証経路を追加する。
- `examples/tokio-moq/src/qmux.rs`: `connect` に `insecure` を追加し、システムルートの検証経路を追加する。`connect_pair` のテスト呼び出しには `insecure: true` を渡す。
- `examples/tokio-moq/src/lib.rs`: `ConnectParams` に `insecure` を追加し、`connect` / `apply_tls_config_h3` / `apply_tls_config_h2` の TLS 設定を変更する。`quic::connect` のテスト呼び出し (自己署名証明書のテスト peer) には `insecure: true` を渡す。`ConnectParams` の `cert` と `insecure` の排他テストを追加する。
- `examples/tokio-moq/src/peer_harness.rs`: `ConnectParams` のテスト用構築箇所 (4 箇所) と `quic::connect` のテスト呼び出し (2 箇所) に `insecure: true` を追加する。テスト peer は自己署名証明書を使うため、システムルートストアでは接続できない。
- `examples/moq-pub/src/cli.rs` / `examples/moq-sub/src/cli.rs`: `--insecure` を追加し、`--cert` の help を更新する。`Config` に `insecure` を追加する。
- `examples/moq-pub/tests/test_cli.rs` / `examples/moq-sub/tests/test_cli.rs`: `--insecure` の help 表示と `--cert` との同時指定エラーをテストで固定する。
- `examples/moq-pub/src/pipeline.rs` / `examples/moq-sub/src/pipeline.rs`: `ConnectParams` に `insecure` を渡す (subscriber は初回接続と GOAWAY 後の移行の 2 箇所)。
- `README.md` / `examples/tokio-moq/README.md` / `examples/moq-pub/README.md` / `examples/moq-sub/README.md`: TLS の既定と `--insecure` の説明・利用例を更新する。
- `docs/missing-features.md`: `--cert` 省略時の既定に関する記述を更新する。
- `e2e-tests/tests/roundtrip.rs` / `e2e-tests/README.md` / `.github/workflows/e2e-test.yml`: `MOQT_E2E_INSECURE=1` のときだけ `--insecure` を渡す。CI の relay が自己署名証明書を使う場合だけ workflow で設定する。

## 完了条件

- `--cert` も `--insecure` も指定しないとき、native QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux のすべてでシステムルートストアによる証明書検証が有効になっていること。
- システムルートを読み込めない場合は接続がエラーになり、検証をスキップしないこと。
- `--insecure` を指定したときだけ証明書検証がスキップされ、スキップした旨の警告ログが出ること。
- `--cert` と `--insecure` の同時指定がエラーになること。
- `--cert` を指定したときは、従来どおりその CA 証明書で検証すること。
- help / README / `docs/missing-features.md` の記述が新しい既定と一致していること。
- `--insecure` の受理と `--cert` との排他をテストで固定していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `prek run --all-files` が通ること。
- e2e テストは既定の検証のまま通ること。自己署名の relay を使う場合は `MOQT_E2E_INSECURE=1` で `--insecure` を渡して通ること。
