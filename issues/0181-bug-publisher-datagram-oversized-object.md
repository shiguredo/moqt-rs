# moq-pub の datagram 配信が datagram のサイズ上限を超える object で失敗する

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-datagram-oversized-object
- Polished: 2026-09-29

## 目的

`moq-pub` の `--use-datagram` は 1 object = 1 datagram で送るため、datagram のサイズ上限 (path MTU と peer の `max_datagram_frame_size`) を超える object (キーフレーム) で送信が失敗して `Fatal` 終了するか、アプリへ通知されずに破棄されて object が欠落する。
`--input-mp4` (パススルー) はエンコード済みキーフレームをそのまま送るため、一般的な MP4 ではほぼ確実に発生する。
`--input-mp4-reencode` とカメラ経路も `--bitrate` / `--keyframe-interval` 次第で超える。
指定できる組み合わせとして成立させる。

## 現状

- `examples/moq-pub/src/datagram_writer.rs` の `DatagramWriter::write_object` は `StreamHandle::send_datagram` の失敗をそのまま返す。
- `examples/moq-pub/src/pipeline.rs` の映像 / 音声の送信分岐は datagram 経路のエラーを `is_transport_session_end` 以外は `return Err(e)` するため、送信失敗が `Fatal` 終了になる。
- 超過時の挙動は境界が 2 段階あり、どちらも現在の実装で対処していない:
  - peer が広告する `max_datagram_frame_size` (RFC 9221 §3) と `max_udp_payload_size` の小さい方を超える場合: `StreamHandle::send_datagram` がエラーを返す (s2n-quic の default Sender は `DatagramError::ExceedsPeerTransportLimits` を返す。
    本 example も s2n-quic 既定の 65535 (`MaxDatagramFrameSize::RECOMMENDED`) を広告する)。
    このエラーは上の分岐から `Fatal` 終了になる。
  - `max_datagram_frame_size` 以下で path MTU を超える場合: s2n-quic は送信時に datagram を暗黙に破棄する (`Sender::dropped_datagrams` が増えるだけでアプリへの通知は無い。
    draft-ietf-moq-transport-21 §11.2 (Datagrams) も "the Object will be dropped without any explicit notification" (訳: object は明示的な通知なしに破棄される) と定める)。
    この場合は `Fatal` にならず、object の欠落として現れる。
- datagram は 1 つの QUIC パケットに収まらなければならず、分割できない (RFC 9221 §5: "DATAGRAM frames cannot be fragmented")。`--input-mp4` のキーフレームは再エンコードしない MP4 のサンプルそのままで、数 KB〜数百 KB になり得る。典型的な MP4 では path MTU 超過 (暗黙破棄)、高ビットレートの MP4 では `max_datagram_frame_size` 超過 (`Fatal`) が起こり得る。
- カメラ経路の CBR エンコーダでもキーフレームは平均より大きくなるため、同じ上限に達し得る (MP4 入力の追加で確実に踏むようになった)。
- 既存の完了条件 (`issues/closed/0179-add-publisher-mp4-passthrough.md` / `issues/closed/0180-add-publisher-mp4-reencode.md`) は datagram を確認対象外としている。

## 設計方針

- datagram の実際の上限サイズ (peer の `max_datagram_frame_size` と path MTU の実測) とキーフレームサイズの分布を実測し、次のどちらを採るかを決める。
  - キーフレームなど上限を超え得る object は subgroup stream へフォールバックする。
  - 上限を超える object を送れないことを起動時に検証 / 警告し、`--input-mp4` と `--use-datagram` の併用はエラーにする (パススルーはサイズを制御できないため)。
- どちらを採る場合も対象経路を明示し、現状に挙げた全経路の挙動を完了条件へ反映する。フォールバックは object 単位の判定のためカメラ / MP4 パススルー / MP4 再エンコードのすべてに適用できる。起動時エラー案は、起動時にサイズを確定できないカメラ経路と、`--bitrate` で抑えられるが保証のない MP4 再エンコード経路へは適用できないため、これらの扱い (フォールバックの併用など) を決める。
- 実測の参考: s2n-quic の公開 API には peer の `max_datagram_frame_size` を取得する手段が無い (examples/tokio-moq/src/webtransport_h3.rs の `WtClient::connect` 付近に同旨のコメントがある)。上限の推定は `send_datagram` の成否 (エラーの有無) と `Sender::dropped_datagrams` (path MTU 超過による破棄数) の観測で行う。
- どちらを採る場合も、サイズの判定を純関数に切り出して単体テストで固定する。
- MOQT の datagram で object を分割することは想定しない (仕様外)。

## 完了条件

- `--input-mp4` / `--input-mp4-reencode` と `--use-datagram` を併用しても `Fatal` 終了しない。エラーにする場合も起動時に分かる。
- フォールバックを採る場合、上限を超える object が subgroup stream で欠落なく配信される (`Fatal` が消えるだけでは、path MTU 超過の暗黙破棄が残るため)。moq-sub は datagram のメディアを処理しないため、フォールバック先の subgroup 経路で受信・再生を確認する。
- 実測した datagram の上限サイズとキーフレームサイズの例が解決方法に記録されている。
- サイズ判定の単体テストが追加されている。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}
