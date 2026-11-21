# MSF Log / Metrics track の payload に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-log-metrics-payload
- Polished: {YYYY-MM-DD}
- Refreshed: 2026-11-22

## pending にした理由

Log / Metrics track の payload 形式は MSF-02 の本文には定義されておらず、別ドラフトの [MOQLOG] / [MOQMETRICS] に委譲されている。
MSF-02 §9.1 は「Implementations MUST follow the object payload format specified in [MOQLOG], Section 4」、§10.1 は「Implementations MUST follow the object payload format specified in [MOQMETRICS], Section 3」と規定する。
ただし MSF-02 の参照節は現行ドラフトと一致せず、[MOQLOG] の Log Record は §5.3、[MOQMETRICS] の catalog / data payload は §5.3 / §5.4 にある。

両ドラフトは `refs/moq/` に存在するため実装自体は可能だが、以下が未確定であり着手時に設計判断が必要なため保留する。

- payload codec を本ライブラリ (`shiguredo_moqt`) に同梱するか、MOQLOG / MOQMETRICS 用の別クレートに分けるか
- MSF-02 §9.3 / §10.3 と [MOQLOG] §5.2 / [MOQMETRICS] §5.4 で Group ID / Object ID の規則が食い違うため、どちらを正とするか
- [MOQLOG] / [MOQMETRICS] は個別 draft であり、将来の改訂で payload 形式が変わる可能性がある

仕様間の不一致は次の 2 点である。

- MSF-02 §9.1 は Log payload を JSON の log entry とし、§9.3 は Group ID をマイクロ秒とするが、[MOQLOG] §5.2 / §5.3 は Group ID を秒とし payload をバイナリの Log Record とする
- MSF-02 §10.3 は Object 0 を capture timestamp と attributes、Object 1 以上を name-value の metric とするが、[MOQMETRICS] §5.4 は Object 0 を Capture Info (Capture Timestamp Offset / Catalog Version)、Object N を Metric Value とし、attributes は catalog track 側に置く

実装では「MSF catalog は MSF-02、log / metrics の payload と Group / Object 規則は [MOQLOG] / [MOQMETRICS]」を第一候補とするが、最終的な優先順位は仕様作者への確認が必要である。MSF-02 は「MSF の catalog / timeline / 伝送規則」を対象とし、Log / Metrics の payload は別ドラフトの責務と切り分けるのが妥当という判断もある。方針が確定した時点で本 issue を `issues/` へ戻して対応する。

## 目的

MSF-02 §9 (Log track) / §10 (Metrics track) が参照する payload 形式に対応し、catalog の `publishTracks` 経由で subscriber から publisher 方向のログ・メトリクスを送受信できるようにする。

## 現状

- `src/msf.rs` の `MsfPackaging` は `MoqLog` / `MoqMetrics` の値を持つが、payload の encode / decode は持たない
- `src/msf.rs` の `MsfTrack` は `publishTracks` のトラック定義 (`role` / `connectionURI` / `authInfo` 等) を扱い、`MsfCatalog` は `publish_tracks` を含めて encode / decode できるが、payload には触れない。track-level の `token` は MSF-02 で削除済み
- [MOQLOG] / [MOQMETRICS] は `refs/moq/draft-jennings-moq-log-04.txt` / `refs/moq/draft-jennings-moq-metrics-03.txt` に存在する
- MSF-02 §9.4 / §10.4 の catalog 要件 (`packaging` = `moqlog` / `moqmetrics`、`role` = `log` / `metrics`) は `MsfPackaging` と `MsfTrack::role` で表現できるが、publishTracks への配置と role の MUST ペアを検証する処理はない

## 設計方針

- [MOQLOG] §5.3 の Log Record (Timestamp Offset / Facility / Hostname / App Name / Proc ID / Msg ID / Message / Attribute) を encode / decode する型を追加する。TraceID / SpanID / InstrumentationScope は Attribute の key として扱う。
  severity は Track Name で表現され Log Record には含まれない。Timestamp Offset は Group ID (秒) へのナノ秒オフセットとして扱う
- [MOQMETRICS] §4 の Resource / Attribute / Metric (Gauge / Counter、64-bit integer / float) と、§5.3 の catalog track (Catalog Header / Metric Description)、§5.4 の data object (Capture Info / Metric Value) を encode / decode する型を追加する。Metric Index は catalog の Object ID と data の Object ID の対応で表現する
- MSF-02 §9.2 / §9.3 / §10.2 / §10.3 と [MOQLOG] §5.1 / §5.2、[MOQMETRICS] §5.1 / §5.4 の Track Namespace / Track Name / Group ID / Object ID の割り当て規則を型またはヘルパーで表現するか、application 層の責務とするかを確定する。両者が食い違う箇所は、どちらを正とするかを先に確定してから型に落とす
- 配置先 (本ライブラリ同梱 / 別クレート) を確定する

## 完了条件

- [MOQLOG] §5.3 の Log Record を encode / decode できること
- [MOQMETRICS] §4 のデータモデルに基づき、§5.3 の catalog track と §5.4 の data object を encode / decode できること
- MSF-02 §9.4 / §10.4 の catalog 要件と整合すること
- `tests/` / `pbt/` に対応テストが追加されていること
