# CMSF の Content Protection をカタログで扱う

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

## 目的

CMSF-01 §4 は、CMAF の media に Common Encryption (CENC) を使う場合の signaling を定めている。カタログの root には `contentProtections` を置き、track には `contentProtectionRefIDs` を置く。

現行実装はどちらも未知フィールドとして捨てるので、次の 2 つが起きる。

- CMSF の保護付きカタログを往復できない。
- 購読側が保護 track を判別できない。0472 で moq-sub が cmaf を選べるようにすると、復号できない track を選んでしまう。

本 issue では、Content Protection の signaling を型として扱う。CENC の暗号化・復号そのものは範囲外とする。前提は 0467 (cmaf packaging) である。CMSF 対応全体の方針は 0467 を参照すること。

## 現状

- `src/msf.rs` の `MsfCatalog` / `MsfTrack` / `MsfCloneTrack` には、`contentProtections` / `contentProtectionRefIDs` に当たるフィールドがない。decode では未知フィールドとして無視され (MSF-02 §5 の MUST ignore)、encode では出力できない。
- `decode_delta` は、delta update に含まれる `version` / `isComplete` / `tracks` / `publishTracks` / `initDataList` を拒否する (MSF-02 §5.3)。`contentProtections` は一覧になく、無視される。
- `MsfTrack` の `Debug` は、`connection_uri` と `track_base_key` の値を伏せ、バイト長だけを表示する。

## CMSF-01 と MSF-02 のずれ

| 項目 | CMSF-01 | MSF-02 | 本実装の扱い |
| --- | --- | --- | --- |
| 暗号化の参照 | §4: `[MSF] Section 4.3` | MSF-02 では §4.4 (Content protection and encryption) | MSF-02 §4.4 を参照する |
| delta update と root field | §4.1.1: `contentProtections` は root (Location: R) | §5.3: delta update は他の Root Catalog fields を含めてはならない | `contentProtections` を root field として扱い、delta update に含まれていれば拒否する。変更は独立カタログでだけ行える |
| key 名 | §4.1.1.4.4 (Authorization URL) と §4.1.1.4.6 (Robustness) は JSON key 名を定めない | - | key 名が決まらないので、Authorization URL と Robustness は実装しない。未知 key として無視する |

key 名の出典は次のとおりである。

- §4.3 本文: `systemID` / `laURL` / `pssh`
- §4.1.1.4.2〜§4.1.1.4.4 本文: `url` / `type`
- §5.2 / §5.3 の例: `refID` / `defaultKID` / `scheme` / `drmSystem` / `certURL`

## 設計方針

- `MsfCatalog` に `content_protections: Vec<CmsfContentProtection>` を追加する。
  - CMSF の型には `Cmsf` の接頭辞を付ける (0469 と同じ規則)。
- `CmsfContentProtection` は次を持つ。
  - `ref_id` (`refID`、必須、カタログ内で一意)
  - `default_kids` (`defaultKID`、必須)。CMSF は要素数を定めていないが、1 要素以上を要求する。default_KID を 1 つも示さない entry は意味を持たないためである。
  - `scheme` (`cenc` / `cbcs`、必須)
  - `drm_system` (`drmSystem`、必須)
- `CmsfDrmSystem` は次を持つ。
  - `system_id` (`systemID`、必須)
  - `la_url` (`laURL`)
  - `cert_url` (`certURL`)
  - `pssh`

  `laURL` / `certURL` は、`url` (必須) と `type` (任意) を持つ object とする。
- 値の検証は次のとおりにする。
  - UUID (`defaultKID` の各要素と `systemID`) は `8-4-4-4-12` の 16 進表記であることを検証する。大文字小文字は問わない (実装判断)。
  - `pssh` は Base64 として decode できることを検証する。
- `MsfTrack` に `content_protection_ref_ids: Option<Vec<String>>` (`contentProtectionRefIDs`) を追加する。
  - CMSF §4.1.2 は「When this field is present, the track content is encrypted」としている。そのため、key がない状態と空配列を区別する。空配列は `InvalidCatalog` にする。空配列を受理すると、保護 track を非保護として選んでしまうためである。
  - 各要素は root の `refID` に解決できなければならない。解決できない catalog は `InvalidCatalog` にする。
  - 解決の検証は、initRef と同じく、完全カタログの decode / encode と delta 適用後に行う。対象は `tracks` と `publishTracks` の両方である。
