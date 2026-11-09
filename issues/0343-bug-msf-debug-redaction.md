# MSF の公開型の Debug 出力から token / trackBaseKey / authInfo / URI の機密値を隠す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-11-10

## 目的

MSF の公開型の `Debug` 出力には、bearer token、trackBaseKey、authInfo、token を含み得る URI (fragment パラメータ、query、`connectionUri`) の値が現れる。`{:?}` をログに出すと機密情報が漏れる。C4M 側の `CatToken` / `CoseKey` は秘密値を長さだけ表示する独自 `Debug` を実装しており、MSF 側も同じ方針に揃える。

## 現状

- `src/msf.rs` の `MsfTrack` / `MsfCloneTrack` は `token` / `track_base_key` (いずれも `Option<String>`) / `auth_info` (`Option<Vec<MsfAuthInfo>>`) を直接持ち、`derive(Debug)` である。`token` / `track_base_key` は `Some("...")` として文字列のまま出る。
- `MsfAuthInfo` は `scheme` と `value_raw` (`Vec<u8>`) を持ち、`value_raw` には scheme 固有の生 JSON を保持し、`derive(Debug)` である。`value_raw` は十進のバイト列 (`value_raw: [34, 88, 89, ...]`) として出るため文字列としては現れないが、値は復元できる。
  - draft-ietf-moq-msf-01 §5.2.43 の例 (`"cat": "%token%"`) のとおり、`resolve_catalog_variables` が decode 前に変数を実値へ置換するため、decode 後の `value_raw` に token の実値が入る。
- `src/msf/uri.rs` の `MsfFragmentParameter` は `name` / `value` (`String`) を持ち、`derive(Debug)` である。`MsfFragment::parameters` と `MsfUri::fragment` を通じて `MsfFragment` / `MsfUri` の `derive(Debug)` に値が現れる。
  - draft-ietf-moq-msf-01 §11.1.1 の予約パラメータ `c4m` は "A base64 encoded C4M token" であり、§5.2.43 の例 `#namespace--name&token=XYZ789` のように任意名のパラメータでも token が届く。§11.4.2 は "The parameter name and format are deployment-specific." とするため、名前で機密かどうかを判別できない。
- §11.4.2 は "Tokens MAY also be supplied as parameters in the connection URI." とし、token の置き場所を fragment に限っていない。`MsfUri::query` (`Option<String>`) と、`MsfTrack` / `MsfCloneTrack` の `connection_uri` (§5.2.36 の endpoint URI、`Option<String>`) は生の文字列のまま `Debug` に出る。
- `MsfCatalog` / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` は上記の型を内側に持つ公開コンテナで、`derive(Debug)` の出力には内側の値が現れる。
  - `MsfCatalog` は `tracks` / `publish_tracks` / `removed_tracks` に `MsfTrack` を持つ。
  - `MsfDeltaUpdate` は `MsfDeltaOperation` を持ち、`MsfDeltaOperation::Add` / `Clone` は `MsfTrack` / `MsfCloneTrack` を持つ。
  - `MsfCatalogDocument` は `MsfCatalog` / `MsfDeltaUpdate` を持つ。
- `MsfCatalog` の構造体 doc は `removed_tracks` が `Debug` に含まれると明記しており、この挙動を変えてはいけない。
- `src/c4m/cat.rs` の `CatToken` と `src/c4m/crypto.rs` の `CoseKey` は、秘密値を長さだけ表示する独自 `Debug` を実装している。表示形式は型の形で次の 2 通りである。
  - `Option` でないバイト列は、フィールド名に `_bytes` を付けて長さを出す (`CatToken` の `raw_bytes` / `payload_bytes` / `signature_bytes`、`CoseKey::Symmetric` の `key_bytes`)。
  - `Option` のバイト列は、フィールド名を変えずに `Option<usize>` を出す (`CoseKey::Ec2` / `CoseKey::Okp` の `private_key`)。
- `tests/test_msf.rs` は `#[path]` で `tests/test_msf/*.rs` を束ねており、MSF の型の `Debug` を検証するテストは無い。
- 影響: example や利用者がカタログ、トラック、MSF URI を `{:?}` でログ出力すると、bearer token と鍵材料が漏れる。現時点で `crates/` に MSF の型を `{:?}` で出力している箇所は無い。

## 設計方針

