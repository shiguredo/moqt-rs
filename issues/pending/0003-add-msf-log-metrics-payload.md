# MSF Log / Metrics track の payload に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-log-metrics-payload
- Polished: {YYYY-MM-DD}

## pending にした理由

Log / Metrics track の payload 形式は MSF-01 の本文には定義されておらず、別ドラフトの [MOQLOG] / [MOQMETRICS] に委譲されている。MSF-01 §9.1 は「Implementations MUST follow the object payload format specified in Section 4 of [MOQLOG]」、§10.1 は「Implementations MUST follow the object payload format specified in Section 3 of [MOQMETRICS]」と規定する。

両ドラフトは `refs/moq/` に存在するため実装自体は可能だが、以下が未確定であり着手時に設計判断が必要なため保留する。

- payload codec を本ライブラリ (`shiguredo_moqt`) に同梱するか、MOQLOG / MOQMETRICS 用の別クレートに分けるか
- §9.2 / §9.3 / §10.2 / §10.3 の Track Namespace / Track Name / Group ID / Object ID の割り当て規則は両ドラフトの規約に依存し、MSF-01 単体では確定できない
- [MOQLOG] / [MOQMETRICS] は個別 draft であり、将来の改訂で payload 形式が変わる可能性がある

MSF-01 は「MSF の catalog / timeline / 伝送規則」を対象とし、Log / Metrics の payload は別ドラフトの責務と切り分けるのが妥当という判断もある。方針が確定した時点で本 issue を `issues/` へ戻して対応する。

## 目的

MSF-01 §9 (Log track) / §10 (Metrics track) が参照する payload 形式に対応し、catalog の `publishTracks` 経由で subscriber から publisher 方向のログ・メトリクスを送受信できるようにする。

## 現状

- `src/msf.rs` の `MsfPackaging` は `MoqLog` / `MoqMetrics` の値を持つが、payload の encode / decode は持たない
- `src/msf.rs` の `MsfTrack` は `publishTracks` のトラック定義 (`role` / `connectionUri` / `token` 等) を扱うが、payload には触れない
- [MOQLOG] / [MOQMETRICS] は `refs/moq/draft-jennings-moq-log-03.txt` / `refs/moq/draft-jennings-moq-metrics-02.txt` に存在する
- MSF-01 §9.4 / §10.4 の catalog 要件 (`packaging` = `moqlog` / `moqmetrics`、`role` = `log` / `metrics`) は `MsfPackaging` の値として表現できるが、payload 形式の実装はない

## 設計方針

- [MOQLOG] §4 の log entry (severity / timestamp / hostname / application / process ID / message ID / message、TraceID / SpanID / InstrumentationScope) を encode / decode する型を追加する
- [MOQMETRICS] §3 の Resources / Attributes / Metrics (Gauge / Counter、f64 / integer) を encode / decode する型を追加する
- §9.2 / §9.3 / §10.2 / §10.3 の Group ID / Object ID 割り当て規則を型またはヘルパーで表現するか、application 層の責務とするかを確定する
- 配置先 (本ライブラリ同梱 / 別クレート) を確定する

## 完了条件

- [MOQLOG] §4 の log entry を encode / decode できること
- [MOQMETRICS] §3 の metrics payload を encode / decode できること
- MSF-01 §9.4 / §10.4 の catalog 要件と整合すること
- `tests/` / `pbt/` に対応テストが追加されていること
