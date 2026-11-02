# 映像も購読境界で表示を選別する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/change-video-subscription-boundary
- Polished: 2026-11-02

## 目的

実測に使った relay は、Location Filter の無い購読に対し、cache が持つ**最新 Group の先頭から** replay する。この replay は映像のキーフレーム (Group の先頭) を含むため起動には有用だが、`SUBSCRIBE_OK` の Largest Object 以前のフレームも含む。音声は購読境界の規則 (`AudioDeliveryOrder`) で境界以下を落としているが、映像には相当する規則が無く、購読前のフレームが表示と録画に回っている。moqt-js の `CatchUpGate` と同じく、境界以前は「復号はするが表示しない」に揃える。

## 現状

- `crates/moq-sub/src/pipeline.rs` の映像経路は `VideoDecodeOrder` で復号順だけを判定し、購読境界の判定を持たない。`AudioDelivery` 側は `AudioDeliveryOrder` が `SUBSCRIBE_OK` の Largest Object を境界に持つ。
- 実測 (relay、publisher を 5 秒先行、10 秒のトレース): `SUBSCRIBE_OK` の境界が `(G, 26)` のとき、境界以下 19〜23 件の Object を decode し、そのまま `frame_tx` へ送出していた (境界より後は 142 件)。
- 実測 (録画): MP4 の先頭サンプル (レコーダの `base_pts_us`) は購読の 864〜902 ms 前の TIMESTAMP であった。FETCH の有無と `NEW_GROUP_REQUEST` の有無によらず同じ値であり、原因は relay の replay である。cache 無効の relay では購読後の値になった。
- 録画経路 (`record_video_object` → `mp4.rs`) は「キーフレーム + VIDEO_CONFIG が揃う最初のサンプル」まで待って基準時刻を決めるため、replay が運ぶキーフレームは録画開始の条件そのものである。
- 表示側は `shiguredo_moqt::playout::buffer::PlayoutBuffer` に到着順で積み、上限超過のときは古い方から捨てる。表示時刻を過ぎたフレームも遅延上限を超えれば捨てる。時間軸の基準は直近 10 秒の「復号の出力を観測した壁時計 − TIMESTAMP」の最小値であり、購読直後は replay のフレームが最初の観測になるため、live が届くまでの基準は replay 側で決まり、購読前のフレームが表示の先頭に描かれる (最初の表示機会に複数溜まっていれば基準に最も近い末尾のフレーム)。
- 上記の実測値は外部の relay と moq-pub を使った手動計測の記録であり、リポジトリ内のテストでは再現しない。

## 設計方針

- 変更対象は `crates/moq-sub/src/pipeline.rs` の映像配送経路 (`deliver_video_object`) と、購読の確立・再購読で境界を設定する配線、`crates/moq-sub/README.md` とする。
- 映像にも購読境界 (起動時の購読では `SUBSCRIBE_OK` の Largest Object、draft-ietf-moq-transport-22 §3.1.4 / §9.20.17。GOAWAY による再購読では後述の経路ごとの最後に受信した Location) を保持し、境界以下の Location の Object は `frame_tx` へ送出しない。
- 復号は行う (境界以降のフレームの参照になるため。moqt-js の `CatchUpGate` と同じ扱い)。判定は復号の後、`frame_tx` への送出前に行い、`VideoDecodeOrder` が通した境界以下の Object は復号を飛ばさない。
- 境界が無い場合は live として送出する (音声の `AudioDeliveryOrder` の規則と同じ)。映像の配送経路 (live の Subgroup stream と FETCH 応答) は常に Location を持つため、位置が分からない場合の分岐は設けない。
- 判定は Location (Group ID, Object ID) の比較で行い、TIMESTAMP の比較で代用しない (`Location` の順序は (group_id, object_id) の辞書順、draft-ietf-moq-transport-22 §8.2)。live の Subgroup stream と FETCH 応答で同じ境界状態を共有し、どちらの経路でも同じ判定を通す。
- 録画は本 issue では変更しない。replay のキーフレームを録画の基準に使う現在の挙動を維持する (境界以下を録画から外すと、次のキーフレームまで録画が始まらない)。録画は境界の判定より前に行い、境界以下の Object も録画の対象にする (録画側の基準時刻による破棄は従来どおり)。録画の先頭に購読前のフレームを残すかどうかは、本 issue では現状維持 (残す) とし、別途判断する。
- 境界は購読のたびに更新する。起動時の購読では `SUBSCRIBE_OK` の Largest Object を境界にする。GOAWAY による再購読 (session 移行と request stream 単位の再発行) では、次の項の規則で決めた Location を境界にする。
- GOAWAY による再購読では、新しい `SUBSCRIBE_OK` の Largest Object を境界にしない (移行中・再発行中に publish された Object を境界以下として捨ててしまう)。境界は経路ごとに次の位置を使い、境界以下として送出しなかった Object も「最後に受信した Location」に含める。
  - session 移行 (`resubscribe_tracks`): 旧 session の stream task を止めて新しい data plane へ切り替える前 (移行先の live が流れる前) に `last_received_locations` を読み直した Location。再購読の開始時に Location Filter へ渡したスナップショットではない。音声の `AudioDeliveryOrder::reset_for_migration` と同じ位置で更新する。
  - request stream 単位の再発行 (`reissue_request`): 再発行の前に取得した最後に受信した Location。音声の `AudioDeliveryOrder::set_boundary` と同じ位置で更新する。
- 境界以下の Object の復号の有無は、モックを使わないテストでは直接は観測できない。完了条件は観測できる効果 (送出の有無、録画の基準) で書く。

## 完了条件

- 境界以下の Location の映像 Object が `frame_tx` へ送出されないこと (live の Subgroup stream と FETCH 応答で共有する配送経路を、実デコーダを使ったユニットテストで固定する)。
- 境界より後の live の映像が従来どおり送出されること。
- 境界が無い場合の映像が送出されること。
- 録画の基準が replay のキーフレームのままであること (MP4 の先頭サンプルが購読前の TIMESTAMP を持つことをテストで固定する)。
- GOAWAY による再購読で、session 移行は移行先の live が流れる前に旧 session で最後に受信した Location、request stream 単位の再発行は再発行前に最後に受信した Location を境界にすること。境界を選ぶ処理は `run` から切り出した関数にして、スナップショット (再購読の開始時に Location Filter へ渡した開始位置) と再読み値 (session 移行の切替時に `last_received_locations` から読み直した値) が異なる入力で、session 移行は再読み値、request 単位の再発行はスナップショットを選ぶことをユニットテストで固定する。
- `crates/moq-sub/README.md` の購読境界の説明が、映像も選別する実装と一致していること。
- 実 relay の E2E (`publish_and_subscribe_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
