# 映像 Object を復号してよいか Group の順序と欠落から判定する

- Created: 2026-10-10
- Completed: {YYYY-MM-DD}
- Branch: feature/add-video-decode-order

## 目的

draft-ietf-moq-transport-22 §2.1 は "Objects can be delivered out of order" とする。Group ごとに別の Subgroup ストリームで届くため、前の Group の末尾が次の Group の先頭より後に届くことがある。次の Group のキーフレームを復号した後に前の Group の delta を復号すると、参照フレームが壊れて映像が崩れる。Group 内で Object が欠けた後の delta も、参照するフレームが無いまま復号することになる。

受信側はどの Object がどのフレームを参照しているかを知らないため、壊れた映像を出すよりキーフレームを待つほうがよい。復号してよい Object だけを通す判定が要る。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `decode_video_stream` は、Subgroup ストリームごとに独立して Object を取り出して復号し、`frame_tx` へ送る。Group の順序や Object ID の欠落を判定していない
- 同じ Group の Object が順序どおりに届くことは Subgroup ストリームの順序保証で担保されるが、Group をまたいだ順序と Group 内の欠落は判定されていない
- moqt-js の `src/videoDecodeOrder.ts` の `VideoDecodeOrder` がこの判定を持つが、moqt-rs には移植されていない
- moqt-js は Prior Object ID Gap (draft-ietf-moq-transport-22 §10.9) を使って、欠落が「存在しない Object」として示されている場合と、示されていない欠落を区別する。§2.1 は "A gap in the observed Object IDs does not by itself convey any information about the skipped Objects" とするため、示されない欠けは欠落として扱う

## 設計方針

moqt-js の `VideoDecodeOrder` を移植し、純粋な状態機械としてライブラリに置く (Sans-I/O。時刻も I/O も要らない)。

- `src/video_decode_order.rs` に `VideoDecodeOrder` を置く。`src/lib.rs` に `pub mod video_decode_order;` を追加する
- `VideoObjectPosition { group_id: u64, object_id: u64, is_key_frame: bool, prior_object_id_gap: u64 }` を受け取り、`VideoObjectAdmission` (`Decode` / `Skip { reason }`) を返す
- `VideoObjectSkipReason` は `Stale` (復号中の Group より古い Group の Object、または直前に復号した Object 以前の Object ID。重複か遅着) と `MissingReference` (参照するフレームが欠けているためキーフレームを待っている) の 2 つ
- 判定の規則
  - 復号中の Group より古い Group の Object は復号しない
  - 同じ Group で直前に復号した Object ID 以前の Object は復号しない
  - キーフレームは、復号中の Group より新しい Group なら復号を始め直す
  - 同じ Group の delta は、直前に復号した Object の次の Object ID のときだけ通す
  - 間の Object ID が欠けている場合は、Prior Object ID Gap がその分の非存在を示すときに限り連続とみなす。示されない欠けは欠落として扱い、次のキーフレームまで delta を捨てる
- 欠落の後でキーフレームを待っている間も、欠落を検出した Group を保持する (それより古い Group の Object を古いとして捨てるため)
- `prior_object_id_gap_of` に相当する関数を用意し、Object Properties から Prior Object ID Gap を読む。Property が無ければ 0
- `reset` で初期状態にする (decoder を作り直したとき)
- 1 Group の Object を 1 本の Subgroup で送る publisher を前提とする。1 Group を複数の Subgroup に分ける publisher の Object ID の飛びも欠落として扱う (どの Object を参照しているか受信側は判断できないため、崩れた映像ではなくキーフレーム待ちに倒す)
- `examples/moq-sub/src/pipeline.rs` の映像の経路で、復号の前に判定を通し、`Skip` の理由をログに出す

## 完了条件

- `VideoDecodeOrder` がライブラリから使えること
- moq-sub の映像の経路が判定を通した Object だけを復号すること
- 古い Group / 重複・遅着 / 欠落 / Prior Object ID Gap の 4 つの場合の判定と、キーフレームで復号を始め直すことがテストで固定されていること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

{未着手}
