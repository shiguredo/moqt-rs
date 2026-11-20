# example の TLS 証明書検証を既定で有効にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-example-tls-default
- Polished: 2026-11-21

## 目的

`--cert` を省略したときに証明書検証をスキップする既定は、C4M の bearer token を送る経路で MITM によるトークン漏洩を許す。example の既定を安全側に変える。

## 現状

- `crates/tokio-moq/src/tls.rs` の `build_client_config` は `disable_cert_validation || ca_cert_pem.is_none()` のとき検証を無効化する。この関数は QMux / WebTransport over HTTP/2 / WebTransport over HTTP/3 と、`build_quic_client_config` 経由の s2n-quic / tobi-quic が共有する。
- `crates/tokio-moq/src/tls.rs` の `build_quic_client_config` は `cert_path` が無いとき
  `build_client_config(None, true, alpn)` を呼ぶ。`crates/tokio-moq/src/lib.rs` の
  `apply_tls_config_h3` / `apply_tls_config_h2` も `--cert` が無いとき
  `ClientConfig::insecure()` を設定する。`crates/tokio-moq/src/quic/ngtcp2.rs` の `connect` は
  `--cert` が無いとき `with_verify_peer(false)` を設定する。このため native QUIC
  (s2n-quic / tobi-quic / ngtcp2) と WebTransport over HTTP/3 / HTTP/2 も既定で検証をスキップする。
- `crates/moq-pub/src/cli.rs` / `crates/moq-sub/src/cli.rs` の `--cert` の help は「省略時は検証をスキップ」と案内している。
- `crates/tokio-moq/README.md` には `--cert` 省略時に検証をスキップする旨と、本番では `--cert` を指定する旨が書かれている。`crates/README.md` にも `--cert` を省略すると検証をスキップする旨が書かれている。`docs/missing-features.md` にも同旨がある。ルート `README.md` には TLS の記述は無い。
- `--insecure` に相当する CLI フラグは無く、検証をスキップするには `--cert` を省略するしかない。
- 影響: 検証なし TLS で C4M トークンが流れ得る。

## 設計方針

- `--insecure` フラグを追加し、証明書検証のスキップはこのフラグを明示したときだけ行う。`--cert` と `--insecure` の同時指定は CLI でエラーにする。`ConnectParams` でも両方が指定された場合は `connect` がエラーを返す。
- `--cert` も `--insecure` も指定しない場合は、OS のシステムルートストアで検証する。
  rustls を使う経路 (QMux / WebTransport over HTTP/2 / WebTransport over HTTP/3 / s2n-quic / tobi-quic) では、
  `crates/tokio-moq` に `rustls-native-certs` を追加し、`load_native_certs()` で得たルートを
  `rustls::RootCertStore` に入れて使う。ルートを 1 つも読み込めない場合は接続をエラーにし、
  検証をスキップする経路へフォールバックしない。
- `quic-ngtcp2` は rustls ではなく `shiguredo_ngtcp2_tokio` の TLS 実装
  (aws-lc-sys の SSL、BoringSSL 由来) を使うため、`rustls-native-certs` は使わない。
  `crates/tokio-moq/src/quic/ngtcp2.rs` の `connect` は、`--cert` も `--insecure` も無いときに
  `ClientConfig::with_verify_peer(true)` を使い、aws-lc-sys の SSL の既定のトラストストア
  (既定の CA パスと `SSL_CERT_FILE` / `SSL_CERT_DIR`) で検証する。
  `--cert` がある場合は `with_ca_cert_pem` で追加し、`--insecure` のときだけ
  `with_verify_peer(false)` にする。
- `s2n-quic-rustls` の `Client::builder()` はシステムルートを読まない。
  s2n-quic / tobi-quic は `tls.rs` で `rustls::ClientConfig` を組み立て、
  s2n-quic は `s2n_rustls::Client::from(...)`、tobi-quic は `ClientTlsConfig::new(...)` に渡す。
  WebTransport over HTTP/3 は tobi-quic の `ClientTlsConfig::new(...)` に渡す。
  native QUIC は TLS 1.3 と ALPN `moqt-23`、WebTransport over HTTP/3 は ALPN `h3` を設定する。
- `ConnectParams` に `insecure: bool` を追加し、`moq-pub` / `moq-sub` の `Config` から初回接続、GOAWAY 後の移行、moq-sub の side session に渡す。`crates/tokio-moq/src/lib.rs` の `connect` はこの値を `quic::connect` / `qmux::connect` / `apply_tls_config_h3` / `apply_tls_config_h2` に渡す。
- `crates/tokio-moq/src/webtransport_h3.rs` の `connect` は、CA 証明書も `insecure` も指定されていないときにシステムルートで検証する。`crates/tokio-moq/src/webtransport_h2.rs` は `tls.rs` の変更で既定がシステムルートになる。`crates/tokio-moq/src/qmux.rs` は `connect` に `insecure` を追加し、`tls.rs` の変更に追随する。
- ローカル開発の利便性を維持するため、`--insecure` の利用例を `crates/README.md`、`crates/tokio-moq/README.md`、`crates/moq-pub/README.md`、`crates/moq-sub/README.md` に書く。
- e2e テストは `MOQT_E2E_INSECURE=1` のときだけ `--insecure` を渡す。`.github/workflows/e2e-test.yml` は、CI の relay が自己署名証明書を使う場合に `MOQT_E2E_INSECURE` を `1` に設定し、公開 CA の証明書を使う場合は設定しない。
- 破壊的変更として扱う。`CHANGES.md` にはエントリを追加しない (CODEBASE.md の指示に従う)。

