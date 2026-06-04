# LOC の Video Payload Format に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-loc-video-payload-format
- Polished: {YYYY-MM-DD}

## pending にした理由

LOC-04 §2.1 (Video Payload Format) の NAL 境界処理を本ライブラリに持たせるか、application / codec 層に委ねるかの設計判断が未確定なため保留する。

現状の `src/loc.rs` は LOC Properties (§2.3) の codec のみを提供し、payload は application 層の責務と明記している。example は AVCC 4 バイト長プレフィックス + Video Config (`avcC` / `hvcC`) の経路のみで、LOC-04 が許容する以下の表現には未対応である。

- §2.1.3 (Length Prefixes in Payload): 「長さ 1 は start code と解釈する」、「4 バイト未満の長さプレフィックスは Video Config で指定する」
- §2.1.4 (Start Code Prefixes in Payload): 3 バイト start code (「track が length prefix も Video Config も使わない場合のみ」)
- §2.1.1 / §2.1.2 の parameter set を payload 内に置くか header (Video Config) に置くかの切り替え

NAL 境界処理はコーデック別の parameter set 解析を伴い、MSF / LOC のコンテナ codec 層とは責務が異なる。ライブラリのスコープを確定した後に、本 issue を `issues/` へ戻して対応する。

## 目的

LOC-04 §2.1 (Video Payload Format) の annexB / length prefix / parameter set を扱えるようにし、H.264 / H.265 の両表現で LOC payload を送受信できるようにする。

## 現状

- `src/loc.rs` は `LocProperties` / `LocPropertyValue` によるプロパティ codec のみで、payload の NAL 境界処理は無い
- example の `examples/moqt-publisher/src/encoder/h264.rs` / `h265.rs` は AVCC 4 バイト長プレフィックス前提で、parameter set を Video Config に格納する
- §2.1.3 の「長さ 1 = start code」と「4 バイト未満の長さプレフィックス」、§2.1.4 の 3 バイト start code は未対応
- Video Config の長さプレフィックスサイズ指定 (§2.1.3) を解釈する API は無い

## 設計方針

- length prefix と start code の双方を解釈する型またはヘルパーを追加する
- Video Config から長さプレフィックスサイズを取得する規則 (§2.1.3) を実装するか、application 層に委ねるかを確定する
- H.264 / H.265 の parameter set (SPS / PPS / VPS) を payload 内 / header のどちらに置くかを切り替えられるようにする
- コーデック非依存のコンテナ層に留めるか、コーデック別ヘルパーを許容するかを確定する

## 完了条件

- annexB / length prefix の双方を解釈できること
- Video Config で長さプレフィックスサイズを指定できること
- LOC-04 §2.1.1-§2.1.4 の各表現を roundtrip できること
- `tests/` / `pbt/` に対応テストが追加されていること