- 実値を直接持つ `MsfTrack` / `MsfCloneTrack` / `MsfAuthInfo` / `MsfFragmentParameter` / `MsfUri` の 5 型に `core::fmt::Debug` を実装する。表示形式は `CatToken` / `CoseKey` の前例に型の形で揃える。
  - `MsfTrack` / `MsfCloneTrack` の `token` / `track_base_key` / `connection_uri` は、フィールド名を変えずにバイト長を `Option<usize>` で表示する (`token: None` / `token: Some(12)`)。`connection_uri` は §11.4.2 のとおり token パラメータを含み得るため隠す。
  - `MsfTrack` / `MsfCloneTrack` の `auth_info` は `MsfAuthInfo` の `Debug` をそのまま使う。`MsfAuthInfo` は `scheme` を表示し、`value_raw` は `value_raw_bytes: <長さ>` として表示する。
  - `MsfFragmentParameter` は `name` を表示し、`value` は名前にかかわらず `value_bytes: <長さ>` として表示する。パラメータ名は配備ごとに決まり (§11.4.2)、名前で機密かどうかを判別できないため、すべての値を隠す (fail-closed)。
  - `MsfUri` は `query` をフィールド名を変えずにバイト長の `Option<usize>` で表示し、`authority` / `path` は従来どおり表示する。`fragment` は `MsfFragment` の `Debug` (パラメータの値は `MsfFragmentParameter` で隠れる) をそのまま使う。query のパラメータも名前で機密かどうかを判別できないため、すべて隠す。
  - 上記以外のフィールドは従来どおり表示する。
- 独自 `Debug` は `let Self { name, namespace, ... } = self;` のように `..` を使わずに全フィールドを分解してから `debug_struct` に渡す。フィールドの書き漏れと、将来追加したフィールドの表示漏れをコンパイラが検出できるようにするため。
- `MsfCatalog` / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` / `MsfFragment` は `derive(Debug)` のままとする。内側の型の `Debug` を通じて実値が隠れるため、独自 `Debug` は不要である。`MsfCatalog` の `removed_tracks` も `MsfTrack` の `Debug` を通じて実値が隠れ、`Debug` に含まれるという既存の doc の挙動は維持される。
- テストは `tests/test_msf/` に新しいファイルを追加し、`tests/test_msf.rs` に `#[path]` の `mod` を追加する。判定は次の 3 つで行う。
  - 実値の文字列 (`token` / `track_base_key` / `connection_uri` / `query` / fragment パラメータの `value` の文字列) が含まれないこと。
  - `value_raw` の十進のバイト列表現 (`format!("{:?}", value_raw)` の結果) と `value_raw: [` が含まれないこと。`MsfAuthInfo` は修正前でも実値の文字列が現れないため、文字列の否定だけでは修正前のコードでも通ってしまう。
  - 長さの表示 (`token: Some(<長さ>)`、`value_raw_bytes: <長さ>`、`value_bytes: <長さ>`) と、非機密フィールドの代表 (`name` / `codec` / `scheme` など) が含まれること。

## 変更対象

- `src/msf.rs`: `MsfTrack` / `MsfCloneTrack` / `MsfAuthInfo` の `Debug`
- `src/msf/uri.rs`: `MsfFragmentParameter` / `MsfUri` の `Debug`
- `tests/test_msf.rs` と `tests/test_msf/` 配下の新しいテストファイル

## 完了条件

- `MsfTrack` / `MsfCloneTrack` / `MsfAuthInfo` / `MsfFragmentParameter` / `MsfUri` と、これらを内側に持つ `MsfCatalog` (`removed_tracks` を含む) / `MsfDeltaUpdate` / `MsfCatalogDocument` / `MsfDeltaOperation` / `MsfFragment` の `{:?}` に、token / trackBaseKey / authInfo / connectionUri / URI の query / fragment パラメータの値が、文字列としても十進のバイト列としても含まれないこと。
- 隠したフィールドは設計方針の形式で長さを表示し、非機密フィールドは従来どおり `Debug` に表示されること。`MsfCatalog` の `Debug` は `removed_tracks` を含む既存の doc の挙動を維持すること。
- 独自 `Debug` が `..` を使わずに全フィールドを分解していること。
- 上記を設計方針の 3 つの判定でテストし、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 対象外

- `parse_fragment_pairs` の戻り値 (`Vec<(String, String)>`) と `resolve_catalog_variables` の戻り値 (`Vec<u8>`) は標準の型であり、独自 `Debug` で覆うには newtype への API 変更が必要になるため扱わない。
- `crates/tokio-moq/src/lib.rs` の `ServerUrl` は `derive(Debug)` で、fragment の生文字列を持つ `MoqtFragment::value` と、C4M token のバイト列を持つ `c4m_tokens` を表示する。MSF の公開型ではなく example 側の型のため、本 issue では扱わない。
