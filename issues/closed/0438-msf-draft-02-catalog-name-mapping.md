# MSF カタログの名前フィールドを v02 の '/' 区切りとバイト比較に合わせる

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 の §5.2.2 (Track namespace)、§5.2.3 (Track name)、§11.2.1〜§11.2.5 がカタログ上の名前表現を変更した。§11.2 は §5.2.2、§5.2.3、§5.2.15 (`depends` の String 要素)、§5.2.34 (`parentName`)、§5.2.35 (`parentNamespace`) を mapping 対象とする。

- `namespace` は `/` 区切りの string。1〜32 fields、各 field は非空。空文字、先頭・末尾の `/`、連続する `/`、32 fields 超はカタログを invalid にする (MUST)。宣言した namespace は継承した namespace を拡張せず置換する。
- `name` は空であってはならない (MUST NOT)。一意性は Full Track Name (namespace + name) で判定する (MUST)。
- mapping は Unicode scalar values の UTF-8 符号化で、正規化・case folding をしない。NUL と未対サロゲートを禁じ、カタログ文書の先頭に BOM を置かない (MUST)。比較は mapped bytes の完全一致で行う (MUST)。名前中の U+FEFF は禁止されない。

現行実装は §8.8 の `-` 区切り表現をカタログにも使っており、v02 の `/` 区切りを解釈できない。

## 現状

- `MsfTrack` / `MsfCloneTrack` / `MsfRemoveTrack` の `namespace` は `Option<String>` を保持するだけで、`/` 分割・field 数検証・`TrackNamespace` への変換を行わない。`decode_common_track_fields` は JSON string であることだけを確認する。
- `MsfTrack.name` / `MsfCloneTrack.parent_name` / `MsfRemoveTrack.name` は空文字を受理する。`decode_track` は `name` の存在だけを要求し、`validate_track_name_uniqueness` は空名 1 件を受理する。
- `crates/moq-sub` の `resolve_track_namespace` はカタログの `namespace` を `parse_namespace` (§8.8 の `-` 区切り) として解釈する。`"customer/livestream/123"` は `/` が §8.8 のエスケープではないため `InvalidEscape` になり、解決に失敗する。逆に v02 の単一 field に `-` を含む値 (例 `"a-b"`) は 2 fields に分割され、静かに別の Track Namespace になる。
- `crates/moq-pub` はカタログの `namespace` に `serialize_namespace` の `-` 区切り形式を書き込む。
- `crates/moq-sub` の `CatalogState::new` は catalog track の namespace を `serialize_namespace` の `-` 形式で渡し、delta の namespace 継承・一致判定に使う。明示された `/` 形式と継承値の表現が一致しない。
- `validate_track_name_uniqueness` は生の `(namespace, name)` で比較し、継承 namespace を解決しない。基準 namespace が分かる文脈でも、明示 namespace と省略 namespace の重複を検出できない。
- NUL の検査は decode にも encode にもない。`\u0000` を含む name / namespace を受理し、encode も出力する。
- BOM と未対サロゲートは `nojson` の JSON パーサが拒否し、`MessageError::InvalidCatalog` になる。挙動を固定するテストはない。
- `MSF_VERSION` は 0437 まで `"draft-01"` のため、`"version":"draft-02"` のカタログは `InvalidCatalogVersion` で拒否される。0438 のテストは draft-01 の version で namespace だけ v02 形式にして検証する。

## 設計方針

- カタログ用の namespace parser を `src/msf.rs` に追加する。`/` で分割し、1〜32 fields、各 field 非空、NUL を含まないことを検証して `TrackNamespace` に変換する。先頭・末尾・連続する `/`、0 fields、4096 bytes 超を拒否する。`TrackNamespace::new` の `InvalidTrackNamespace` / `FieldLengthExceeded` は `MessageError::InvalidCatalog` に写像する。
- カタログ用の name mapping を追加する。空文字と NUL を拒否し、それ以外は UTF-8 バイト列をそのまま返す。
- decode と encode の両方で検証する。対象は `MsfTrack` の `namespace` / `name`、`MsfCloneTrack` の `namespace` / `name` / `parent_name` / `parent_namespace`、`MsfRemoveTrack` の `namespace` / `name`、`depends` の String 要素、`publishTracks` の track である。`depends` の型変更は 0445 が担当するため、0438 では既存の String 要素の検証に限定する。
- `serialize_namespace` / `parse_namespace` は §8.8 の表現で、URL と DPoP の Authorization Context でも使うため変更しない。カタログ用に UTF-8 の各 field を `/` で連結するシリアライザを追加し、`crates/moq-pub` のカタログ生成と `crates/moq-sub` の `CatalogState::new` がこれを使う。
- field に `/` を含む `TrackNamespace` はカタログで表現できないため、publisher 側で拒否する。`receive_catalog` は `CatalogState::new` にこの表現を渡し、`CatalogState` の doc を更新する。
- カタログ内の比較は、継承を解決した後の `TrackNamespace` または mapped bytes で行う。基準 namespace を受け取って Full Track Name の一意性を検証する API を追加し、`MsfCatalog::track_key` / `find_track_index` / `apply_delta` と `crates/moq-sub` の `CatalogState` から使う。
- 基準 namespace はカタログトラックの MOQT namespace で、JSON だけからは決まらない。MOQT の生バイト列とカタログ文字列を直接比較する API は現行にないため、比較は `TrackNamespace` のバイト等価で行い、新しい比較経路は作らない。
- BOM と未対サロゲートの拒否は `nojson` の挙動に依存するため、テストで固定する。
- 実施順序: 0438 は 0437 より前に実施する。0437 が入るまでは `MSF_VERSION` が `"draft-01"` のため、テストは draft-01 の version で namespace だけ v02 形式にする。

