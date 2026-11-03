# 空の Track Name を PROTOCOL_VIOLATION で拒否する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §2.4.1、§6.5、§9.4.2.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message/common.rs` の `decode_track_name` / `encode_track_name` と `validate_full_track_name` は空名を許す。
SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の各経路は長さの上限だけを検証し、session-level track の空名を旧 DOES_NOT_EXIST の扱いにする経路もある。

## 設計方針

Track を名指す要求と REDIRECT target で最低 1 バイトを要求し、受信の違反は PROTOCOL_VIOLATION、送信は状態登録前に拒否する。
namespace-scoped REDIRECT の Track Name は空でなければならないため、共有 decode 関数で一律に空を禁止せず文脈別に検証する。
Track Namespace が 0 フィールドであることは引き続き許す。

## 完了条件

- 通常 namespace と `.session` の空 Track Name を、wire と直接 Session API の双方で拒否すること。
- 1 バイトの名前と空 Track Namespace が正しく使えること。
- namespace-scoped REDIRECT の空名を維持し、非空名は INVALID_FIELD になること。

## 解決方法

### 空名の分類

空の Track Name は `MessageError::ProtocolViolation` として拒否する。根拠は draft-ietf-moq-transport-23 §2.4.1 (Track Naming) が
"Each Track Name MUST contain at least one byte. If an endpoint receives a Track Name with a length of 0, it MUST close the session with a PROTOCOL_VIOLATION."
と終了コードを明示していることである。§12.2 (Session Termination Codes) の INVALID_FIELD (0x301) も「制御メッセージとしては解析できたがフィールド値が許されない」として字面上は該当するが、
同節の PROTOCOL_VIOLATION の定義が「より具体的なコードが無い場合に使う」と述べる一方で §2.4.1 は PROTOCOL_VIOLATION を名指しして MUST を課しているため、明示された分類を採る。
§12.2 の優先規則 ("uses DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to the other codes") は、4,096 バイト超過の判定を空名の判定より先に置くことで満たす
(空名は 0 バイトのため実際には両立しないが、判定順序を他の検証と揃える)。

### メッセージ層 (`src/message/common.rs` / `src/message.rs`)

- `src/message/common.rs` に `validate_track_name` を追加した。空名を `ProtocolViolation` で拒否する。
- `decode_track_name` / `encode_track_name` は変更しない。length-prefixed な Track Name の共有デコード関数は文脈を持たないため、空名の禁止は「Track を名指す」ことを知っている呼び出し側で行う。
- `src/message.rs` の `Subscribe` / `Publish` / `Fetch` / `TrackStatus` の encode / decode で、`validate_full_track_name` の直後に `validate_track_name` を呼ぶ。wire の decode が空名を PROTOCOL_VIOLATION に写像する。
- `Redirect` は空の Track Name を表現でき、encode / decode では空の可否を判定しない。REQUEST_ERROR の decode は応答先が track-scoped か namespace-scoped かを知らず、
  namespace-scoped request の Redirect target は空でなければならない (§9.4.2.1 (REDIRECT)) ため、一律に空を禁止すると仕様違反になる。応答先の種別を知っている Session 層が検証する。
- Track Namespace の 0 フィールドは引き続き許す (変更なし)。

### Session 層

- `Session::validate_peer_track_name` / `Session::validate_outgoing_track_name` を追加し、`validate_track_name` の結果をセッション終了コード / 送信エラーへ写像する。
- 受信: `accept_incoming_track_request` (SUBSCRIBE / TRACK_STATUS)、`handle_peer_publish`、`handle_peer_fetch` の先頭で検証し、`fail` で `Closing` に遷移させる。
  request 単位の拒否 (REQUEST_ERROR) ではなくセッション終了を MUST とする検証のため、request ID の parity 検証や予約名前空間の拒否より前に置いた。
- `.session` 名前空間の空名を DOES_NOT_EXIST の理由 "empty track name in .session namespace" にしていた分岐を削除した。空名は §2.4.1 の検証でセッション終了になり、
  `.session` の非空名だけが従来どおり DOES_NOT_EXIST ("session-level track does not exist") になる。
  `src/session/subscription/recv.rs` と `src/session/fetch.rs` の両方を揃えた。
- 送信: `send_subscribe` / `send_publish` / `send_fetch` / `send_track_status` は `check_peer_goaway` の直後、request_id の採番と状態登録の前に拒否する。
- REDIRECT の受信: `Session::validate_peer_redirect` で Redirect target の空の Track Name を PROTOCOL_VIOLATION にする。
- REDIRECT の送信: `Session::send_request_error` で、購読の終端などの状態遷移より前に空の Redirect target を拒否する。
- 本ライブラリが送受信する request は SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の track-scoped 4 種だけであるため、
  Redirect target には track-scoped の規則 (1 バイト以上) だけを適用する。
