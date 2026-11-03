# 制御メッセージ長を varint 化し MAX_CONTROL_MESSAGE_SIZE を扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-control-message-length-limit
- Polished: {YYYY-MM-DD}

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
