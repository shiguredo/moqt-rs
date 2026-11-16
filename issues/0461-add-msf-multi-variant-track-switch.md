# MSF multi-variant fast track switching を購読側に実装する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-multi-variant-track-switch
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §6.3 (Multi-Variant Fast Track Switching) は、keyframe 間隔の異なる alternate group 内の track を切り替える手順を定めた。

- 参加時は、帯域制約を満たす最も近い switch point を持つ track へ最初に購読する (SHOULD)。
- 再生開始後、より大きい `keyframeSpacing` の track へ valid switch point で切り替えてよい (MAY)。
- source の `keyframeSpacing` を S、target を T とすると、source の Group G / Object N からの切り替えは `(G * S + N) / T` が整数になるとき valid である。その値が target の開始 Group ID で、Object ID 0 から受信する。

現行実装は alternate group を認識せず、カタログから最初に一致した映像 track を選ぶだけである。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `receive_catalog` は codec が一致する最初の映像 track を選び、`altGroup` を見ない。
- `keyframeSpacing` を表す型・フィールドがない。switch point を計算する関数もない。
- alternate group 内の track を切り替える購読状態機械、切り替え時の decoder 再初期化、旧 track の購読解除の経路がない。
- `VideoDecodeOrder::reset` は「映像の track を切り替えたとき」に呼ぶと doc にあるが、実際に呼ぶ経路はない。
- `GroupSwitchGate` は同一 track 内の Group 境界を扱う。

## 設計方針

- switch point の判定を `(G * S + N) % T == 0` として実装し、target の開始 Group ID を `(G * S + N) / T` で求める純粋関数を追加する。S と T が 0 の場合の扱いを明示し、0 除算を避ける。
- カタログから alternate group の track 群を取り出し、帯域制約と switch point で初期購読先を選ぶ。帯域情報は既存の購読パラメータから取得できない場合は、選択に必要な情報を引数で受け取る形にする。
- 切り替え時は target track の開始 Group ID から Object ID 0 で受信し、`VideoDecodeOrder::reset` を呼んで decoder 状態を作り直す。旧 track の購読解除と新 track の購読開始の順序を定義する。
- `keyframeSpacing` の型と catalog への追加は publisher packaging の issue に依存するため、その後に実施する。

## 完了条件

- `(G * S + N) / T` が整数になる場合だけ切り替え可能と判定し、target の開始 Group ID を返す関数がテストで固定されていること。
- S = 0 または T = 0 の入力で panic せず、仕様に沿ったエラーまたは非 valid を返すこと。
- alternate group 内の track を、帯域制約を満たす switch point で切り替えられること。切り替え後に decoder 状態が作り直されること。
- 切り替え後も Object ID 0 から受信し、旧 track の購読が解除されること。
- 上記を `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
