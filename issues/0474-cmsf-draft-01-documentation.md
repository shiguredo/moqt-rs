# README や SKILL.md などの文書に CMSF draft-01 対応を記載する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23
- Refreshed: 2026-11-23

## 目的

CMSF 対応 (0467〜0472) が揃ったら、利用者向けの入口になる文書と、対応仕様を名乗る crate doc やモジュール doc に CMSF draft-01 を載せる。0437 (MSF の版表記) や 0420 (C4M の文書) と同じく、機能の issue とは分けて最後に更新する。

開始条件に 0473 (prft による壁時計の同期) は含めない。CMSF-01 は prft を定めておらず、0473 は moq-sub の実装上の選択である。0473 がなくても、CMSF を対応仕様として載せる内容に偽りはない。0473 は、着手前の確認ができなければ pending になり得る。

次の文書は、それぞれの機能の issue が自分の分を更新するので、本 issue の対象外である。

- `docs/msf.md` の CMSF の節 (0467 が新設し、0469〜0473 が追記する。0468 は Catalog の節を直す)。ただし、CMSF の節の外にある冒頭の対応仕様の文は本 issue で直す
- `crates/moq-pub/README.md` (0471)
- `crates/moq-sub/README.md` (0472)
- `crates/moq-codec` の crate doc (0473)

## 現状

- `README.md` の「対応仕様」には、MoQT draft-23 / LOC draft-05 / MSF draft-02 / C4M draft-02 が ``[表題](URL) `draft-NN` `` の形で載っている。CMSF は載っていない。LOC の行は、表題 (「Low Overhead Media Container」) を縮めて「Low Overhead Container」にしている。
- `skills/shiguredo-moqt/SKILL.md` は、ライブラリ `shiguredo_moqt` の機能と API のリファレンスである。moq-pub / moq-sub の CLI は扱っていない。CMSF の記載はない。対応仕様に関わる記載は次の 4 か所にある。
  - frontmatter の description の「LOC / MSF の codec」。スキルが起動する条件にもなる
  - 特徴の箇条の「**対応仕様**: MOQT `draft-23` / LOC `draft-05` / MSF `draft-02` / C4M `draft-02`」
  - モジュール構成の表の `msf` 行の「MSF Catalog / Timeline / URI」
  - 「## 準拠仕様」の節。MSF の行は「draft-ietf-moq-msf-01 - MOQT Streaming Format」と、その URL のままである。0437 は完了条件で SKILL.md の MSF の版表記と URL を draft-02 にするとしていたが、この節を更新し損ねている
- `skills/shiguredo-moqt/SKILL.md` の MSF の API の記載:
  - 型の一覧は「`MsfCatalog` / `MsfDeltaOperation` / `MsfDeltaUpdate` / `MsfTrack` / `MsfCloneTrack` / `MsfUpdateTrack` / `MsfRemoveTrack` / `MsfInitData` / `MsfBuffers` / `MsfPackaging`」と、定数 `MSF_CATALOG_TRACK_NAME` / `MSF_VERSION` である。
  - ライブラリは re-export をしないので、SKILL.md は「型は定義元モジュールから直接 import すること」としている。`msf` モジュールには `src/msf.rs` 本体のほかに、サブモジュール `uri` / `log` / `metrics` (`src/msf/`) がある。
  - `MsfTrack` のフィールドは「`name` / `namespace` / … などを持つ単一構造体」と書かれている。`MsfCloneTrack` のフィールドの記載はない。
  - `MsfTrack::stream_mapping` と `StreamMapping` の記載はない。
