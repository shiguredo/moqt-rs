# CMSF の SAP Type timeline の型付き API を追加する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23

## 目的

CMSF-01 §3.6 は、Event Timeline の特別な形として SAP Type timeline を定めている。これは、各 Object が始まる SAP type と、その Object の最も早い表示時刻 (EPT) を通知する。

現行実装は Event Timeline を汎用の JSON として扱うだけで、SAP Type timeline の規則 (index、data の形、値域) を検証できない。本 issue では、SAP Type timeline の型付き API をライブラリに追加する。

CMSF 対応全体の方針は 0467 を参照すること。コードの前提はないが、`docs/msf.md` の CMSF の節は 0467 で新設するので、0467 の後に着手する。

## 現状

- `src/msf.rs` の `decode_event_entry` は、大文字の `T` / `L` / `M` だけを index として読み、`data` を任意の JSON 値として保持する。
- `org.ietf.moq.cmsf.sap` 用の型や定数はない。
- `MsfEventIndex::Location(u64, u64)` が `L` の index を表す。

## CMSF-01 と MSF-02 のずれ

| 項目 | CMSF-01 | MSF-02 | 本実装の扱い |
| --- | --- | --- | --- |
| Event Timeline の index key | §3.6.1: SAP Type timeline の index は `'l'` が MUST | §8.2: `T` / `L` / `M` (大文字) | `L` を使う。小文字の `l` は受理しない。この拒否は既存の `MsfEventTimeline::decode` が行っており、MSF-02 の大文字化に合わせた現行実装の方針を維持する |
| eventType の登録 | §3.6.1: eventType は `org.ietf.moq.cmsf.sap` (MUST)。§8: IANA アクションなし | §14.2: MSF Event Timeline Types registry を新設 (初期値なし) | eventType は文字列として扱っており、registry の有無で挙動は変わらない |

## 設計方針

- 定数 `CMSF_SAP_TYPE_EVENT_TYPE` (`"org.ietf.moq.cmsf.sap"`) と、record 型 `CmsfSapTypeRecord { group_id: u64, object_id: u64, sap_type: u8, ept_ms: i64 }` を追加する。
  - CMSF が定める型と定数には `Cmsf` / `CMSF_` の接頭辞を付ける。0470 の Content Protection の型も同じ規則にする。
- 変換は timeline 単位で、両方向とも失敗し得るものとする。
  - `MsfEventTimeline` → `Vec<CmsfSapTypeRecord>`: decode 済みの `MsfEventTimeline` を入力にとる。バイト列からの decode は既存の関数に任せる。圧縮がなければ `MsfEventTimeline::decode` を使う。MSF_COMPRESSION に従う gzip の展開を含む場合は、自由関数の `decode_event_timeline` を使う。
  - `&[CmsfSapTypeRecord]` → `MsfEventTimeline`: 手で組んだ record も、下の規則で検証してから変換する。既存の encode と同じく、自分で decode できない SAP Type timeline を出力しないためである。
  - どちらの向きも、違反は `InvalidCatalog` にする。timeline 系の不正を `InvalidCatalog` で返すのは、既存の `MsfEventTimeline` / `MsfMediaTimeline` と同じ慣例である。
- record の規則:
  - index は `L` (`MsfEventIndex::Location`) だけを許す。`T` / `M` の entry は拒否する。
  - `data` は 2 要素の整数配列とする。整数の読み方は、既存の `M` index と同じ `parse_integer_value` に揃え、整数値の JSON number (例: `2.0`) を受理する。
  - 1 要素目の SAP type は 0〜3 とする (CMSF §3.6.1)。Object ID 0 (MSF-02 §6.2 により Group の先頭 Object) の record では 1 か 2 を要求する。
  - 2 要素目の EPT は ms の整数 (`i64`) とする。`MsfEventIndex::MediaPtsMs(i64)` と同じ型である。
- SAP Type timeline がどの media track を記述するかを、CMSF は定めていない。ライブラリでは対応付けを要求しない。publisher が `depends` に対象 track 名を載せる運用を、`docs/msf.md` の CMSF の節に書く。
- moq-pub / moq-sub による SAP Type timeline track の発行と購読は範囲外とする。

## 完了条件

- CMSF-01 §3.6.2 の例の index を `L` に置き換えたもの (4 record) が、`Vec<CmsfSapTypeRecord>` と相互変換できること。
- `MsfEventTimeline` → record の変換で、次の entry が `InvalidCatalog` で拒否されること。
  - `T` / `M` の index
  - 2 要素でない data
  - 数値でない要素 (文字列 `"2"`、`null`、bool) と、整数値でない数値 (例: `2.5`)
  - 範囲外の SAP type
  - Object ID 0 で SAP type が 1 / 2 以外
- `MsfEventTimeline` → record の変換で、整数値の JSON number の要素 (例: `[2.0, 1000]`) が整数として受理されること。既存の `media_pts_index_accepts_integer_valued_json_numbers` と同じ位置付けである。
- record → `MsfEventTimeline` の変換で、範囲外の SAP type と、Object ID 0 で SAP type が 1 / 2 以外の record が `InvalidCatalog` で拒否されること。
- 上記の固定例と拒否ケースを `tests/test_msf/event_timeline.rs` で固定すること。小文字 `l` の拒否は既存の `lowercase_index_keys_rejected` で固定済みなので、追加しない。
- 規則を満たす任意の record 列で、record → `MsfEventTimeline` → バイト列 → `MsfEventTimeline` → record の往復が一致することを、`pbt/tests/prop_msf.rs` で固定すること。
- `docs/msf.md` の CMSF の節に、SAP Type timeline、`depends` の運用、上記「ずれ」の判断 (index key を `L` にすること) を追記すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
