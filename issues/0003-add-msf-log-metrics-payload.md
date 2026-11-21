# MSF Log / Metrics track の payload に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-log-metrics-payload
- Polished: 2026-11-22
- Refreshed: 2026-11-22

## reopened にした理由

仕様の優先順位を次のように決め、実装に着手できる状態になったため reopened にする。仕様間の不整合の内容もこの issue に含める。

- MSF catalog の構造・namespace の区切り規則・packaging / role は MSF-02 を一次資料とする
- log / metrics の Track Namespace の tuple と Track Name、payload、Group / Object 規則は MOQLOG-04 / MOQMETRICS-03 を一次資料とする
- 上流へのフィードバックは本 issue の範囲外とし、必要になった時点で別途判断する

## 優先度

現時点では優先度低。MOQLOG-04 / MOQMETRICS-03 の仕様が安定した後に再評価する。

## 目的

MSF-02 §9 (Log track) / §10 (Metrics track) が参照する payload 形式に対応し、log / metrics の payload を encode / decode できるようにする。まず payload codec と MOQMETRICS の catalog track を追加し、publishTracks 経路と example への統合は別 issue とする。

## 現状

- `src/msf.rs` の `MsfPackaging` は `MoqLog` / `MoqMetrics` の値を持つが、payload の encode / decode は持たない
- `src/msf.rs` の `MsfTrack` は `publishTracks` のトラック定義 (`role` / `connectionURI` / `authInfo` 等) を扱い、`MsfCatalog` は `publish_tracks` を含めて encode / decode できるが、payload には触れない。track-level の `token` は MSF-02 で削除済み
- [MOQLOG] / [MOQMETRICS] は `refs/moq/draft-jennings-moq-log-04.txt` / `refs/moq/draft-jennings-moq-metrics-03.txt` に存在する
- MSF-02 §9.4 / §10.4 の catalog 要件 (`packaging` = `moqlog` / `moqmetrics`、`role` = `log` / `metrics`) は `MsfPackaging` と `MsfTrack::role` で表現できるが、publishTracks への配置と role の MUST ペアを検証する処理はない

## 仕様間の不整合

MSF-02 と下位ドラフトには次の不整合がある。実装では上記の優先順位に従い、MSF-02 の該当記述（payload / Group / Object / namespace 例）は使わない。

- MSF-02 §9.1 はログ payload を JSON の log entry とし、[MOQLOG] Section 4 を参照している。MOQLOG-04 §5.3 はバイナリの Log Record で、§4 は Resource ID。
- MSF-02 §9.3 は Group ID をマイクロ秒、Object ID を同じマイクロ秒内で増加としている。MOQLOG-04 §5.2 は Group ID を秒、1 group = 1 秒、1 log = 1 Object。
- MSF-02 §10.1 は [MOQMETRICS] Section 3 を参照している。MOQMETRICS-03 §3 は Architecture Overview で、データモデルは §4、payload は §5.3 / §5.4。
- MSF-02 §10.3 は Object 0 をナノ秒の capture timestamp + attributes、Object 1 以上を name-value の metric としている。MOQMETRICS-03 §5.4 は Object 0 を Capture Info (Capture Timestamp Offset + Catalog Version)、Object N を Metric Value とする。
- MSF-02 §6.1 の Group numbering は、MOQMETRICS-03 §5.4 の疎な Group ID と両立する（どちらも PRIOR_GROUP_ID_GAP を用いる）。一方、MSF-02 §6.2 の Object ID を 1 ずつ増加させる規則は、MOQMETRICS-03 §5.4 の Prior Object ID Gap による欠番と両立しない。
- MSF-02 §5.6.17 の namespace 例 (`metrics.moq.arpa/metrics-v1/...`、`moq-syslog.arpa/logs-v1/...`) は、MOQLOG-04 §5.1 / MOQMETRICS-03 §5.1 の tuple と一致しない。
- MOQLOG-04 §6 / MOQMETRICS-03 §6 は namespace を MOQT §8.8 の形式で示している。MSF-02 §5.2.2 は MSF catalog の namespace を `/` 区切りで扱う。
- MOQMETRICS-03 §6 は `token` / `connectionUri` と書いている。MSF-02 は `authInfo` / `connectionURI` を使う。

## 設計方針

- [MOQLOG] §5.3 の Log Record (Timestamp Offset / Facility / Hostname / App Name / Proc ID / Msg ID / Message / Attribute) を encode / decode する型を追加する。TraceID / SpanID / InstrumentationScope は Attribute の key として扱う。
  severity は Track Name で表現され Log Record には含まれない。Timestamp Offset は Group ID (秒) へのナノ秒オフセットとして扱う
- [MOQMETRICS] §4 の Resource / Attribute / Metric (Gauge / Counter、64-bit integer / float。§5.3 の Value Representation で unsigned / signed / float を区別する) と、§5.3 の catalog track (Catalog Header / Metric Description)、§5.4 の data object (Capture Info / Metric Value) を encode / decode する型を追加する
- Metric Description / Metric Value の型は Object ID (Metric Index) を保持し、Metric Description 自体には Metric Index を encode しない。catalog の Object ID と data の Object ID の対応で Metric Index を表現する
- Track Namespace は [MOQLOG] §5.1 / [MOQMETRICS] §5.1 の tuple を使い、Track Name は [MOQLOG] §5.1 / [MOQMETRICS] §5.2 の severity / granularity の数字を使う。MSF catalog の namespace 文字列へは MSF-02 §5.2.2 の `/` 区切りで写像する。MOQLOG-04 §6 / MOQMETRICS-03 §6 の MOQT §8.8 形式は catalog の JSON には使わない
- 実装は `src/msf/log.rs` / `src/msf/metrics.rs` に置き、`src/msf.rs` で `pub mod log;` / `pub mod metrics;` を宣言して `msf::log` / `msf::metrics` として公開する（re-export はしない）
- Group ID / Object ID の割り当ては application 層の責務とし、payload codec は Timestamp Offset / Capture Timestamp Offset を Group ID に対するオフセットとして encode / decode する。割り当て規則は [MOQLOG] §5.2 と [MOQMETRICS] §5.4 を一次資料とし、MSF-02 §9.3 / §10.3 / §6.2 の食い違う記述は使わない
- decode は各仕様の MUST を検証し、定義済みフィールドの後ろのバイトは将来拡張のため無視する。少なくとも [MOQLOG] §5.3 の Timestamp Offset < 1,000,000,000 / Facility 0〜23 と、[MOQMETRICS] §5.3 / §5.4 の Metric Type / Value Representation / Capture Timestamp Offset < 1,000,000 を検査する
- publishTracks 経路と example への統合は別 issue とする。catalog の publishTracks への配置と role の MUST ペア検証もその別 issue で行い、本 issue では `MsfPackaging` / `MsfTrack::role` の表現を変更しない

## 完了条件

- [MOQLOG] §5.3 の Log Record を encode / decode できること
- [MOQMETRICS] §4 のデータモデルに基づき、§5.3 の catalog track と §5.4 の data object を encode / decode できること
- [MOQLOG] §5.3 / [MOQMETRICS] §5.3 / §5.4 の MUST に反する値が decode で拒否されること
- MSF-02 §9.4 / §10.4 の packaging / role の値と矛盾しないこと
- `tests/` / `pbt/` に対応テストが追加されていること