- 送信側で空名を拒否する理由: 送ってしまうと peer が PROTOCOL_VIOLATION でセッションを閉じる一方、自側は送信成功と状態登録を済ませてしまう。
  encode 層でも拒否するが、API 呼び出し時点で拒否して状態の食い違いを作らない。

### テストと PBT

- `tests/test_message.rs` に `track_name_length` モジュールを追加し、4 種 × 通常 / `.session` 名前空間の空名を encode / decode の双方で拒否することを固定した。
- 直接 Session API の受信経路 (空名でセッション終了、REQUEST_ERROR を送らない) と送信経路 (拒否時に request_id を消費しない) のテストを SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS に追加した。
- `pbt/tests/prop_message.rs` の track-scoped request の Track Name 生成を 0 バイト許容から 1 バイト以上へ変更した (空名は encode が拒否するためラウンドトリップ対象から外れる)。

## 完了条件の検証

| 完了条件 | 結果 |
| --- | --- |
| 通常 namespace と `.session` の空 Track Name を wire と直接 Session API の双方で拒否 | 満たす |
| 1 バイトの名前と空 Track Namespace が正しく使える | 満たす |
| namespace-scoped REDIRECT の空名を維持 | 満たす (codec が空名を保持) |
| namespace-scoped REDIRECT の非空名を INVALID_FIELD にする | 該当経路なし (下記) |

- wire (codec) の拒否: `tests/test_message.rs` の `track_name_length::request_messages_with_empty_track_name_are_rejected_on_encode` が 4 種の encode を、
  `track_name_length::request_messages_with_empty_track_name_are_rejected_on_decode` が 4 種 × 通常 / `.session` 名前空間の decode を `MessageError::ProtocolViolation` で固定する。
- 直接 Session API の拒否: `tests/test_session/subscription/handshake.rs` の `subscribe_empty_track_name_closes_session` /
  `subscribe_session_namespace_empty_track_name_closes_session` / `publish_empty_track_name_closes_session` /
  `publish_session_namespace_empty_track_name_closes_session`、`tests/test_session/track_status.rs` の
  `recv_track_status_empty_track_name_closes_session`、`tests/test_session/fetch/validation.rs` の
  `fetch_session_namespace_empty_track_closes_session` / `fetch_regular_namespace_empty_track_closes_session` が、
  `SESSION_PROTOCOL_VIOLATION` と `SessionState::Closing` を確認し、状態登録と REQUEST_ERROR の送信が無いことも固定する。
- 送信 API の拒否: `tests/test_session/subscription/handshake.rs` の `send_subscribe_empty_track_name_rejected` / `send_publish_empty_track_name_rejected`、
  `tests/test_session/track_status.rs` の `send_track_status_empty_track_name_rejected`、`tests/test_session/fetch/validation.rs` の
  `send_fetch_empty_track_name_rejected` が、拒否後の最初の送信が request_id 0 を使うこと (採番を消費しないこと) を固定する。
- 1 バイトの名前と空 Track Namespace: `tests/test_message.rs` の `track_name_length::one_byte_track_name_with_empty_namespace_round_trips` が 4 種の encode / decode を、
  `tests/test_session/subscription/handshake.rs` の `subscribe_with_one_byte_track_name_and_empty_namespace_accepted` が Session の送受信と状態登録を固定する。
- `.session` の非空名は従来どおり DOES_NOT_EXIST: `tests/test_session/fetch/validation.rs` の `fetch_session_namespace_non_empty_track_rejected`、
  `tests/test_session/track_status.rs` の `recv_track_status_reserved_namespaces_are_rejected`、
  `tests/test_session/subscription/handshake.rs` の `publish_reserved_namespace_precedes_parameter_range`、`fetch_namespace_non_first_field_session_accepted` が固定する。
- namespace-scoped REDIRECT の空名: `tests/test_message.rs` の `track_name_length::redirect_with_empty_track_name_round_trips` と
  `request_error_details::redirect_golden_bytes` が、空の Track Name を持つ REDIRECT の encode / decode を固定する。
- track-scoped REDIRECT の空名: `tests/test_session/request_stream.rs` の `redirect_empty_track_name_for_subscribe_closes_session` が受信、
  `send_request_error_redirect_empty_track_name_rejected` が送信を `SESSION_PROTOCOL_VIOLATION` で拒否することを固定する。非空名は
  `redirect_nonempty_track_name_for_subscribe_accepted` / `redirect_nonempty_track_name_for_fetch_accepted` が受理を固定する。
- namespace-scoped REDIRECT の非空名の INVALID_FIELD は該当経路が無い。本ライブラリは SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS を送らない
  (`CODEBASE.md` のリレー非対応) ため、§9.4.2.1 が対象とする「namespace-scoped request の Redirect」を受信する経路が存在しない。codec は応答先の種別を知らないため
  一律に判定せず、Session 層は track-scoped の規則だけを適用する。将来 namespace-scoped request を実装するときに、その request を追跡する state と合わせて追加する。
- 検証コマンド: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` がすべて成功する。
