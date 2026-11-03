# 制御メッセージ長を varint 化し MAX_CONTROL_MESSAGE_SIZE を扱う

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9、§9.1.8、§15.5](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `ControlMessage::encode` / `decode` と `src/decoder.rs` の `MessageDecoder::try_decode_message` は Length を固定 16 ビットで処理し、65535 バイトを固定上限にしている。
`src/parameter.rs` の `SetupOptions` に新しい Setup Option はない。

## 設計方針

Length を vi64 に変更し、Option Type `0x09` の `MAX_CONTROL_MESSAGE_SIZE` を追加する。
最小値・既定値は 65535 で、peer SETUP 受信前は SETUP 自体を含めて既定値を使う。
Option の値は奇数 Type でも長さ付きバイト列ではなく varint である。
`Session` と `crates/tokio-moq/src/moqt_client.rs` の decoder 作成・送信経路へ両方向の上限を渡す。
長さ宣言の段階で上限を検証し、巨大な本文を待ってから拒否しない。
大きな受信上限を許す場合のバッファ量と transport のフロー制御も整合させる。

## 完了条件

- Length に MOQT の vi64 (1 〜 9 バイト) を使い、非最小符号化とヘッダ・本文の分割受信を検証すること。巨大な宣言長の検証で実際に巨大本文を確保しないこと。
- 65535 未満の設定値を拒否し、省略・交渉前・交渉後の上限を区別できること。
- 受信上限超過は `FIELD_LENGTH_EXCEEDED` になり、送信上限超過は送出前に拒否すること。
- 本文長の不一致は `MALFORMED_MESSAGE` とし、他の KVP 値長上限を緩めないこと。

## 解決方法

- 制御メッセージの Length を固定 16 ビットから vi64 に変更した。`ControlMessage::encode` は
  peer の上限を引数に取り、本文長が上限を超える場合は書き出し前に `FieldLengthExceeded` を
  返す。非最小符号化の Length を受理し、宣言長が残りバッファを超える場合は追加データ待ち、
  本文長が Length と一致しない場合は `MalformedMessage` とした。`MessageDecoder` は自側の
  受信上限を保持し、Length を宣言する段階で上限を検証するため、巨大な本文を確保しない。
- Setup Option `MAX_CONTROL_MESSAGE_SIZE` (0x09) を追加した。型番号 0x09 は奇数型のため
  §8.3 (Key-Value-Pair Structure) の規則に従って Length 付きバイト列とし、その中身を varint
  として符号化する。Length と varint のバイト数が一致しない場合は `KeyValueFormattingError`
  とした。最小値・既定値は 65535 で、65535 未満は `InvalidField` で拒否する。
- `Session` に自側の受信上限 (`local_max_control_message_size`) と peer の送信上限
  (`peer_control_message_size_limit`) を追加し、SETUP 受信前 (BeforeSetup) /
  省略 (Omitted) / 広告 (Advertised) を区別できるようにした。自側 SETUP の 0x09 は
  varint 形式であることと重複がないことを検証する。
- `crates/tokio-moq` の制御ストリーム・request ストリームの decoder 作成と送信経路へ
  両方向の上限を伝播し、上限超過はストリームを開く前に失敗させた。
- `src/error.rs` に `FieldLengthExceeded` / `MalformedMessage` / `InvalidField` を追加し、
  `session_error_code` の写像と関連ドキュメントを追随させた。
- テスト: Length の非最小符号化・9 バイト・分割受信・境界値・上限超過の本文未着拒否、
  0x09 の Length 付き wire・Length 不一致・空値・はみ出し・余剰バイト、非既定上限での
  70,000 バイト受理、Session の上限状態と自己 SETUP 検証、peer_harness の統合テスト 3 件、
  PBT の sampler / 分割不変式 / 送信上限境界を追加した。

## 完了条件の検証

- Length に vi64 を使い、非最小符号化と分割受信を検証すること: `tests/test_decoder.rs` の
  `test_non_minimal_length_encoding_is_accepted` / `test_length_varint_split_across_pushes` /
  `test_nine_byte_length_encoding` と PBT の分割不変式で固定した。
- 巨大な宣言長の検証で実際に巨大本文を確保しないこと:
  `test_declared_length_over_limit_is_rejected_without_body` で、ヘッダのみの push で
  `FieldLengthExceeded` になることを固定した。
- 65535 未満の設定値を拒否し、省略・交渉前・交渉後の上限を区別できること:
  `tests/test_parameter.rs` の `max_control_message_size` と `tests/test_session/setup.rs` の
  各テストで固定した。
- 受信上限超過は `FIELD_LENGTH_EXCEEDED`、送信上限超過は送出前に拒否すること:
  `test_declared_length_over_limit_is_rejected_without_body` と
  `test_non_default_receive_limit_accepts_larger_body`、`ControlMessage::encode` の上限引数の
  テストで固定した。
- 本文長の不一致は `MALFORMED_MESSAGE` とし、他の KVP 値長上限を緩めないこと:
  `test_complete_frame_with_malformed_body_is_malformed_message` と
  `setup_option_value_length_limit` の既存テストで固定した。
