# CMSF の SAP Type timeline の型付き API を追加する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

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
| Event Timeline の index key | §3.6.1: SAP Type timeline の index は `'l'` が MUST | §8.2: `T` / `L` / `M` (大文字) | `L` を使う。小文字の `l` は受理しない。MSF-02 の大文字化に合わせた現行実装の方針を維持する |
| eventType の登録 | §8: IANA アクションなし。`org.ietf.moq.cmsf.sap` を使う | §14.2: MSF Event Timeline Types registry を新設 (初期値なし) | eventType は文字列として扱っており、registry の有無で挙動は変わらない |

## 設計方針

- 定数 `CMSF_SAP_TYPE_EVENT_TYPE` (`"org.ietf.moq.cmsf.sap"`) と、record 型 `CmsfSapTypeRecord { group_id, object_id, sap_type, ept_ms }` を追加する。`MsfEventTimeline` との相互変換を提供する。
  - CMSF が定める型と定数には `Cmsf` / `CMSF_` の接頭辞を付ける。0470 の Content Protection の型も同じ規則にする。
- index は `L` だけを許す。`data` は 2 要素の整数配列とする。
  - 1 要素目の SAP type は 0〜3 とする (CMSF §3.6.1)。Object ID 0 (MSF-02 §6.2 により Group の先頭 Object) の record では 1 か 2 を要求する。
  - 2 要素目の EPT は ms の整数 (`i64`) とする。`MsfEventIndex::MediaPtsMs(i64)` と同じ型である。
  - 違反は `InvalidCatalog` にする。
- SAP Type timeline がどの media track を記述するかを、CMSF は定めていない。ライブラリでは対応付けを要求しない。publisher が `depends` に対象 track 名を載せる運用を、`docs/msf.md` の CMSF の節に書く。
- moq-pub / moq-sub による SAP Type timeline track の発行と購読は範囲外とする。

## 完了条件

- CMSF-01 §3.6.2 の例の index を `L` に置き換えたものが、`CmsfSapTypeRecord` と相互変換できること。
- 次の payload が `InvalidCatalog` で拒否されること。
  - 小文字 `l` の index
  - `T` / `M` の index
  - 2 要素でない data
  - 範囲外の SAP type
  - Object ID 0 で SAP type が 1 / 2 以外
- 上記を `tests/test_msf/event_timeline.rs` で固定すること。
- `docs/msf.md` の CMSF の節に、SAP Type timeline、`depends` の運用、上記「ずれ」の判断 (index key を `L` にすること) を追記すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
