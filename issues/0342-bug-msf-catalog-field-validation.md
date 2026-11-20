# MSF カタログのフィールド検証を draft-ietf-moq-msf-02 の MUST / SHOULD に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-catalog-field-validation
- Polished: 2026-11-21

## 目的

draft-ietf-moq-msf-02 のカタログは複数のフィールドに MUST / SHOULD の検証を要求するが、`src/msf.rs` は一部を存在確認だけで通す、あるいは encode と decode の検証範囲が非対称で「自身が decode できないカタログを encode できる」。producer と consumer の両方を仕様に合わせる。

## 現状

- `connectionURI` は任意文字列として decode / encode され、`validate_full_track` でも検証しない。§5.2.37 は valid な MOQT endpoint URI (MOQT §6.1 の `moqt-URI`) を MUST とする。v02 の例は `moqt://` のみで、v01 の例にあった `https://` は MOQT URI から導出される WebTransport 用の URI である。テストも `"not a uri"` の受理を固定している。
- `cipherSuite` は `moq-secure-objects` のとき存在だけを検証し、値が supported cipher suite かを見ない。§4.4.3 は `moq-secure-objects` のとき supported cipher suite value を MUST とし、§5.2.39 は `aes-128-gcm-sha256` / `aes-256-gcm-sha512` / `aes-128-ctr-hmac-sha256-80` を定義する。
- `decode_full_catalog` は §5.1.7 の「`initDataList` は `tracks` 配列より後」の順序 (SHOULD) を検証しない。nojson の `to_object` は文書順で列挙するため、`tracks` と `initDataList` の出現位置を比較して検証できる。
- `validate_lang_tag` は irregular (grandfathered) タグと privateuse の骨格 (`x` + 1 個以上の ASCII 英数字サブタグ) を検査するが、primary 以外の subtag の最大 8 文字、extension singleton の後続、variant / extlang / script / region の構造を検査しない。§5.2.33 は standard tag を MUST とする。
- `initDataList.data` のうち `type` が `inline` のもの (§5.1.7 は Base64 encoded) と `trackBaseKey` (§5.2.41 は base64-encoded) を Base64 として検証しない。`track-property` / `object-property` の `data` は hex notation であり、既に `validate_init_data` が検証している。
- `authInfo` は同一 scheme キーの重複を受理し、encode でも重複メンバーを出力する (RFC 8259 §4 の一意性 SHOULD)。`MsfAuthInfo` の doc は「重複 scheme は順序どおりに保持する」としているため、重複拒否に合わせて更新する必要がある。
- `validate_full_catalog_for_encode` は `catalog.version` を検査しない。手組みの `MsfCatalog { version: "x" }` を encode できるが、同じライブラリの decode は `InvalidCatalogVersion` で拒否する。`framerate` は decode が非有限を拒否する一方、encode は finite を検査せず NaN / inf を `null` として出力する。
- `validate_full_track` / `validate_clone_track_fragment` / `MsfCatalog::apply_delta` の doc の節番号とフィールド名の対応 (§5.2.8 / §5.2.9 / §5.2.18 / §5.2.22 / §5.2.28 / §5.2.29 / §5.2.32 / §5.2.33 / §5.2.35 / §8.2 / §4.3.3) が v02 と一致しない。
- 影響: 不正なカタログを受理・生成する。相互運用で相手の実装に依存した挙動になる。

## 設計方針

- decode / encode の両方で、上記フィールドを §5 の表に沿って検証する。
- clone 断片 (`MsfCloneTrack`) は親から属性を継承し得るため、明示された値だけを検証し、欠如は拒否しない (継承解決後の `MsfTrack` は `validate_full_track` で検証する)。
- `lang` は RFC 5646 §2.1 の `Language-Tag` ABNF 全体 (langtag / privateuse / grandfathered) を構文検証する。RFC 5646 §2.1.1 の大文字小文字無視に従い、privateuse の `X` も受理する (現行の小文字 `x` のみの受理範囲を広げる)。IANA Language Subtag Registry との照合は行わず、その制限を `MsfTrack` / `MsfCloneTrack` の `lang` の doc に明記する。
- `connectionURI` は §5.2.37 と MOQT §6.1 の `moqt-URI` (scheme、非空の host を含む authority、
  path-abempty、query) に従い、msf.rs で RFC 3986 の構文を検証する。scheme は RFC 3986 §3.1 の
  とおり大文字小文字を区別しない (`MOQT://` も受理する)。`moqt://` 以外の scheme
  (v01 の例にあった `https://` を含む) は拒否する。`decode_common_track_fields` の
  「URI 形式検証は行わない」コメントを、msf.rs が検証する契約に更新する。`MsfTrack` /
  `MsfCloneTrack` の `connection_uri` の doc に、msf.rs が `moqt://` の URI 構文を検証する
  契約を明記する。
