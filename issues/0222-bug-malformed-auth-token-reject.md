# 不正な AUTHORIZATION_TOKEN を MALFORMED_AUTH_TOKEN でメッセージ単位に拒否する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-malformed-auth-token-reject
- Polished: 2026-10-10

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は 2 つの失敗を区別して
いる。Token 構造が decode できない場合は「the receiver MUST close the Session with
KEY_VALUE_FORMATTING_ERROR」、構造は正しいが内容が不正な場合は「The receiver of a message
containing a well-formed Token structure that is otherwise invalid MUST reject that message with
an MALFORMED_AUTH_TOKEN error.」である。後者はメッセージ単位の拒否であり、MALFORMED_AUTH_TOKEN は
REQUEST_ERROR のコード 0x4 として定義される (§12.3 (Request Error Codes) / §16.11.2)。
現状の example は両者を区別せず、`MalformedAuthToken` をセッション終了またはメッセージ破棄として
扱うため、MUST を満たしていない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `session_error_code` と
  `examples/moq-sub/src/pipeline.rs` の `session_error_code` は
  `MessageError::KeyValueFormattingError` だけを `SESSION_KEY_VALUE_FORMATTING_ERROR` に写し、
  それ以外を `SESSION_PROTOCOL_VIOLATION` に写す。`MessageError::MalformedAuthToken` は後者に
  落ちる。moq-pub は main ループへ `Error::Moqt` として伝播した場合に `close_on_message_error`
  で、moq-sub は main ループの `Err(Error::Moqt)` 経路と data stream 側の
  `request_session_termination` で、それぞれこの写像によりセッションを閉じる。
- ただし `examples/tokio-moq/src/moqt_client.rs` の `spawn_peer_bidi_accept_task` (relay が
  転送する SUBSCRIBE / FETCH の先頭メッセージを受け取る経路) は、decode 失敗を warn ログを
  出してメッセージを破棄するだけである。ここでは `MalformedAuthToken` でも
  `KeyValueFormattingError` でもセッションは閉じず、REQUEST_ERROR も返らない。
  そのため本経路はどの失敗種別でも §8.9 の MUST を満たさない。
- 両関数の doc には「`MalformedAuthToken` は §8.9 がメッセージ単位の reject を MUST で求めるが、
  decode を中断した時点で Request ID が得られずメッセージ単位の reject を送れない」という制約が
  書かれている。制約の原因は library のメッセージ decode がトークン検証を含めて一括で行われ、
  失敗時に Request ID を保持しない点にある。なおこの doc の「セッション終了として
  PROTOCOL_VIOLATION に写す」は main ループへ伝播した場合の話であり、破棄経路には当てはまらない。
- `src/message_parameter.rs` の検証は、重複する (Token Type, Token Value) の登録のような
  「構造は正しいが内容が不正」な場合に `MessageError::MalformedAuthToken` を返す。
  同じ検証は `src/parameter.rs` の `SetupOptions::decode` にもあり、SETUP でも
  `MalformedAuthToken` になり得る。
- AUTHORIZATION_TOKEN を持つメッセージ (SUBSCRIBE / FETCH / REQUEST_UPDATE / PUBLISH /
  TRACK_STATUS) は Request ID をパラメータより前に符号化するため、パラメータ部の decode に
  失敗しても Request ID 自体は復元できる。ただし現在の decode は失敗時に Request ID を返さない。

## 設計方針

- decode を中断せず Request ID を確保してから、当該メッセージだけを
  REQUEST_ERROR (MALFORMED_AUTH_TOKEN) で拒否する経路を作る。対象は
  `spawn_peer_bidi_accept_task` が読む先頭メッセージ (SUBSCRIBE / FETCH / PUBLISH /
  TRACK_STATUS) と、確立済み request stream 上の後続メッセージ (REQUEST_UPDATE など) の両方。
  - 対象メッセージの Request ID を、パラメータ部の decode 失敗時にも取得できるようにする。
    実現は次のどちらかでよい (どちらを選んでも完了条件を満たすこと)。
    - library の decode がメッセージ種別と Request ID をエラー情報として復元する
    - `MessageParameters` のトークン一意性検証を decode から分離する
      (この場合、`MalformedAuthToken` を返す decode を固定している既存テスト
      `tests/test_message_parameter.rs` の `decode_rejects_duplicate_token_type_value` などと、
      公開 `MessageParameters::decode` の契約が変わる点に注意する)
  - §9.20.2 (AUTHORIZATION TOKEN Parameter) がトークンを許可するメッセージは、本ライブラリの
    実装範囲 (SUBSCRIBE / FETCH / REQUEST_UPDATE / PUBLISH / TRACK_STATUS) ではすべて
    Request ID を持つ。トークンを許可しない応答系メッセージ (SUBSCRIBE_OK / REQUEST_OK /
    FETCH_OK / PUBLISH_STATE_NOTIFY) にトークンが載った場合は §9.20.1 (Parameter Scope) の
    PROTOCOL_VIOLATION によるセッション終了を維持する。なお decode は一意性検証を scope 検証より
    先に行うため、これらのメッセージでも重複トークンは `MalformedAuthToken` になり得るが、
    セッション終了に倒す点は変わらない。
  - SETUP (`SetupOptions::decode` 由来の `MalformedAuthToken`) も Request ID を持たないため
    セッション終了に倒す。
- Token 構造自体が decode できない場合 (`KeyValueFormattingError`) は §8.9 の MUST どおり
  KEY_VALUE_FORMATTING_ERROR でセッションを閉じる。`spawn_peer_bidi_accept_task` の破棄もやめ、
  relay 転送の SUBSCRIBE / FETCH でもセッションが閉じるようにする。
- SETUP の `MalformedAuthToken` の終了コードは、§12.2 (Session Termination Codes) に定義される
  MALFORMED_AUTH_TOKEN (0x16) と PROTOCOL_VIOLATION (0x3) のどちらを使うか、根拠とともに
  doc に書く (現行コメントの制約をそのまま残さない)。
- example 側の `session_error_code` の写像は、library が MalformedAuthToken をメッセージ拒否へ
  変換した後に残る経路 (SETUP など Request ID を持たない経路) だけを扱うよう整理する。

## 完了条件

- 構造は正しいが内容が不正なトークンを含む SUBSCRIBE / FETCH で、セッションを閉じずに
  `MALFORMED_AUTH_TOKEN` の REQUEST_ERROR が当該 request に返ること。
- Token 構造が decode できない場合は `KEY_VALUE_FORMATTING_ERROR` でセッションが閉じること。
  relay 転送の SUBSCRIBE / FETCH を含め、decode 失敗を黙って破棄する経路が残らないこと。
- SETUP の `MalformedAuthToken` の終了コードが doc と実装で一致していること。
- 上記がテストで固定されていること。実 relay が必要な E2E を除き、モック・スタブを使わずに
  検証できる層 (library の decode / 検証層 `tests/` と example の単体テスト) で行うこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
