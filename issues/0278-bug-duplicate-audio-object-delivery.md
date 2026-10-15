# 音声 Object が live と FETCH の両方で届いたときに二重に送出される

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-duplicate-audio-object-delivery
- Polished: {YYYY-MM-DD}

## 目的

`examples/moq-sub` の音声は、購読より前に publish された Group を FETCH で取得して live と同じ再生経路へ流す。FETCH の要求 range の最新 Object が live の最初の Object と一致する場合、同じ Object が 2 回送出される。二重に届いた音声は再生側で同じ時刻のサンプルとして扱われ、MP4 への二重書き込みや同じ音の重複再生になりうるため、同一 Object の判定方法を決めて重複送出をなくす。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `AudioDeliveryOrder::admit` は送出済みの TIMESTAMP と比較し、古い Object だけを `AudioDeliveryAdmission::Stale` として落とす。同じ TIMESTAMP の Object は送出する (同じ TIMESTAMP を持つ別の Object を落とさないため)。
- `AudioDelivery::deliver` は録画 → Audio Config の検証 → 送出順の判定 → デコード → `audio_tx` への送出の順に処理する。`audio_tx` は製品では jitter buffer の入力、E2E テストでは `audio_frames` である。
- FETCH の要求 range は `plan_fetch_range` が決め、終端は SUBSCRIBE_OK の Largest Object である。live の最初の Object が Largest Object と同じ場合、その Object は FETCH 応答と live の両方で届く。
- 実測 (実 relay での E2E) では 3 回の実行のうち 1 回、live の Group の直後に同じ TIMESTAMP の FETCH 応答 Object が送出された。
- 映像は `shiguredo_moqt::video_decode_order::VideoDecodeOrder` が (Group ID, Object ID) を鍵に判定するため、同じ Object を二重に復号しない。音声は TIMESTAMP しか見ていない。

## 設計方針

次のどちらを採るかを決め、選んだ理由を `AudioDeliveryOrder` の doc に記録する。

- (a) (Group ID, Object ID) を鍵にして同一 Object を落とす。映像の `VideoDecodeOrder` と同じ考え方で、TIMESTAMP が同じ別の Object を落とさない。FETCH 応答は Group と Object ID を持つため、`AudioDelivery::deliver` まで渡す経路を広げる必要がある。
- (b) 同じ TIMESTAMP の Object を落とす (`admit` の比較を `<=` にする)。変更は小さいが、TIMESTAMP を共有する別の Object を落とす可能性がある。

どちらを採る場合も、TIMESTAMP を持たない Object の扱い (現状は送出し、基準も変えない) は変えない。

## 完了条件

- 同じ Object が live と FETCH の両方で届いたときに、音声が 1 回だけ送出されること。
- TIMESTAMP を共有する別の Object を落とさないこと (採った方針に応じてテストで固定する)。
- 上記が `examples/moq-sub` のユニットテストで固定されていること。
- 実 relay の E2E (`publish_and_subscribe_audio_datagram_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
