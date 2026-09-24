# MSF の PBT トラック生成器に codec と role を追加する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/test-msf-codec-pbt
- Polished: {YYYY-MM-DD}

## 目的

MSF のカタログ検証は codec 文字列と role から audio / video を判定して §5.2.18 / §5.2.22 / §5.2.28 / §5.2.29 の MUST を課す。しかし `pbt/tests/prop_msf.rs` のトラック生成器は `role` と `codec` を常に `None` で生成するため、codec / role 起点の検証が PBT で一度も実行されていない。生成器を拡張し、codec / role を持つトラックでも decode → encode の往復が成立することを PBT で押さえる。

## 現状

`pbt/tests/prop_msf.rs` の `sample_track` は `role: None` と `codec: None` を固定値で設定している。`full_catalog_roundtrip` は最大 5 トラックを生成し `(namespace, name)` だけで重複排除する。

そのため次の経路は PBT の対象外になっている。

- codec から audio / video を判定したときの bitrate / samplerate / channelConfig の要求
- role と codec が食い違うときの要求の重ね合わせ
- `signlanguage` / `audiodescription` の role 依存の要求
- codec を持つトラックを含むカタログの decode → encode 往復

## 設計方針

`sample_track` に codec と role の生成を追加し、生成した codec に対応する必須フィールドも同時に生成して「受理されるカタログ」の往復を検証する。

- codec はレジストリの登録名 (完全一致形 / `名前.` 付き形 / 未登録形) と `None` を混ぜて生成する
- codec が audio と判定される場合は samplerate / channelConfig / bitrate を、video と判定される場合は bitrate を必ず生成する
- role は予約 role (`video` / `signlanguage` / `audio` / `audiodescription`) と `None` を混ぜる
- role と codec が食い違う組み合わせも生成し、両方の要求を満たす値を生成する
- 生成したトラックが検証を通ることを前提に、decode → encode 往復でカタログが等価であることを検証する
- MSF の group 検証 PBT とは生成器を共有するが、この issue では生成器の拡張と往復検証だけを対象にする

## 完了条件

- `sample_track` が codec と role を生成すること
- codec / role に応じた必須フィールドが同時に生成され、生成カタログが decode と encode に成功すること
- `full_catalog_roundtrip` が codec / role を持つトラックを含むカタログで往復を検証すること
- 生成器が codec を持たないトラックも引き続き生成すること
- `make pbt` (`cargo test -p pbt`) と `make test` / `make clippy` / `make fmt` が通ること