## 変更対象

- `crates/tokio-moq/Cargo.toml`: `rustls-native-certs` を追加する (用途コメントを付ける)。
- `crates/tokio-moq/src/tls.rs`: `build_client_config` を、`insecure` のときだけ検証を無効化し、CA 証明書が無いときはシステムルートを使うように変更する。`build_quic_client_config` にも `insecure` を追加する。doc コメントを新しい既定に合わせる。
- `crates/tokio-moq/src/quic/s2n.rs` / `crates/tokio-moq/src/quic/tobi_backend.rs` /
  `crates/tokio-moq/src/quic/ngtcp2.rs`: `connect` に `insecure` を追加し、
  s2n-quic / tobi-quic は `build_quic_client_config` に、ngtcp2 は
  `ClientConfig::with_verify_peer` / `with_ca_cert_pem` に渡す。
  `crates/tokio-moq/src/quic.rs` はバックエンドの `connect` を再エクスポートするだけで、
  シグネチャは各バックエンドに従う。
- `crates/tokio-moq/src/webtransport_h3.rs`: `connect` に、CA 証明書も `insecure` も無い場合のシステムルート検証経路を追加する。
- `crates/tokio-moq/src/qmux.rs`: `connect` に `insecure` を追加し、システムルートの検証経路を追加する。`qmux.rs` のテスト呼び出し (6 箇所) には `insecure: true` を渡す。
- `crates/tokio-moq/src/lib.rs`: `ConnectParams` に `insecure` を追加し、
  `connect` / `apply_tls_config_h3` / `apply_tls_config_h2` の TLS 設定を変更する。
  `connect` で `cert` と `insecure` の同時指定をエラーにする。
  `quic::connect` のテスト呼び出し (自己署名証明書のテスト peer) と、
  テスト用 `ConnectParams` の構築箇所 (1 箇所) には `insecure: true` を渡す。
  `ConnectParams` の `cert` と `insecure` の排他テストを追加する。
  `ConnectParams` / `connect` の doc コメントを更新する。
- `crates/tokio-moq/src/peer_harness.rs`: `ConnectParams` のテスト用構築箇所 (5 箇所) と `quic::connect` のテスト呼び出し (2 箇所) に `insecure: true` を追加する。テスト peer は自己署名証明書を使うため、システムルートストアでは接続できない。
- `crates/moq-pub/src/cli.rs` / `crates/moq-sub/src/cli.rs`: `--insecure` を追加し、`--cert` の help を更新する。`Config` に `insecure` を追加する。
- `crates/moq-pub/tests/test_cli.rs` / `crates/moq-sub/tests/test_cli.rs`: `--insecure` の help 表示と `--cert` との同時指定エラーをテストで固定する。
- `crates/moq-pub/src/pipeline.rs` / `crates/moq-sub/src/pipeline.rs`:
  `ConnectParams` に `insecure` を渡す。moq-sub は初回接続、GOAWAY 後の移行、
  side session の 3 箇所であり、`MigrationRequest` / `SideSessionRequest` /
  `start_side_session` / `dispatch_side_session_command` にも `insecure` を追加して
  `Config` から引き継ぐ。moq-sub のテスト用 `Config` にも `insecure: false` を追加し、
  テスト用 `ConnectParams` (1 箇所) にも `insecure: true` を追加する。
- `crates/README.md` / `crates/tokio-moq/README.md` / `crates/moq-pub/README.md` / `crates/moq-sub/README.md`: TLS の既定と `--insecure` の説明・利用例を更新する。
- `docs/missing-features.md`: `--cert` 省略時の既定に関する記述を更新する。
- `e2e-tests/tests/roundtrip.rs` / `e2e-tests/README.md` / `.github/workflows/e2e-test.yml`: `MOQT_E2E_INSECURE=1` のときだけ `--insecure` を渡す。workflow は CI の relay の証明書に合わせて `MOQT_E2E_INSECURE` を設定する (自己署名なら `1`、公開 CA なら未設定)。

## 完了条件

- `--cert` も `--insecure` も指定しないとき、native QUIC (s2n-quic / tobi-quic / ngtcp2) / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux のすべてでシステムルートストアによる証明書検証が有効になっていること。ngtcp2 は aws-lc-sys の SSL の既定のトラストストアを使う。
- システムルートを読み込めない場合は接続がエラーになり、検証をスキップしないこと (ngtcp2 はハンドシェイクの失敗として観測される)。
- `--insecure` を指定したときだけ証明書検証がスキップされ、スキップした旨の警告ログが出ること。
- `--cert` と `--insecure` の同時指定がエラーになること。
- `--cert` を指定したときは、従来どおりその CA 証明書で検証すること。
- help / README / `docs/missing-features.md` の記述と、`ConnectParams` / `connect` / `apply_tls_config_h3` / `apply_tls_config_h2` / `tls.rs` の doc コメントが新しい既定と一致していること。
- `--insecure` の受理と `--cert` との排他をテストで固定していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `cargo clippy -p tokio-moq --no-default-features --features quic-ngtcp2 --all-targets -- -D warnings` と `cargo clippy -p tokio-moq --no-default-features --features quic-tobi --all-targets -- -D warnings` が通ること。
- `prek run --all-files` が通ること。
- e2e テストは、CI の relay の証明書に合わせた設定 (自己署名なら `MOQT_E2E_INSECURE=1`、公開 CA なら未設定) で通ること。
