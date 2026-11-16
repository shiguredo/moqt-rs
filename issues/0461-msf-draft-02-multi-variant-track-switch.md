# MSF multi-variant fast track switching を購読側に実装する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §6.3 (Multi-Variant Fast Track Switching) は、keyframe 間隔の異なる alternate group 内の track を切り替える手順を定めた。

- 参加時は、帯域制約を満たす最も近い switch point を持つ track へ最初に購読する (SHOULD)。
- 再生開始後、より大きい `keyframeSpacing` の track へ valid switch point で切り替えてよい (MAY)。
- source の `keyframeSpacing` を S、target を T とし、alternate group の開始 Group ID を G0 とすると、source の Group G / Object N からの切り替えは `(G - G0) * S + N` が `T` で割り切れるとき valid である。target の Group ID は `G0 + ((G - G0) * S + N) / T` で、Object ID 0 から受信する。

§4.3 は alternate group 内の全 track の開始 Group ID が同一であることを MUST とする。開始 ID は 0 に限らない。現行 publisher は壁時計ベースの開始 ID を使う。

現行実装は alternate group を認識せず、カタログから最初に一致した映像 track を選ぶだけである。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `receive_catalog` は codec が一致する最初の映像 track を選び、`altGroup` を見ない。
- `keyframeSpacing` を表す型・フィールドがない。switch point を計算する関数もない。
- alternate group 内の track を切り替える購読状態機械、切り替え時の decoder 再初期化、旧 track の購読解除、帯域情報の受け渡し経路がない。
- `VideoDecodeOrder::reset` は「映像の track を切り替えたとき」に呼ぶと doc にあるが、実際に呼ぶ経路はない。
- `GroupSwitchGate` は同一 track 内の Group 境界を扱う。
- 0460 は `MsfTrack.keyframe_spacing` を `Option<u64>` で追加し、`keyframeSpacing` を持たないカタログを受理する。None のときの切り替え可否が未定義である。

## 設計方針

- 0460 の後に実施する。0460 が追加する `keyframe_spacing` と alternate group の Group 割り当て API を使う。
- switch point の判定を `((G - G0) * S + N) % T == 0` として実装し、target の Group ID を `G0 + ((G - G0) * S + N) / T` で求める純粋関数を追加する。G0 は alternate group の開始 Group ID とし、source と target で同一であることを前提にする。異なる場合は切り替え不可とする。
- S または T が 0 、または `keyframeSpacing` が None の track は切り替え候補から除外する。None は「keyframe 間隔が不明」として扱い、候補にしない。
- 参加時の初期購読先は、利用可能帯域を引数で受け取り、`bitrate` (§5.2.23) が帯域以下の track のうち `keyframeSpacing` が最小のものを選ぶ。該当がなければ `bitrate` が最小の track を選ぶ。帯域が不明な場合は `keyframeSpacing` が最小の track を選ぶ。
- カタログから alternate group の track 群を取り出し、上記の規則で初期購読先を選ぶ。帯域は既存の購読パラメータから取得できないため、関数の引数として受け取る。
- 切り替え時は target track の開始 Group ID から Object ID 0 で受信し、`VideoDecodeOrder::reset` を呼んで decoder 状態を作り直す。旧 track の購読解除と新 track の購読開始の順序を定義する。
- 複数の alternate group がある場合は、購読対象の映像 track が属する group だけを対象にする。

## 完了条件

- `(G - G0) * S + N` が `T` で割り切れる場合だけ切り替え可能と判定し、target の Group ID `G0 + ((G - G0) * S + N) / T` を返す関数が、G0 が 0 でないケースを含めてテストで固定されていること。
- G0 が source と target で異なる場合に切り替え不可となること。
- S = 0、T = 0、`keyframeSpacing` が None の track が切り替え候補から除外されること。
- 帯域を指定した初期購読先の選択が、`bitrate` と `keyframeSpacing` の規則どおりになること。帯域不明時の選択もテストされていること。
- alternate group 内の track を、valid switch point で切り替えられること。切り替え後に decoder 状態が作り直されること。
- 切り替え後も Object ID 0 から受信し、旧 track の購読が解除されること。
- 0460 の後に実施することを issue に明記すること。
- 上記を `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