- 対応仕様を名乗る記載が、ほかに次の場所にある。どれも CMSF を含まない。
  - `crates/README.md`: 「draft-ietf-moq-transport-23、draft-ietf-moq-loc-05、draft-ietf-moq-msf-02、draft-ietf-moq-c4m-02 に準拠しています。」
  - crate doc: `crates/moq-pub/src/lib.rs`、`crates/moq-pub/src/main.rs`、`crates/moq-sub/src/lib.rs`、`crates/moq-sub/src/main.rs` の「draft-ietf-moq-transport-23、draft-ietf-moq-loc-05、draft-ietf-moq-msf-02 に準拠。」
  - モジュール doc: `src/msf.rs` の「draft-ietf-moq-msf-02 に基づく。」と、`src/lib.rs` の `msf` モジュールの「MOQT Streaming Format のカタログとタイムラインのコーデック (draft-ietf-moq-msf-02)」
  - `docs/msf.md` の冒頭: 「[draft-ietf-moq-msf-02](…) の実装状況です。」
  - `docs/missing-features.md`: 対象仕様の段落に CMSF がない。
    - 第 3 節「ライブラリの API 露出と example での未使用」の表は、「`src/` の公開 API のうち `crates/` から呼ばれていないもの、および draft の MAY に対応していないもの」を収録している。
    - その表の「`initDataList` の `inline` / `track-property` 型、`initRef` の読み取り」の行 (`MsfInitDataKind::Inline` / `TrackProperty`、`MsfTrack::init_ref`、MSF §5.1.7 / §5.2.14 / §12.2) は、「moq-pub は `object-property` 型の entry と initRef だけを載せる (5.14)。moq-sub は initRef を読まず、in-band の Video Config / Audio Config を使う」と書いている。
    - 0471 は cmaf の track の init segment を initDataList の inline entry に入れて initRef から参照し、0472 はそれを読む。そのため、完了後は cmaf の track で `MsfInitData` (`MsfInitDataKind::Inline`) と initRef が両方の crate から使われる。
    - 一方、`MsfInitDataKind::TrackProperty` の entry は、完了後もどちらの crate からも使われない。
    - loc の track の initRef と `ObjectProperty` の entry は moq-pub が載せる (`crates/moq-pub/src/catalog.rs`。Video Config を Object に載せない VP8 / VP9 の映像 track を除く) が、moq-sub は loc の track の initRef を読まない。0472 は LOC 経路を変えないので、loc の track では in-band の Video Config / Audio Config を使うままである。
    - 0471 / 0472 の完了で、ほかにも次の記述が事実と食い違う。
      - 5.12: 「`crates/moq-pub/src/catalog.rs` が設定するのは name / namespace / … / authInfo である。」loc の track の initRef と root の `object-property` 型の initDataList がすでに加わっているので、現状でも食い違っている。0471 で、cmaf の track の initRef と root の `inline` 型の entry が加わる
      - 6.5: 「トラックを選ぶ条件は codec の一致だけで、`packaging` (§5.2.4) と `isLive` (§5.2.8) を判定していない。」現状でも `is_loc_packaging` で loc 以外を除外しているので食い違っており、0472 で cmaf の track も選ぶようになる
      - 6.13: 「録画は MP4 のみで音声は Opus に限る (AAC は警告して音声を録画しない)。」0472 で、cmaf の track では AAC / VP8 / VP9 も録画する
      - 5.14: 「Video Config / Audio Config を Object に載せる track (VP8 / VP9 の映像を除く) に `object-property` 型の entry を指す initRef を載せる」。0471 の cmaf の track は Object に LOC property を付けず、object-property の initRef も載せないので、loc の track に限る記述になる
    - 0467〜0470 で増える CMSF の公開 API のうち、次は完了後も `crates/` から呼ばれない。
      - `MsfTrack` / `MsfCloneTrack` の `max_grp_sap_starting_type` / `max_obj_sap_starting_type`。0471〜0473 は設定も参照もしない
      - SAP Type timeline の型と相互変換。0469 は moq-pub / moq-sub での発行と購読を範囲外にしている
      - `MsfCatalog::content_protections` と `Cmsf*` の Content Protection の型。0471 は Content Protection の発行を範囲外にしており、0472 は track の `contentProtectionRefIDs` で除外するだけで root を読まない
- `refs/moq/draft-ietf-moq-cmsf-01.txt` の表題は「CMSF- a CMAF compliant implementation of MOQT Streaming Format」である。

## 設計方針

### 仕様の表記

