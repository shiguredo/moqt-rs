# MSF の PBT トラック生成器に codec と role を追加する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/test-msf-codec-pbt
- Polished: 2026-09-28
- Updated: 2026-10-11

## 目的

MSF のカタログ検証は codec 文字列と role から audio / video を判定して §5.2.18 / §5.2.22 / §5.2.28 / §5.2.29 の MUST を課す。しかし `pbt/tests/prop_msf.rs` のトラック生成器は `role` と `codec` を常に `None` で生成するため、codec / role 起点の検証が PBT で一度も実行されていない。生成器を拡張し、codec / role を持つトラックを含むカタログでも encode → decode の往復が成立することを PBT で押さえる。

## 現状

`pbt/tests/prop_msf.rs` の `sample_track` は `role` と `codec` を一度も設定せず、`MsfTrack::new` のデフォルトである `None` のまま生成する。`full_catalog_roundtrip` は最大 5 トラック (`tracks`) と最大 2 トラック (`publishTracks`) を生成し `(namespace, name)` だけで重複排除する。`sample_track` は `delta_roundtrip` の add 更新でも使われる。

そのため次の経路は PBT の対象外になっている。

- codec から audio / video を判定したときの bitrate / samplerate / channelConfig の要求
- role と codec が食い違うときの要求の重ね合わせ
- `signlanguage` / `audiodescription` の role 依存の要求
- codec を持つトラックを含むカタログの encode → decode 往復

## 設計方針

`sample_track` に codec と role の生成を追加し、生成した codec に対応する必須フィールドも同時に生成して「受理されるカタログ」の往復を検証する。

- codec はレジストリの登録名 (完全一致形 / `名前.` 付き形 / `名前-` 付き形 (`pcm-`) / 未登録形) と `None` を混ぜて生成する
- codec が audio と判定される場合は samplerate / channelConfig / bitrate を、video と判定される場合は bitrate を必ず生成する
- role は draft-ietf-moq-msf-01 §5.2.6 (Track role) の予約 role のうち audio / video の判定に関わるもの (`video` / `signlanguage` / `audio` / `audiodescription`。Table 4 の予約 role はこの 4 つ以外にもある) と `None` を混ぜる
- role を設定したトラックには codec も必ず生成する (`src/msf.rs` の `validate_media_track_fields` は role 由来の要求でも codec の存在を必須とするため。role と codec は独立に混ぜず、生成時に組み合わせを決めてから必須フィールドを決める)
- role と codec が食い違う組み合わせも生成し、両方の要求 (video なら codec / bitrate、audio なら codec / bitrate / samplerate / channelConfig) を満たす値を生成する
- 生成したトラックが検証を通ることを前提に、encode → decode の往復でカタログが等価であることを検証する
- MSF の group 検証 PBT ([issues/0145](../issues/0145-test-msf-group-validation-pbt.md)) とは生成器を共有するが、この issue では生成器の拡張と往復検証だけを対象にする (group 一貫性の生成は 0145 が担当する)

## 完了条件

- `sample_track` が codec と role を生成すること
- codec / role に応じた必須フィールドが同時に生成され、生成カタログが decode と encode に成功すること
- `full_catalog_roundtrip` が codec / role を持つトラックを含むカタログで往復を検証すること
- 生成器が codec を持たないトラックも引き続き生成すること
- `make pbt` (`cargo test -p pbt`) と `make test` / `make clippy` / `make fmt` が通ること