- `contentProtectionRefIDs` は、cmaf 以外の track では解釈しない。0467 の SAP starting type と同じ規則 (共通の判定関数) を使う。
  - decode では捨てる。encode では拒否する。
  - clone / update の `MsfCloneTrack` には `content_protection_ref_ids: Option<Vec<String>>` を追加し、`None` を親からの継承とする。packaging が明示されていて cmaf 以外のときだけ、捨てる / 拒否する。
  - 継承の解決後に cmaf でなければ落とす。
- clone / update でも空配列は `InvalidCatalog` にする。decode / encode の時点で拒否する。
  - 空配列を「非保護への上書き」として受理すると、保護された親から非保護の track を作れてしまい、0472 の moq-sub がそれを選ぶためである。
  - 保護の有無を変えたいときは、`contentProtectionRefIDs` を持たない track を add する。
- CMSF §4.1.1 は「Content protection information MUST NOT be duplicated at the track level」と定めている。
  - cmaf の track object に `contentProtections` key があれば、`InvalidCatalog` にする。この key は本実装が理解する root field なので、MSF-02 §5 の MUST ignore とは衝突しない。
  - 対象は次のとおりである。
    - `tracks` / `publishTracks`
    - delta の add
    - delta の clone / update。packaging が cmaf と明示されているか、省略されている場合に限る
  - packaging を省略した clone / update は、継承によって cmaf になり得る。そのため、継承の解決を待たずに拒否する。
  - packaging が cmaf 以外と明示された track object では、CMSF のフィールドを解釈しない規則に従い、未知フィールドとして無視する。
- `decode_delta` の root field 拒否一覧に `contentProtections` を加える。
- `Debug` では、`laURL` / `certURL` の `url` と `pssh` の値を伏せ、バイト長だけを表示する。URL は token を含み得るためで、既存の `connection_uri` / `track_base_key` と同じ扱いである。
- 次の 2 つは検証しない。どちらも ISO BMFF の解析が要る。
  - ClearKey (§4.3) の SHOULD (laURL、version 1 の PSSH)
  - 保護 track の init data に sinf / schm / schi / tenc を含める MUST (§4.2)
- CENC による暗号化・復号と、DRM のライセンス取得は範囲外とする。
- `docs/msf.md` の CMSF の節に、次を追記する。
  - Content Protection の扱い
  - 上記の「ずれ」の判断。具体的には、delta update で `contentProtections` を拒否すること、Authorization URL と Robustness を実装しないこと
  - 範囲外の項目

## 完了条件

- `contentProtections` と `contentProtectionRefIDs` を含む完全カタログが、decode / encode で往復一致すること。
  - CMSF-01 §5.2 / §5.3 の例も同様であること。ただし例は version を `"draft-02"` にし、`...` で省略された `data` / `pssh` を有効な Base64 に置き換えたものとする。
- 次の不正が `InvalidCatalog` になること。
  - `refID` の重複
  - 必須フィールドの欠如
  - `defaultKID` が空
  - `scheme` が `cenc` / `cbcs` 以外
  - UUID の書式違反
  - `pssh` が Base64 でない
  - `contentProtectionRefIDs` が空配列 (`tracks` / `publishTracks` / delta の add / clone / update)
  - `contentProtectionRefIDs` が解決できない
  - cmaf の track object の `contentProtections` (`tracks` / `publishTracks` / delta の add、および packaging が cmaf または省略の clone / update)
- `contentProtections` を含む delta update が拒否されること。delta で追加した cmaf track の `contentProtectionRefIDs` が、既存カタログに対して解決されること。
- packaging が cmaf 以外と明示された track object の `contentProtections` が無視されること。
- cmaf 以外の track の `contentProtectionRefIDs` について、次のとおりになること。
  - decode では捨てられ、encode では拒否されること
  - clone / update の継承規則が 0467 と同じであること
- `Debug` 出力に、`laURL` / `certURL` の `url` と `pssh` の値が出ないこと。
- 上記を `tests/test_msf` で固定し、`pbt/tests/prop_msf.rs` の生成器に `contentProtections` を加えること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