- 表題の先頭の「CMSF- a」は除き、「CMAF compliant implementation of MOQT Streaming Format」と書く (実装判断)。LOC の行と同じく、表題を読みやすく縮める。
- `README.md` の対応仕様の行は、``[CMAF compliant implementation of MOQT Streaming Format](https://datatracker.ietf.org/doc/html/draft-ietf-moq-cmsf-01) `draft-01` `` とする。MSF の行の次に置く。
- 準拠を名乗る文 (`crates/README.md`、crate doc、モジュール doc) には、既存の書き方に合わせて `draft-ietf-moq-cmsf-01` を加える。

### SKILL.md

- 対応仕様に関わる 4 か所を更新する。
  - description: 「LOC / MSF / CMSF の codec」にする。CMSF についての質問でもスキルが起動するようにするためである。
  - 特徴の箇条の対応仕様: MSF の後に CMSF `draft-01` を加える。
  - モジュール構成の表の `msf` 行: CMSF の型も持つことが分かるように、「MSF / CMSF Catalog / Timeline / URI」にする。
  - 準拠仕様の節: 「draft-ietf-moq-cmsf-01 - CMAF compliant implementation of MOQT Streaming Format」と URL を加える。
- 準拠仕様の節の MSF の行を、draft-ietf-moq-msf-02 と、その URL に直す。0437 の取り残しで、CMSF を追記する行のすぐ隣にあるためである。
- API の記載は、機械的な基準で決める。
  - 0467〜0470 で `src/` に追加・削除・変更された pub 項目をすべて記載する。pub 項目には、型、enum の variant、定数、関数、メソッド、フィールドを含み、変更にはシグネチャとフィールドの型の変更を含む。実装した `src/` の差分から拾い、issue の記述からは拾わない。関数名や `scheme` の型、新しい型の置き場所のように、issue の時点では決まっていないものがあるためである。
  - CMSF の型が `msf` モジュールの外 (新しいモジュールなど) に置かれた場合は、モジュール構成の表に行を加え、`src/lib.rs` のそのモジュールの doc にも `draft-ietf-moq-cmsf-01` を書く。
  - 既存の記載の粒度に合わせて、次のように書く。
    - 型と定数は、型の一覧の行に加える。
    - `MsfTrack` に追加したフィールドは、フィールド一覧に加える。
    - `MsfCloneTrack` に追加したフィールドは、1 行で書く。そのとき、`None` が親からの継承であることを添える。
    - 関数とメソッドは、名前と入出力の型、失敗するときのエラーを 1 行で書く。
    - enum に追加した variant (`MsfPackaging::Cmaf` など) は、型の一覧の enum 名に variant を添えて書く。
    - `MsfCatalog` に追加したフィールドは、1 行で書く。
    - 新しい型のフィールドは、型ごとに 1 行で、フィールド名と必須かどうかを書く。
    - サブモジュール (`msf::uri` など) に置かれた項目には、import のパスを添える。re-export がないためである。
  - 少なくとも次が含まれるはずである。これは基準による拾い漏れを確かめるための目安で、閉じた一覧ではない。
    - 0467: `MsfPackaging::Cmaf`、`MsfTrack` / `MsfCloneTrack` の `max_grp_sap_starting_type` / `max_obj_sap_starting_type`
    - 0468: `MsfTrack::stream_mapping` の戻り値の変更。`Result<Option<StreamMapping>, MessageError>` になり、loc 以外の packaging では `Ok(None)` を返す。SKILL.md には元の記載がないので、`StreamMapping` とともに新しく書き起こす
    - 0469: `CmsfSapTypeRecord`、`CMSF_SAP_TYPE_EVENT_TYPE`、`MsfEventTimeline` と record の列の相互変換 (2 方向)
    - 0470: `CmsfContentProtection`、`CmsfDrmSystem`、`CmsfDrmUrl`、`MsfCatalog::content_protections`、`MsfTrack` / `MsfCloneTrack` の `content_protection_ref_ids`
- CLI の記載は加えない。CLI は `crates/moq-pub/README.md` / `crates/moq-sub/README.md` が扱う。

### ほかの文書

- `crates/README.md`、crate doc (`crates/moq-pub/src/lib.rs`、`crates/moq-pub/src/main.rs`、`crates/moq-sub/src/lib.rs`、`crates/moq-sub/src/main.rs`)、モジュール doc (`src/msf.rs`、`src/lib.rs` の `msf` モジュール) の準拠の文に、`draft-ietf-moq-cmsf-01` を加える。
- `docs/missing-features.md`:
  - 対象仕様の段落に、CMSF `draft-ietf-moq-cmsf-01` を加える。
  - 「`initDataList` の `inline` / `track-property` 型、`initRef` の読み取り」の行は削除せず、完了後も使われない範囲に絞って書き直す。
    - 完了後も、`MsfInitDataKind::TrackProperty` はどちらの crate からも使われず、loc の track の initRef は moq-pub が載せるが moq-sub は読まないので、表の収録基準に当たり続けるためである (0467 は cmaf の initRef の参照先を inline に限り、0471 / 0472 も cmaf の track では inline だけを使う)。
    - 機能: `track-property` 型の initDataList entry と、loc の track の `initRef` の読み取り
    - API: `MsfInitDataKind::TrackProperty`、`MsfTrack::init_ref` (loc の track の読み取り)
    - 仕様: MSF §5.1.7 / §5.2.14 / §12.2 (現在の列のまま)
    - 影響: moq-pub は loc の track に `object-property` 型の entry を指す initRef を載せる (5.14) が、moq-sub は読まず、in-band の Video Config / Audio Config を使う。cmaf の track では moq-pub / moq-sub とも inline entry の init segment を使う。`track-property` 型はどちらの crate も使わない
  - 0471 / 0472 の完了で事実と食い違う記述 (現状に挙げた 5.12 / 5.14 / 6.5 / 6.13) を、完了後の挙動に合わせて直す。
  - 第 3 節の表に、完了後も `crates/` から呼ばれない CMSF の公開 API (現状に挙げた SAP starting type のフィールド、SAP Type timeline、Content Protection) の行を加える。第 3 節の収録基準 (`crates/` から呼ばれていない公開 API) に当たるためである。仕様の列は CMSF-01 の節番号 (§3.5.2 / §3.6 / §4) で書く。
  - 0473 (prft による壁時計の同期) の有無に関わる cmaf の時刻の制約は、第 6 節に書かない (実装判断)。記述が 0473 の完了の有無で変わり、0473 が後から完了したときに誰が消すかの引き継ぎが要るので、0474 の中で閉じないためである。
  - 本 issue で書き直す箇所 (表の initDataList の行、新しく加える行、5.12 / 5.14 / 6.5 / 6.13 の節) の中にある節番号は、すべて MSF-02 / CMSF-01 の番号にする。
- `docs/msf.md` の冒頭の対応仕様の文に、`draft-ietf-moq-cmsf-01` (CMSF の節) を加える。

## 完了条件

- `README.md` の対応仕様に、設計方針の表記で CMSF draft-01 の行があること。
- `skills/shiguredo-moqt/SKILL.md`:
  - description、特徴の対応仕様、モジュール構成の表の `msf` 行、準拠仕様の節に、CMSF draft-01 があること。
  - 準拠仕様の節の MSF の行が、draft-ietf-moq-msf-02 とその URL になっていること。
  - 0467〜0470 で `src/` に追加・削除・変更された pub 項目 (シグネチャとフィールドの型の変更を含む) が、すべて記載されていること。`MsfCloneTrack` のフィールド、0469 の相互変換、`MsfTrack::stream_mapping` を含む。確かめ方は、`src/` の差分の pub 項目と、SKILL.md の記載を突き合わせることである。
  - CLI の記載を加えていないこと。
- `crates/README.md`、crate doc、モジュール doc の準拠の文に、`draft-ietf-moq-cmsf-01` があること。CMSF の型を `msf` モジュールの外に置いた場合は、SKILL.md のモジュール構成の表にそのモジュールの行があり、`src/lib.rs` のそのモジュールの doc に `draft-ietf-moq-cmsf-01` があること。
- `docs/missing-features.md`:
  - 対象仕様に CMSF があること。
  - 「`initDataList` の `inline` / `track-property` 型、`initRef` の読み取り」の行が、`track-property` 型の entry と loc の track の initRef の読み取りに絞られ、仕様の列が MSF §5.1.7 / §5.2.14 / §12.2 であり、moq-pub が loc の track に `object-property` の initRef を載せることと、cmaf の track で inline entry を使うことの両方と食い違っていないこと。
  - 5.12 / 5.14 / 6.5 / 6.13 が、0471 / 0472 の完了後の挙動と食い違っていないこと。
  - 第 3 節に、完了後も `crates/` から呼ばれない CMSF の公開 API の行があること。
  - 本 issue で書き直した箇所の中の節番号が、すべて MSF-02 / CMSF-01 の番号であること。
- `docs/msf.md` の冒頭の対応仕様の文に、`draft-ietf-moq-cmsf-01` があること。
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
