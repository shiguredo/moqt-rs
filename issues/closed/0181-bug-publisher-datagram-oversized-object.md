# moq-pub の datagram 配信が datagram のサイズ上限を超える object で失敗する

- Created: 2026-09-29
- Completed: 2026-10-06
- Branch: feature/fix-publisher-datagram-oversized-object
- Polished: 2026-09-29

## 目的

`moq-pub` の `--audio-datagram` は音声トラックの object を datagram で送る。datagram は分割できず
(RFC 9221 §5 "DATAGRAM frames cannot be fragmented")、1 つの QUIC パケットに収まる必要がある。
RFC 9221 §3 は peer が広告した `max_datagram_frame_size` を超える DATAGRAM の送信を MUST NOT とし、
受信側は PROTOCOL_VIOLATION で接続を閉じる MUST を負う。path MTU を超える datagram は s2n-quic が
通知なく破棄し、draft-ietf-moq-transport-22 §11.2 も「上限を超えた object は通知なく破棄される」と
定める。

上限超過が Fatal 終了になるか通知なく破棄されると、原因と対処が分からない。送信前にサイズを判定し、
実サイズ・上限・対処を含むエラーにする。

## 現状

- 2026-10-06 の変更で映像の datagram 配送を廃止した (issues/closed/0203-change-publisher-datagram-tracks.md)。
  datagram を使うのは音声トラックだけになり、`--input-mp4` のキーフレーム (IDR) が datagram の
  上限を超える問題は発生しなくなった
- `examples/moq-pub/src/datagram_writer.rs` の `DatagramWriter::write_object` は
  `StreamHandle::send_datagram` の失敗をそのまま返すため、peer の `max_datagram_frame_size` 超過が
  `examples/moq-pub/src/pipeline.rs` の音声送信分岐から `return Err(e)` となり Fatal 終了する
- path MTU 超過は s2n-quic が datagram を暗黙に破棄する (`Sender::dropped_datagrams` が増えるだけで
  アプリへの通知は無い)
- 上限は peer の `max_datagram_frame_size` (RFC 9221 §3) と path MTU の小さい方であるが、
  s2n-quic の公開 API から peer の値を取得する手段が無いため、example 側で保守的な上限を持つ

## 設計方針

- `--datagram-max-size <BYTES>` を追加し、既定を 1160 にする (RFC 9000 §14 が定める経路の最小
  datagram サイズ 1200 は QUIC パケットヘッダと AEAD タグを含む UDP payload であるため、
  QUIC のパケットヘッダと DATAGRAM frame のヘッダ分を引いた保守的な値)。運用者が経路に合わせて
  調整できるようにする。範囲は 1〜65535 とし、範囲外は起動時にエラーにする
- `DatagramWriter::write_object` はフィルタ評価を通った後、`send_datagram` の前に datagram の
  実サイズ (OBJECT_DATAGRAM のヘッダ + Properties + payload) を判定し、超える場合は
  `Error::DatagramTooLarge { size, limit }` を返す。メッセージには実サイズ・上限・対処
  (`--audio-datagram` を外す / `--datagram-max-size` を上げる) を含める
- サイズ判定は純関数 `check_datagram_size` に切り出し、境界値 (上限と等しい / 上限 + 1) を
  単体テストで固定する
- 暗黙破棄は検出できないため、上限を保守的に取ることで避ける (RFC 9221 §5 は MTU 制約への対処を
  アプリケーションの責務とする)
- `examples/README.md` に `--datagram-max-size` を追記し、CHANGES.md に `[FIX]` を追記する

## 解決方法

`moq-pub` の音声 datagram のサイズ上限超過を、送信前の判定と対処法付きのエラーにした。

- `--datagram-max-size <BYTES>` を追加した (既定 1160)。RFC 9000 §14 (Datagram Size) が定める
  経路の最小 datagram サイズ 1200 は QUIC パケットヘッダと AEAD タグを含む UDP payload である
  ため、QUIC のパケットヘッダと DATAGRAM frame のヘッダ分を引いた保守的な値を既定にした。
  範囲は 1〜65535 とし、範囲外と数値でない値は起動時にエラーにする
- `examples/moq-pub/src/datagram_writer.rs` の `DatagramWriter::write_object` は、フィルタ評価を
  通った後・`send_datagram` の前に `check_datagram_size(buf.len(), max_size)` で MOQT の
  OBJECT_DATAGRAM (ヘッダ + Properties + payload) の実サイズを判定し、超える場合は
  `Error::DatagramTooLarge { size, max_size }` を返す
- エラーメッセージは実サイズ・上限・対処 (`--audio-datagram` を外す / `--datagram-max-size` を
  上げる) を含み、`main` の `Fatal: ...` として利用者に見える。原因不明の Fatal 終了と
  path MTU 超過による通知なしの破棄を避ける
- datagram 配送を使わない場合 (`--audio-datagram` なし / 音声トラックを送らない) は
  `--datagram-max-size` を黙って捨てずに警告する
- テストは `check_datagram_size` の境界 (上限未満 / 上限と等しい / 上限 + 1)、`--datagram-max-size`
  の解析 (既定値 / 指定 / 数値でない値 / 範囲外)、`Error::DatagramTooLarge` のメッセージを
  固定した
- `examples/README.md` に `--datagram-max-size` を追記し、CHANGES.md に `[FIX]` を追記した
- 引用した仕様は、分割不可と MTU 制約への対処が RFC 9221 §5 (Behavior and Usage)、
  `max_datagram_frame_size` 超過の MUST NOT が同 §3 (Transport Parameter) である

## 完了条件

- 上限を超える object を送ろうとした場合に、実サイズと上限と対処を含むエラーで終了し、
  原因不明の Fatal 終了や通知なしの破棄にならないこと
- 上限以内の object は従来どおり送信されること
- サイズ判定の単体テスト (上限未満 / 上限と等しい / 上限 + 1) と `--datagram-max-size` の解析テスト
  (既定値 / 指定 / 数値でない値 / 範囲外) があること
- 上限超過のメッセージに実サイズ・上限・対処が含まれることがテストで固定されていること
- `examples/README.md` と CHANGES.md が追随していること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `prek run --all-files` が通ること