- `cipherSuite` は `moq-secure-objects` のとき §5.2.39 の定義値 (`aes-128-gcm-sha256` / `aes-256-gcm-sha512` / `aes-128-ctr-hmac-sha256-80`) に限定する。clone 断片では `encryptionScheme` が明示されたときだけ値を検証し、省略時は親から継承し得るため継承解決後の `validate_full_track` に委ねる。
- `authInfo` の同一 scheme キーの重複は decode / encode の両方で拒否する (RFC 8259 §4 の一意性 SHOULD)。decode は `decode_auth_info`、encode は各 track の `auth_info` を走査して検出する (`auth_infos` は全 track を平坦化するため、重複判定には使わない)。
- `initDataList` の順序は、nojson の `to_object` が文書順で列挙することを使って
  `decode_full_catalog` で検証する。`tracks` より前に `initDataList` があるカタログは
  `InvalidCatalog` にする (§5.1.7 の SHOULD は tracks 配列より後のみを求めるため、
  `publishTracks` との前後は問わない)。SHOULD 違反を拒否する契約を
  `MsfCatalogDocument::decode` の doc に明記する。encode は tracks → publishTracks →
  initDataList の順で出力し、SHOULD を満たす。encode 側の §5.1.7 を MUST としている
  コメントも SHOULD に修正する。
- Base64 は RFC 4648 §4 の標準 Base64 (パディングあり) を `base64ct::Base64` で decode 可能か検証する。`inline` の `initDataList.data` と `trackBaseKey` が対象である。
- encode は「自身の decode が受理する文書だけを出力する」契約を doc とテストで固定する。`catalog.version` が `MSF_VERSION` でない場合は `InvalidCatalogVersion` を返し、`framerate` が非有限の場合は `InvalidCatalog` を返す。
- 検証する契約 (Base64、`cipherSuite` の定義値、`connectionURI` の MOQT URI 構文、`lang` の RFC 5646 構文、`framerate` の有限性、`catalog.version` の `MSF_VERSION` 一致、`authInfo` の scheme 一意性) は、該当する公開フィールドと `MsfCatalogDocument::encode` の doc に明記する。
- あわせて `docs/msf.md` の「検証規則」を今回の検証範囲に同期させる (`docs/msf.md` の「inline の Base64 検証は 0342 が担当する」という記述も削除する)。
- `validate_full_track` / `validate_clone_track_fragment` / `MsfCatalog::apply_delta` の doc にある節番号とフィールド名の対応を v02 に合わせる。

## 完了条件

- msf.rs で検証する MUST / SHOULD に対応する negative test があること。
- `catalog.version` が `MSF_VERSION` でない完全カタログの encode が `InvalidCatalogVersion` で拒否されること。
- `framerate` が NaN / inf の手組みの完全カタログと delta の encode が `InvalidCatalog` で拒否されること。
- `authInfo` の重複 scheme を decode / encode の両方で拒否すること (既存の `auth_info_duplicate_scheme_kept_in_order` は重複拒否のテストに置き換える)。あわせて `MsfAuthInfo` の doc を重複拒否に合わせて更新すること。
- `connectionURI` の `"not a uri"` と `https://` を拒否し、`moqt://` と `MOQT://` を受理すること (既存の `connection_uri_invalid_value_accepted` / `connection_uri_invalid_value_accepted_in_clone` を拒否テストに置き換える)。
- `initDataList` が `tracks` より前にある完全カタログが decode で拒否されること。
- `initDataList.data` の `inline` が Base64 でない場合と、`trackBaseKey` が Base64 でない場合に decode / encode が拒否されること。
- `cipherSuite` が `moq-secure-objects` で定義値以外の場合に decode / encode が拒否されること。
- `lang` の大文字 `X` privateuse (`X-FOO` / `X-foo`) を受理するテストを追加し、既存の `lang_uppercase_privateuse_rejected` を置き換えること。
- `lang` の ABNF 違反 (subtag の 9 文字以上、extension singleton の後続なし、不正な variant / extlang / script / region) が拒否されること。
- `lang` の IANA Language Subtag Registry 照合を行わない制限を `MsfTrack` / `MsfCloneTrack` の `lang` の doc に明記すること。
- 検証する契約 (Base64、`cipherSuite` の定義値、`connectionURI` の MOQT URI 構文、`lang` の RFC 5646 構文、`framerate` の有限性、`catalog.version` の `MSF_VERSION` 一致、`authInfo` の scheme 一意性) が該当する公開フィールドと `MsfCatalogDocument::encode` の doc に明記されていること。`docs/msf.md` の「検証規則」が実装と一致していること。
- `validate_full_track` / `validate_clone_track_fragment` / `MsfCatalog::apply_delta` の doc にある節番号とフィールド名の対応が v02 に修正されていること。
- encode 側の §5.1.7 を MUST としているコメントが SHOULD に修正されていること。
- encode したカタログを自身の decode で roundtrip できること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
