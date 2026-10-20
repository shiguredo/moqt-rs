# MSF カタログのフィールド検証を MSF-01 の MUST に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-catalog-field-validation
- Polished: 2026-10-21

## 目的

MSF-01 のカタログは複数のフィールドに MUST の検証を要求するが、`src/msf.rs` は一部を存在確認だけで通す、あるいは encode と decode の検証範囲が非対称で「自身が decode できないカタログを encode できる」。producer と consumer の両方を仕様に合わせる。

## 現状

- `connectionUri` は任意文字列として decode / encode され、`validate_full_track` でも検証しない。§5.2.36 は valid な MOQT endpoint URI (`moqt://` / `https://`) を MUST とする。テストも `"not a uri"` の受理を固定している。
- `cipherSuite` は `moq-secure-objects` のとき存在だけを検証し、値が supported cipher suite かを見ない。§4.3.3 / §5.2.39 は `aes-128-gcm-sha256` など定義済みの値であることを MUST とする。
- `decode_full_catalog` は §5.1.7 の「`initDataList` は `tracks` 配列より後」の順序を検証しない。
- `validate_lang_tag` は irregular (grandfathered) タグと privateuse の骨格 (`x` + 1 個以上の ASCII 英数字サブタグ) を検査するが、primary 以外の subtag の最大 8 文字、extension singleton の後続、variant / extlang / script / region の構造を検査しない。§5.2.32 は standard tag を MUST とする。
- `initDataList.data` (§5.1.7 は Base64 encoded) と `trackBaseKey` (§5.2.41 は base64-encoded) を Base64 として検証しない。
- `authInfo` は同一 scheme キーの重複を受理し、encode でも重複メンバーを出力する (RFC 8259 §4 の一意性 SHOULD)。`MsfAuthInfo` の doc は「重複 scheme は順序どおりに保持する」としているため、重複拒否に合わせて更新する必要がある。
- `validate_full_catalog_for_encode` は `catalog.version` を検査しない。手組みの `MsfCatalog { version: "x" }` を encode できるが、同じライブラリの decode は `InvalidCatalogVersion` で拒否する。`framerate` は decode が非有限を拒否する一方、encode は finite を検査せず NaN / inf を `null` として出力する。
- 影響: 不正なカタログを受理・生成する。相互運用で相手の実装に依存した挙動になる。

## 設計方針

- decode / encode の両方で、上記フィールドを §5 の表に沿って検証する。
- clone 断片 (`MsfCloneTrack`) は親から属性を継承し得るため、明示された値だけを検証し、欠如は拒否しない (継承解決後の `MsfTrack` は `validate_full_track` で検証する)。
- `lang` は RFC 5646 §2.1 の `Language-Tag` ABNF 全体 (langtag / privateuse / grandfathered) を構文検証する。RFC 5646 §2.1.1 の大文字小文字無視に従い、privateuse の `X` も受理する (現行の小文字 `x` のみの受理範囲を広げる)。IANA Language Subtag Registry との照合は行わず、その制限を `MsfTrack` / `MsfCloneTrack` の `lang` の doc に明記する。
- `connectionUri` は §5.2.36 の例に従い `moqt://` / `https://` の URI として検証する。msf.rs で検証する場合は MOQT §3.1.1 の `moqt-URI` (scheme、非空の host を含む authority、path-abempty、query) と RFC 3986 の構文に従う。transport 層に委譲する場合は、`MsfTrack` / `MsfCloneTrack` の `connection_uri` の公開 doc に「msf.rs は検証せず transport 層が検証する」契約を明記する。
- `cipherSuite` は `moq-secure-objects` のとき §5.2.39 の定義値 (`aes-128-gcm-sha256` / `aes-256-gcm-sha512` / `aes-128-ctr-hmac-sha256-80`) に限定する。clone 断片では `encryptionScheme` が明示されたときだけ値を検証し、省略時は親から継承し得るため継承解決後の `validate_full_track` に委ねる。
- `authInfo` の同一 scheme キーの重複は decode / encode の両方で拒否する (RFC 8259 §4 の一意性 SHOULD)。
- `initDataList` の順序は、生 JSON のメンバー順を提供できる経路で検証する。実装しない場合は `MsfCatalogDocument::decode` の公開 doc に「nojson がメンバー順を提供しないため順序検証を行わない」制限を明記する。
- Base64 は RFC 4648 §4 の標準 Base64 (パディングあり) を `base64ct::Base64` で decode 可能か検証する。
- encode は「自身の decode が受理する文書だけを出力する」契約を doc とテストで固定する。`catalog.version` が `MSF_VERSION` でない場合は `InvalidCatalogVersion` を返し、`framerate` が非有限の場合は `InvalidCatalog` を返す。
- 検証する契約 (Base64、`cipherSuite` の定義値、`framerate` の有限性、`catalog.version` の `MSF_VERSION` 一致) は、該当する公開フィールドと `MsfCatalogDocument::encode` の doc に明記する。あわせて `docs/msf.md` の「検証規則」を今回の検証範囲に同期させる。

## 完了条件

- msf.rs で検証する MUST / SHOULD に対応する negative test があること。`connectionUri` を transport 層へ委譲する場合と `initDataList` の順序検証を実装しない場合は、公開 doc に契約・制限を明記し、その契約を固定するテストを追加すること。
- `catalog.version` が `MSF_VERSION` でない完全カタログの encode が `InvalidCatalogVersion` で拒否されること。
- `framerate` が NaN / inf の手組みカタログの encode が `InvalidCatalog` で拒否されること。
- `authInfo` の重複 scheme を decode / encode の両方で拒否すること (既存の `auth_info_duplicate_scheme_kept_in_order` は重複拒否のテストに置き換える)。あわせて `MsfAuthInfo` の doc を重複拒否に合わせて更新すること。
- `connectionUri` を msf.rs で検証する場合、既存の `connection_uri_invalid_value_accepted` / `connection_uri_invalid_value_accepted_in_clone` を拒否テストに置き換えること。
- `lang` の大文字 `X` privateuse (`X-FOO` / `X-foo`) を受理するテストを追加し、既存の `lang_uppercase_privateuse_rejected` を置き換えること。
- `lang` の IANA Language Subtag Registry 照合を行わない制限を `MsfTrack` / `MsfCloneTrack` の `lang` の doc に明記すること。
- 検証する契約 (Base64、`cipherSuite` の定義値、`framerate` の有限性、`catalog.version` の `MSF_VERSION` 一致) が該当する公開フィールドと `MsfCatalogDocument::encode` の doc に明記されていること。`docs/msf.md` の「検証規則」が実装と一致していること。
- encode したカタログを自身の decode で roundtrip できること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
