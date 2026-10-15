# 音声 Object が live と FETCH の両方で届いたときに二重に送出される

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-audio-playback-boundary
- Polished: 2026-10-15

## 目的

moq-sub は購読直後に直近 4 Group を FETCH し、購読より前に publish された音声を live と同じ経路へ
流す。FETCH の要求 range の終端は `SUBSCRIBE_OK` の Largest Object であり、
draft-ietf-moq-transport-22 §3.1.4 (Largest Object) は Largest Object が「まだ到着しうる Object」を
指すこと ("can still be arriving")、§3.2 (Fetch) は「FETCH の range が Largest Object を含む場合、
その Object を完全に含める」と定める。したがって境界の Object は live と FETCH の両方で届きうる。

その結果、同じ音声が 2 回 `audio_tx` へ送出され、録画にも 2 回書き込まれる。mp4.rs の `insert` は同一 pts を別キーで 2 つ保持し `try_flush` が両方を書き出すため、flush 前に 2 枚届けば MP4 に重複が残る。実 relay の E2E でも 3 回の実行のうち 1 回、境界の Object が live の直後に二重に送出されたことを観測している (E2E の PTS 検査は `>=` のため重複そのものは検出しない)。

FETCH 応答が live より後に届く場合の巻き戻りは、送出済み TIMESTAMP との比較で防いでいる。本 issue は、この 2 つを個別の防御ではなく「購読境界より前の音声は鳴らさない・録らない」という 1 つの規則で解消する。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` は映像と音声の両方について、`SUBSCRIBE_OK` の Largest Object を終端とする直近 4 Group を FETCH する (`FETCH_RECENT_GROUPS = 4`)。
- FETCH 応答は `read_audio_fetch_stream` から `AudioDelivery::deliver` へ流れ、live の datagram / subgroup stream と同じ経路で `audio_tx` へ送出される。録画は `deliver` の先頭で境界や順序に関わらず行われる。
- `AudioDeliveryOrder::admit` は送出済みの TIMESTAMP と比較し、古い Object だけを落とす。同じ TIMESTAMP の Object は「別の Object でありうる」として送出するため、境界の Object が live と FETCH の両方で届くと 2 回送出され、録画も 2 回行われる。
- `deliver` は `admit` が `Stale` を返すと復号の前に return するため、境界以前の Object は復号されないことがある (live が先に届くと、FETCH の Object はすべて `Stale` になる)。
- 映像は `VideoDecodeOrder` が (Group ID, Object ID) で同一 Object を復号の対象から外すため `frame_tx` への二重送出は起きないが、録画は admission より前に行われ (`deliver_video_object` は「録画は復号の可否に関わらず行う」)、mp4.rs の基準時刻による破棄は最初のキーフレーム以前だけを対象とするため、境界の Object の二重書き込みは映像側にも同じ露出がある。本 issue では映像を変更しない。
- 音声の購読には Location Filter を載せていないため、live は Largest Object の位置を含みうる。
- moqt-js は relay の cache から届く Object を `CatchUpGate` で選別する。境界は `SUBSCRIBE_OK` の `LARGEST_OBJECT` (§9.20.17) で、境界以前は「復号はするが提示・再生はしない」。境界が無いときはすべて live として扱い、位置が分からない Object も再生する。判定は Location の比較であり、TIMESTAMP は使わない。

## 設計方針

- `SUBSCRIBE_OK` の Largest Object (LARGEST_OBJECT) を音声の境界として保持し、境界以下の Location の Object は次のように扱う。
  - Audio Config の検証だけを行い、復号はしない。復号器は datagram / subgroup stream / FETCH 応答で 1 つを共有しており、live の音声を復号したあとに古い音声で状態を進めると、次に届く live の音声が不連続になるためである (Audio Config は Properties から検証できるため、復号しなくても適用は落ちない)。moqt-js は cache の音も復号するが、moqt-js は Audio Config の適用が復号経路にあるためであり、moq-sub の経路には当てはまらない。
  - `audio_tx` へ送出しない。
  - 録画しない (購読前の音声を残す意味が無く、同一 pts の二重書き込みを防ぐ)。
  - TIMESTAMP の送出順の判定 (`admit`) は呼ばない。送出も録画も復号もしないため判定を通す必要が無く、判定の基準を古い TIMESTAMP で始めないためでもある。
  - catch-up の観測 (`catch_up.observe`) へ渡さない (古い TIMESTAMP で lag が跳ね、復号器が作り直される)。
- 境界より後の Object は従来どおり送出・録画・復号する。TIMESTAMP による送出順の判定は、live の datagram が並び替わって届く場合 (draft-ietf-moq-transport-22 §2.1.2 (Object States) / §2.3.1 (Group IDs)) の防御として残す。
- Largest Object が無い場合 (未 publish) は境界を設定せず、すべて live として扱う (送出・録画する)。
- 位置が分からない Object は境界と比べられないため送出する。実経路では 3 経路すべてで Location が取れる (datagram は `ObjectDatagram`、subgroup stream は `header.group_id` と `DecodedSubgroupObject::object_id`、FETCH は `DecodedFetchObject`) ため、これは契約の確認である。
- 判定は Location (Group ID, Object ID) の比較で行い、TIMESTAMP の比較で代用しない。
- GOAWAY 移行の再購読では、移行先の live が流れる前に (`resubscribe_tracks` の直後、datagram 受信タスクへ新しい alias 表を渡す前に) 新しい Largest Object で境界を更新する。
- 音声の FETCH は残す。ただし境界以下の音声を送出・録画・復号しない規則により、音声の FETCH の実利は Audio Config の検証に限られる (音声にキーフレームは無い)。FETCH 自体をやめるかは別途判断し、本 issue では扱わない。
- 購読に Next Object の Location Filter を載せれば live と FETCH の重複は構造的に消える (draft §3.3.1 の注記 "these Objects do not pass a filter that starts at the Next Object" / §3.4 (Fill Semantics) は fill range と購読のフィルタが独立に評価されると定める) が、購読で受け取る範囲の意味が変わり映像と catalog を含む購読全体の判断になるため、本 issue では境界による選別を採る。
- 映像は現状 (`VideoDecodeOrder` と録画の基準時刻) を維持し、本 issue では変更しない。映像の録画の重複は別 issue として扱う。
- 送出順の基準 (`last_pts_us`) は GOAWAY 移行で初期化しない (移行前の基準を引き継ぐ)。移行先の TIMESTAMP が基準より古い場合は落ちるが、既存挙動であり本 issue の対象外とする。

## 完了条件

- 境界以下の Location の音声 Object が `audio_tx` へ送出されないこと。
- 境界以下の Location の音声 Object が録画されないこと (MP4 を読み戻してサンプル数で確認する)。
- 境界以下の音声 Object でも Audio Config の検証が行われること (`audio_config_handled` で確認する)。
- 境界以下の音声 Object が catch-up の観測へ渡らないこと (未観測のままであることを `AudioReceiveCatchUpStats` で確認する)。
- 境界の位置の Object が live と FETCH の両方で届いても、送出も録画も二重にならないこと。
- Largest Object が無い場合はすべて送出されること。
- 位置が分からない Object は送出されること。
- 境界より後の live の音声が従来どおり送出され、並び替わって届いた古い音声が TIMESTAMP の判定で落ちること (境界ありの構成で確認する)。
- 3 経路 (datagram / subgroup stream / FETCH 応答) の位置が、それぞれの経路の Group ID と Object ID から取れること (実エンコーダと実デコーダを使ったユニットテストで確認する)。
- 再購読で境界が更新されること (`AudioDeliveryOrder::set_boundary` のユニットテストで確認する)。
- ユニットテストで確認するとした項目が `examples/moq-sub` のユニットテストで固定されていること。
- 購読直後の境界と FETCH 応答の経路の配線は、実 relay の E2E (`publish_and_subscribe_audio_datagram_roundtrip`) を `RUST_LOG=debug` と `--nocapture` で実行し、境界以下の音声が落ちるログ (`Skipping an audio object before the subscription boundary`) で確認すること。
- 実 relay の E2E (`publish_and_subscribe_audio_datagram_roundtrip`) が通ること。

残る隙間として次を記録する。3 経路の呼び出し側 (位置を渡す箇所) と再購読の配線は transport の stream を必要とするためユニットテストでは囲えない。購読直後と FETCH 応答の経路は E2E のログで確認するが、datagram / subgroup stream の呼び出し側と再購読の配線は移行の E2E が無いためコードの確認に留める。

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
