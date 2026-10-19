# example の TLS 証明書検証を既定で有効にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-example-tls-default
- Polished: {YYYY-MM-DD}

## 目的

`--cert` を省略したときに証明書検証をスキップする既定は、C4M の bearer token を送る経路で MITM によるトークン漏洩を許す。example の既定を安全側に変える。

## 現状

- `examples/tokio-moq/src/tls.rs` の `build_client_config` は `disable_cert_validation || ca_cert_pem.is_none()` のとき検証を無効化する。
- `examples/moq-pub/src/cli.rs` / `examples/moq-sub/src/cli.rs` の `--cert` の help も「省略時は検証をスキップ」と案内している。
- README には警告があるが、既定は安全側になっていない。
- 影響: 検証なし TLS で C4M トークンが流れ得る。

## 設計方針

- 検証スキップは明示的な `--insecure` (または同等のフラグ) を要求する。
- `--cert` 省略時はシステムルートストアを使う。
- ローカル開発の利便性を維持するため、`--insecure` の利用例を README に書く。
- 破壊的変更として扱う。

## 完了条件

- `--cert` 省略時の既定が証明書検証有効になっていること。
- 検証スキップには明示的なフラグが必要であること。
- README / help の記述が更新されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
