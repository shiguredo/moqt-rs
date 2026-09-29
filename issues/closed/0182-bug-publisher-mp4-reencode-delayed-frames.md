# moq-pub の再エンコード配信で AV1 の遅延フレームが周回時に欠落する

- Created: 2026-09-29
- Completed: 2026-09-30
- Branch: feature/fix-mp4-reencode-delayed-frames
- Polished: 2026-09-29

## 目的

`moq-pub --input-mp4-reencode` の AV1 入力で、dav1d が内部に保持する遅延フレーム (alt-ref など) が周回の先頭で破棄され、毎周フレームが欠落する。フレーム遅延のある AV1 でも欠落しないようにする。

## 現状

- `examples/moq-pub/src/mp4/reencode.rs` の `VideoDecoder::reset` は AV1 で `Av1Decoder::reset` を呼び、`shiguredo_dav1d::Decoder::flush` で周回の先頭にデコーダ状態を戻す。crate のドキュメントどおり `flush` は「未消費のデータやバッファ中のフレームは全て破棄される」ため、遅延していたフレームが失われる。
- `examples/moq-pub/src/decoder/av1.rs` の `Av1Decoder::decode` は `next_frame()` の列挙が `None` を返した時点で止まる。`shiguredo_dav1d` の `Decoder::finish` の NOTE には「`dav1d_get_picture` が EAGAIN を返した後にもう一度呼び出すと、強制的にバッファ内のデコード画像が取得される」とあり、周回の先頭で吐き切っていない。
- そのため、フレーム遅延のある AV1 (例: alt-ref を使うエンコーダ出力) では周回の末尾フレームが毎周欠落する。moq-pub 内蔵の AV1 エンコーダ (libaom realtime) と moq-sub の `--mp4` 保存は遅延が無いため往復確認では気づきにくい。

## 設計方針

- 周回の末尾 (サンプルが尽きた分岐) の `reorder.drain()` より前に、dav1d の遅延フレームを吐き切る処理を追加する。
  `VideoDecoder::reset` (dav1d の `flush`) はこの直後に呼ばれるため「reset の前」でもあるが、`reorder.drain()` の後に置くと、吐き出したフレームは `VideoReorder` の `next_index` が末尾のため積まれるだけで供給されず、その後の `VideoReorder` の作り直しで破棄される (`pts_queue.clear()` も `reset` の後)。
  `next_frame()` を `None` の後にもう一度呼ぶ挙動を crate の NOTE に従って使い、取得したフレームは既存の PTS 待ち行列と表示順の並べ替え (`VideoReorder`) に流してから供給する。
- 吐き切る処理は `Av1Decoder` にメソッドを追加し、`VideoDecoder` 経由で呼ぶ。Video Toolbox はフレームを遅延させないため何もしない。
- 遅延フレームが取得できることは、遅延のある AV1 (alt-ref を含むストリーム) を入力にした実機確認で固定する。テスト用の遅延入力を用意できる場合は単体テストでも固定する。

## 完了条件

- フレーム遅延のある AV1 (alt-ref を含む) を `--input-mp4-reencode` で配信したとき、周回をまたいでもフレームが欠落しない。
- 遅延フレームの吐き出しが単体テストまたは実機確認で固定されている。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

`examples/moq-pub/src/decoder/av1.rs` に `Av1Decoder::drain_delayed` を追加し、`examples/moq-pub/src/mp4/reencode.rs` の周回末尾で `reset` (dav1d の `flush`) より先に呼ぶようにした。

- `Av1Decoder::drain_delayed`: `shiguredo_dav1d::Decoder::finish` を呼んだ後、`next_frame` の列挙が空になるまで繰り返してバッファ内のデコード済みフレームを取り出す。フレームの取り出しは `decode` と共通の `take_frames_until_eagain` に集約した (dav1d の NOTE は「`dav1d_get_picture` が EAGAIN を返した後にもう一度呼び出すと、強制的にバッファ内のデコード画像が取得される」と定める)
- `VideoDecoder::drain_delayed`: AV1 は `Av1Decoder::drain_delayed` に委譲し、Video Toolbox (H.264 / H.265) はフレームを遅延させないため常に空を返す
- `reencode.rs` の周回末尾 (`next_sample` が `None` を返した分岐) で、`reorder.drain()` より前に `decoder.drain_delayed()` を呼び、取得したフレームを `drain_delayed_frames` で PTS に対応付けて `VideoReorder` へ流す。`reorder.drain()` の後に置くと、吐き出したフレームは `next_index` が末尾のため積まれるだけで供給されず、その後の `VideoReorder` の作り直しで破棄される
- `drain_delayed_frames`: 遅延フレームの入力サンプルは前の周回で `PtsQueue::push_input` に登録済みであるため、残っている PTS のうち表示順で最小のものを対応付ける。対応付けられないフレームは供給せずに破棄する (warn ログ)。対応付けのための `PtsQueue::peek_output` と `PtsQueue::take_specific_output` を追加した

追加したテスト:

- `pts_queue_peeks_and_takes_the_minimum_pts`: 遅延フレームへ対応付ける PTS を表示順の最小値として確認してから取り出せること、先頭でない PTS は取り出さないこと、PTS が余っていなければ確認できないことを固定した

未実施の確認:

- 実機確認は relay が必要なため行っていない。フレーム遅延のある AV1 (alt-ref を含む) を用意できれば、`--input-mp4-reencode` で 2 周させて供給フレーム数が入力サンプル数の 2 倍になることで確認できる
- 手元で用意できた AV1 (SVT-AV1 で生成した MP4) は dav1d が各サンプルで 1 フレームを即座に出力する (遅延フレームが発生しない) ため、この入力では `drain_delayed` が空を返すこと (既存経路に影響しないこと) だけを確認した
- 内蔵の AV1 エンコーダ (libaom realtime) は lookahead を持たないため、alt-ref を含むテスト入力をリポジトリ内で生成できない

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
