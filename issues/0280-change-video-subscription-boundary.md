# 映像も購読境界で表示を選別する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/change-video-subscription-boundary
- Polished: {YYYY-MM-DD}

## 目的

relay は Location Filter の無い購読に対し、cache が持つ**最新 Group の先頭から** replay する。この replay は映像のキーフレーム (Group の先頭) を含むため起動には有用だが、`SUBSCRIBE_OK` の Largest Object 以前のフレームも含む。音声は購読境界の規則 (`AudioDeliveryOrder`) で境界以下を落としているが、映像には相当する規則が無く、購読前のフレームが表示と録画に回っている。moqt-js の `CatchUpGate` と同じく、境界以前は「復号はするが表示しない」に揃える。

## 現状

- `examples/moq-sub/src/pipeline.rs` の映像経路は `VideoDecodeOrder` で復号順だけを判定し、購読境界の判定を持たない。`AudioDelivery` 側は `AudioDeliveryOrder` が `SUBSCRIBE_OK` の Largest Object を境界に持つ。
- 実測 (relay、publisher を 5 秒先行、10 秒のトレース): `SUBSCRIBE_OK` の境界が `(G, 26)` のとき、境界以下 19〜23 件の Object を decode し、そのまま `frame_tx` へ送出していた (境界より後は 142 件)。
- 実測 (録画): MP4 の先頭サンプル (レコーダの `base_pts_us`) は購読の 864〜902 ms 前の TIMESTAMP であった。FETCH の有無と `NEW_GROUP_REQUEST` の有無によらず同じ値であり、原因は relay の replay である。cache 無効の relay では購読後の値になった。
- 録画経路 (`record_video_object` → `mp4.rs`) は「キーフレーム + VIDEO_CONFIG が揃う最初のサンプル」まで待って基準時刻を決めるため、replay が運ぶキーフレームは録画開始の条件そのものである。
- 表示側は `shiguredo_moqt::playout::buffer::PlayoutBuffer` に到着順で積み、捨てるのは上限超過のときだけである。最初のフレームが時間軸の基準になるため、購読前のフレームが表示の先頭になる。

## 設計方針

- 映像にも購読境界 (`SUBSCRIBE_OK` の Largest Object) を保持し、境界以下の Location の Object は `frame_tx` へ送出しない。復号は行う (境界以降のフレームの参照になるため。moqt-js の CatchUpGate と同じ扱い)。
- 境界が無い場合と位置が分からない Object は live として送出する (音声の規則と同じ)。
- 判定は Location (Group ID, Object ID) の比較で行い、TIMESTAMP の比較で代用しない。
- 録画は本 issue では変更しない。replay のキーフレームを録画の基準に使う現在の挙動を維持する (境界以下を録画から外すと、次のキーフレームまで録画が始まらない)。録画の先頭に購読前のフレームを残すかは別に判断する。
- 境界は購読のたびに更新する (GOAWAY 移行の再購読では、移行先の live が流れる前に更新する)。
- 境界以下の Object の復号の有無は、モックを使わないテストでは観測できない。完了条件は観測できる効果 (送出の有無、録画の基準) で書く。

## 完了条件

- 境界以下の Location の映像 Object が `frame_tx` へ送出されないこと (ユニットテストで固定する)。
- 境界より後の live の映像が従来どおり送出されること。
- 境界が無い場合と位置が分からない Object は送出されること。
- 録画の基準が replay のキーフレームのままであること (MP4 の先頭サンプルが購読前の TIMESTAMP を持つことをテストで固定する)。
- 実 relay の E2E (`publish_and_subscribe_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
