# 不正な AUTHORIZATION_TOKEN を MALFORMED_AUTH_TOKEN でメッセージ単位に拒否する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-malformed-auth-token-reject
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は 2 つの失敗を区別して
いる。Token 構造が decode できない場合は「the receiver MUST close the Session with
KEY_VALUE_FORMATTING_ERROR」、構造は正しいが内容が不正な場合は「The receiver of a message
containing a well-formed Token structure that is otherwise invalid MUST reject that message with
an MALFORMED_AUTH_TOKEN error.」である。後者はメッセージ単位の拒否であり、MALFORMED_AUTH_TOKEN は
REQUEST_ERROR のコード 0x4 として定義される (§12.3 (Request Error Codes) / §16.11.2)。
現状の example は両者を区別せず、セッション全体を閉じているため MUST を満たしていない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `session_error_code` と
  `examples/moq-sub/src/pipeline.rs` の `session_error_code` は
  `MessageError::KeyValueFormattingError` だけを `SESSION_KEY_VALUE_FORMATTING_ERROR` に写し、
  それ以外を `SESSION_PROTOCOL_VIOLATION` に写す。`MessageError::MalformedAuthToken` は後者に
  落ちる。
- moq-pub は `close_on_message_error` で、moq-sub は終了依頼の経路で、それぞれセッションを
  閉じる。
- 両関数の doc には「`MalformedAuthToken` は §8.9 がメッセージ単位の reject を MUST で求めるが、
  decode を中断した時点で Request ID が得られずメッセージ単位の reject を送れない」という制約が
  書かれている。制約の原因は library のメッセージ decode がトークン検証を含めて一括で行われ、
  失敗時に Request ID を保持しない点にある。
- `src/message_parameter.rs` の検証は、重複する (Token Type, Token Value) の登録のような
  「構造は正しいが内容が不正」な場合に `MessageError::MalformedAuthToken` を返す。
- AUTHORIZATION_TOKEN を持つメッセージ (SUBSCRIBE / FETCH / REQUEST_UPDATE / PUBLISH など) は
  Request ID をパラメータより前に符号化するため、パラメータ部の decode に失敗しても
  Request ID 自体は復元できる。

## 設計方針

- decode を中断せず Request ID を確保してから、当該メッセージだけを
  REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否する経路を作る。
  - 対象メッセージの Request ID を、パラメータ部の decode 失敗時にも取得できるようにする
    (library 側で「メッセージ種別と Request ID の復元」を行うか、パラメータ検証を分離する)。
  - 拒否するのは request stream 上のメッセージに限る。Request ID を持たない経路が残る場合は、
    セッション終了に倒す条件と根拠を doc に書く (現行コメントの制約をそのまま残さない)。
- Token 構造自体が decode できない場合は従来どおり KEY_VALUE_FORMATTING_ERROR でセッションを
  閉じる。
- example 側の `session_error_code` の写像は、library が MalformedAuthToken をメッセージ拒否へ
  変換した後に残る経路だけを扱うよう整理する。

## 完了条件

- 構造は正しいが内容が不正なトークンを含む SUBSCRIBE / FETCH で、セッションを閉じずに
  `MALFORMED_AUTH_TOKEN` の REQUEST_ERROR が当該 request に返ること。
- Token 構造が decode できない場合は `KEY_VALUE_FORMATTING_ERROR` でセッションが閉じること
  (現行維持)。
- 上記 2 つがテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