## 完了条件

- `"customer/livestream/123"` が 3 fields の `TrackNamespace` に変換され、URL 形式 `customer-livestream-123--catalog` と往復一致すること。
- 空文字、`"a//b"`、`"/a"`、`"a/"`、33 fields、NUL を含む field、4097 bytes 超の namespace が `MessageError::InvalidCatalog` で拒否されること。
- 空の `name` / `parentName` / `removeTracks` の `name` が拒否されること。
- `depends` の String 要素に空文字または NUL が含まれる場合に拒否されること。
- 基準 namespace を渡した検証で、明示 namespace と省略 namespace の同名 track が同じ Full Track Name として解決され、重複として拒否されること。namespace が異なる同名 track は別 track として受理されること。
- encode が空文字・NUL・不正な namespace を含むカタログを出力しないこと (decode と同じ検証を encode でも行う)。
- 先頭に BOM を付けたカタログと `"\uDEAD"` を含むカタログが `MessageError::InvalidCatalog` で拒否されること。名前中の U+FEFF は受理されること。
- 多フィールド namespace の完全カタログに対し、namespace を省略した delta の add / remove / clone が同じ Full Track Name として解決されること (`crates/moq-sub/src/catalog.rs` のテスト)。
- `crates/moq-pub` のカタログ JSON が `/` 区切りで出力され、`crates/moq-sub` が同じ `TrackNamespace` に解決すること。cross-crate の検証は `e2e-tests` または各 crate のテストに置く。
- `pbt/tests/prop_msf.rs` の `parent_namespace` 生成器が v02 の namespace 制約を満たすこと。
- `crates/moq-sub` の invalid namespace テストが v02 の `/` 構文に合っていること。
- `tests/test_msf` の `catalog_decode.rs` / `encode_decode_roundtrip.rs` / `delta_apply.rs` に上記を固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/msf.rs` にカタログ用の名前写像 API を追加した。
  - `parse_catalog_namespace`: `/` 区切りの JSON 文字列を `TrackNamespace` に写像する。1 〜 32 fields、各 field 非空、NUL 禁止、namespace 単体 4,096 バイト以下を検証する。
  - `serialize_catalog_namespace`: `TrackNamespace` を `/` 区切りへ戻す。UTF-8 でない field、NUL、`/` を含む field は表現できないため拒否する。
  - `catalog_track_name_bytes`: track name を UTF-8 バイト列に写像し、空文字列と NUL を拒否する。
  - `MsfCatalog::validate_track_names`: 基準 namespace を解決した Full Track Name の一意性と 4,096 バイト制限を検証する。
- `MsfCatalog::apply_delta` の基準 namespace を `Option<&str>` から `Option<&TrackNamespace>` へ変更し、`track_key` / `find_track_index` / `removed_tracks` の比較をカタログ文字列ではなく写像後のバイト値に統一した。
- decode / encode / apply の各経路で、`name` / `namespace` / `parentName` / `parentNamespace` / `depends` の写像規則と Full Track Name の長さを検証するようにした。clone の namespace 省略時は親トラックの namespace を継承する §5.1.6 の規則を文書化し、テストで固定した。
- `crates/moq-sub` の `CatalogState` は `TrackNamespace` を保持し、Full カタログの適用時にも継承を解決した一意性を検証する。`resolve_track_namespace` は §8.8 表現ではなく v02 の `/` 区切りを解決する。
- `crates/moq-pub` はカタログの namespace を `serialize_catalog_namespace` で `/` 区切りに変換し、表現できない namespace を設定エラーとして拒否する。
- `pbt/tests/prop_msf.rs` の namespace 生成器を v02 の制約に合わせ、通常トラック・clone の namespace と JSON エスケープを要する field を生成するようにした。`fuzz/fuzz_targets/fuzz_msf_apply_delta.rs` も新しい `apply_delta` の引数型に追従した。
- `docs/msf.md` と `skills/shiguredo-moqt/SKILL.md` に新しい写像 API、`removed_tracks` のキー、Full Track Name の長さ、clone の namespace 継承規則を追記した。
- `tests/test_msf` に次を追加した。
  - namespace の `customer/livestream/123` から URL 形式 `customer-livestream-123--catalog` への往復
  - 不正 namespace / name / depends / parentNamespace / publishTracks の拒否
  - BOM と未対サロゲートの拒否、U+FEFF の受理
  - JSON エスケープ等価、Unicode 正規化なし、`-` が区切りでないこと
  - Full Track Name 4,096 / 4,097 バイトの境界
  - multi-field namespace の delta add / remove / clone
  - `crates/moq-sub` と `crates/moq-pub` のテスト追従

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
