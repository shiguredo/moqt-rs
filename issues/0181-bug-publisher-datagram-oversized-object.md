# moq-pub の datagram 配信が datagram のサイズ上限を超える object で失敗する

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-datagram-oversized-object
- Polished: {YYYY-MM-DD}

## 目的

`moq-pub` の `--use-datagram` は 1 object = 1 datagram で送るため、datagram のサイズ上限を超える object (キーフレーム) で送信が失敗し `Fatal` 終了する。`--input-mp4` (パススルー) はエンコード済みキーフレームをそのまま送るため、一般的な MP4 ではほぼ確実に発生する。`--input-mp4-reencode` とカメラ経路も `--bitrate` / `--keyframe-interval` 次第で超える。指定できる組み合わせとして成立させる。

## 現状

- `examples/moq-pub/src/datagram_writer.rs` の `DatagramWriter::write_object` は `StreamHandle::send_datagram` の失敗をそのまま返す。
- `examples/moq-pub/src/pipeline.rs` の映像 / 音声の送信分岐は datagram 経路のエラーを `is_transport_session_end` 以外は `return Err(e)` するため、送信失敗が `Fatal` 終了になる。
- QUIC / WebTransport の datagram は 1 パケットに収まるサイズ (MTU 未満) しか送れない。`--input-mp4` のキーフレームは再エンコードしない MP4 のサンプルそのもので数 KB〜数百 KB になり得る。
- カメラ経路の CBR エンコーダでもキーフレームは平均より大きくなるため、同じ上限に達し得る (MP4 入力の追加で確実に踏むようになった)。
- 既存の完了条件 (`issues/closed/0179-add-publisher-mp4-passthrough.md` / `issues/closed/0180-add-publisher-mp4-reencode.md`) は datagram を確認対象外としている。

## 設計方針

- datagram の実際の上限サイズとキーフレームサイズの分布を実測し、次のどちらを採るかを決める。
  - キーフレームなど上限を超え得る object は subgroup stream へフォールバックする。
  - 上限を超える object を送れないことを起動時に検証 / 警告し、`--input-mp4` と `--use-datagram` の併用はエラーにする (パススルーはサイズを制御できないため)。
- どちらを採る場合も、サイズの判定を純関数に切り出して単体テストで固定する。
- MOQT の datagram で object を分割することは想定しない (仕様外)。

## 完了条件

- `--input-mp4` / `--input-mp4-reencode` と `--use-datagram` を併用しても `Fatal` 終了しない。エラーにする場合も起動時に分かる。
- 実測した datagram の上限サイズとキーフレームサイズの例が解決方法に記録されている。
- サイズ判定の単体テストが追加されている。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}
