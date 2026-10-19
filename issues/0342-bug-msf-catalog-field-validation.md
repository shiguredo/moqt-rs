# MSF カタログのフィールド検証を MSF-01 の MUST に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-catalog-field-validation
- Polished: {YYYY-MM-DD}

## 目的

MSF-01 のカタログは複数のフィールドに MUST の検証を要求するが、`src/msf.rs` は一部を存在確認だけで通す、あるいは encode と decode の検証範囲が非対称で「自身が decode できないカタログを encode できる」。producer と consumer の両方を仕様に合わせる。

## 現状

- `connectionUri` は任意文字列として decode / encode され、`validate_full_track` でも検証しない。§5.2.36 は valid な MOQT endpoint URI (`moqt://` / `https://`) を MUST とする。テストも `"not a uri"` の受理を固定している。
- `cipherSuite` は `moq-secure-objects` のとき存在だけを検証し、値が supported cipher suite かを見ない。§4.3.3 / §5.2.39 は `aes-128-gcm-sha256` など定義済みの値であることを MUST とする。
- `decode_full_catalog` は §5.1.7 の「`initDataList` は `tracks` 配列より後」の順序を検証しない。
- `validate_lang_tag` は primary 以外の subtag を「1 文字以上の ASCII 英数字」としか見ず、最大 8 文字、extension singleton、privateuse の構造を見ない。§5.2.32 は standard tag を MUST とする。
- `initDataList.data` (§5.1.7 は Base64 encoded) と `trackBaseKey` (§5.2.41 は base64-encoded) を Base64 として検証しない。
- `authInfo` は同一 scheme キーの重複を受理し、encode でも重複メンバーを出力する (RFC 8259 §4 の一意性 SHOULD)。
- `validate_full_catalog_for_encode` は `catalog.version` を検査しない。手組みの `MsfCatalog { version: "x" }` を encode できるが、同じライブラリの decode は `InvalidCatalogVersion` で拒否する。`framerate` は decode が非有限を拒否する一方、encode は finite を検査せず NaN / inf を `null` として出力する。
- 影響: 不正なカタログを受理・生成する。相互運用で相手の実装に依存した挙動になる。

## 設計方針

- decode / encode の両方で、上記フィールドを §5 の表に沿って検証する。
- `connectionUri` は `moqt://` / `https://` の URI として検証する。transport 層に委譲する場合は公開 API の契約として明記する。
- `cipherSuite` は `moq-secure-objects` のとき定義済みの値に限定する。
- `initDataList` の順序は、生 JSON のメンバー順を提供できる経路で検証する。実装しない場合は decode の doc に制限を明記する。
- Base64 は `base64ct` で decode 可能か検証する。
- encode は「自身の decode が受理する文書だけを出力する」契約を doc とテストで固定する。

## 完了条件

- 上記の MUST / SHOULD に対応する negative test があること。
- encode したカタログを自身の decode で roundtrip できること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
