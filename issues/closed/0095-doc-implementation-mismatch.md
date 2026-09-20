# ドキュメントと実装の不整合を修正する

- Created: 2026-09-16
- Completed: 2026-09-16
- Branch: feature/fix-doc-implementation-mismatch
- Polished: {YYYY-MM-DD}

## 目的

コード内コメントと `docs/IMPLEMENTATION.md` の記述が実装および一次資料と食い違っている箇所がある。
読者が「draft-21 の全メッセージに対応済み」と誤解したり、削除されていない仕様を「削除済み」と
誤認したりするため修正する。

## 現状

- `src/message.rs` の `Fetch` 構造体に `/// PUBLISH_SKIPPED メッセージ (draft-ietf-moq-transport-21
  §9.19 (PUBLISH_SKIPPED))` という doc コメントが付いている。隣接する構造体からコピーされた
  残骸であり、`Fetch` の説明として誤っている (PUBLISH_SKIPPED 自体も未実装)。
- `docs/IMPLEMENTATION.md` の Object Properties / Range Filters の節に
  「TRACK_PROPERTY_FILTER は SUBSCRIBE_TRACKS 削除に伴い PUBLISH 選別を行わない」とある。
  draft-ietf-moq-transport-21 §9.18 に SUBSCRIBE_TRACKS は存在し、削除されていない。
  削除したのは本ライブラリが relay 専用機能を対象外としたことによる (SUBSCRIBE_TRACKS 自体を
  未実装にした) であり、draft の変更ではない。
- `docs/IMPLEMENTATION.md` の MOQT の節は実装済み項目のみを列挙しており、未実装の
  PUBLISH_NAMESPACE (0x06) / NAMESPACE (0x08) / NAMESPACE_DONE (0x0E) / PUBLISH_SKIPPED (0x0F) /
  SUBSCRIBE_NAMESPACE (0x50) / SUBSCRIBE_TRACKS (0x51) と RENDEZVOUS_TIMEOUT (0x04) が
  読み取れない。LOC と MSF の節には「未対応」節があるため、MOQT の節にも同等の記載を置く。

## 設計方針

- `src/message.rs` の誤った doc コメントを `Fetch` の説明に直す。
- `docs/IMPLEMENTATION.md` の SUBSCRIBE_TRACKS の記述を、未実装の理由 (relay 専用機能を
  対象外としていること) が読み取れる表現に直す。
- `docs/IMPLEMENTATION.md` の MOQT の節に「未対応」節を追加し、未実装のメッセージと
  RENDEZVOUS_TIMEOUT を、それぞれ relay 専用機能であることを根拠として明記する。
- 実装の変更は行わない。

## 完了条件

- `src/message.rs` の `Fetch` の doc コメントが実装と一致していること
- `docs/IMPLEMENTATION.md` に MOQT の未対応項目が列挙されていること
- `docs/IMPLEMENTATION.md` の SUBSCRIBE_TRACKS の記述が draft の記載と矛盾しないこと
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` が通ること

## 解決方法

- `src/message.rs` の `Fetch` に付いていた `PUBLISH_SKIPPED` の doc コメントを
  `FETCH メッセージ (draft-ietf-moq-transport-21 §9.11 (FETCH))` に直した。
- `docs/IMPLEMENTATION.md` の TRACK_PROPERTY_FILTER の記述を、draft から削除された仕様では
  なく本ライブラリが relay 専用機能 (SUBSCRIBE_TRACKS) を対象外としているため選別対象が
  無いことを読み取れる表現に直した。
- `docs/IMPLEMENTATION.md` の MOQT の節に「未対応」節を追加し、relay 専用の
  PUBLISH_NAMESPACE (0x06) / NAMESPACE (0x08) / NAMESPACE_DONE (0x0E) / PUBLISH_SKIPPED (0x0F) /
  SUBSCRIBE_NAMESPACE (0x50) / SUBSCRIBE_TRACKS (0x51) と RENDEZVOUS_TIMEOUT (0x04) を列挙した。
  受信時は未知のメッセージ種別と同じく `SESSION_PROTOCOL_VIOLATION` でセッションを閉じる
  ことも併記した。

検証:

- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` が通ることを確認した (実装の変更は無く、doc コメントと
  docs のみの変更である)。
