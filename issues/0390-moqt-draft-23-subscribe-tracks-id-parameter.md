# PUBLISH の SUBSCRIBE_TRACKS_ID パラメータを扱う

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.6、§9.8、§9.19.21](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の定数・`value_encoding` に SUBSCRIBE_TRACKS_ID がなく、`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` にもない。
draft 23 の正当な PUBLISH にこの値が含まれると、未知パラメータとして codec で失敗する。

## 設計方針

Type `0x36` の varint パラメータと型付きアクセサを追加し、PUBLISH でのみ受理・送信する。
元の SUBSCRIBE_TRACKS の Request ID を保持・利用側に通知できるようにする。
ライブラリは SUBSCRIBE_TRACKS 自体を実装しないため、その要求との照合 state は追加しない。受信時は ID を関連付け情報として保持・通知し、対応する要求がないことだけを理由に拒否やセッション終了を行わず、通常の PUBLISH の検証とアプリケーションによる受理・拒否判断へ進める。
送信 API は型と PUBLISH のスコープを検証し、対応する namespace 要求の存在検証は追加しない。SUBSCRIBE_TRACKS 起因の PUBLISH にのみ指定する契約を API の説明へ明記する。

## 完了条件

- Type `0x36` の golden バイト列と PUBLISH の往復を検証すること。
- SUBSCRIBE / FETCH / REQUEST_UPDATE など許可されない文脈では INVALID_PARAMETER になること。
- 対応する namespace 要求がない場合も ID を保持・通知し、通常の PUBLISH の検証・受理判断へ進むこと。ID の照合 state や namespace 機能を追加しないこと。

## 依存・関連

- 関連: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)

## 解決方法

`src/message_parameter.rs` に Type `0x36` の `PARAM_SUBSCRIBE_TRACKS_ID` を追加し、`value_encoding` に varint を登録して型付きアクセサ `MessageParameters::subscribe_tracks_id()` を追加した。
`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` に追加し、draft-ietf-moq-transport-23 §9.8 (PUBLISH) の列挙を 11 種に更新した。
§9.19.1 (Parameter Scope) は許可されないメッセージ型での受信を INVALID_PARAMETER で閉じる MUST を定めるため、SUBSCRIBE / FETCH / REQUEST_UPDATE / TRACK_STATUS / PUBLISH_OK では従来どおりスコープ検証で拒否される。
受信した PUBLISH の値は `Subscription::subscribe_tracks_id` に保持し、アプリが `Session::subscription` から読めるようにした。
ライブラリは SUBSCRIBE_TRACKS を実装しないため、要求との照合 state は追加していない。
対応する namespace 要求が無い PUBLISH でも値の保持だけを行い、拒否やセッション終了はせずに通常の PUBLISH の検証 (認証トークン、Range Filter、予約名前空間、Track Alias、Track Property など) とアプリによる受理・拒否判断へ進む。
送信側は `Session::send_publish` の doc に、SUBSCRIBE_TRACKS の結果として送る PUBLISH にだけ指定し、対応する要求の存在検証は行わない契約を明記した。
`MessageParameters::resulting_publish_parameters` は SUBSCRIBE_TRACKS_ID を引き継がない。この値は要求のパラメータではなく、publisher が PUBLISH ごとに設定する元要求の Request ID だからである (§3.6.2 (Parameters on SUBSCRIBE_TRACKS) / §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter))。
`docs/moqt.md` と `skills/shiguredo-moqt/SKILL.md` にパラメータとアクセサを追記した。

## 完了条件の検証

- Type `0x36` の golden バイト列と PUBLISH の往復を検証すること。
  - `tests/test_message_parameter.rs` の `subscribe_tracks_id_accessor_and_golden_bytes` が count=1 / delta=0x36 / 値 300 (0x81 0x2C) の golden バイト列と encode / decode 双方向の一致、アクセサの値、不在時の `None` を検証する。
  - `tests/test_message.rs` の `subscribe_tracks_id_golden_bytes_and_round_trip` が SUBSCRIBE_TRACKS_ID を持つ PUBLISH の golden バイト列 (Type 0x1D) と decode / encode 往復、decode 後のアクセサ値を検証する。
  - `tests/test_session/parameter_rules.rs` の `publish_with_subscribe_tracks_id_accepted_without_request` がセッション API の送受信往復、ワイヤ上の値、送信側と受信側双方の `Subscription::subscribe_tracks_id` を検証する。
- SUBSCRIBE / FETCH / REQUEST_UPDATE など許可されない文脈では INVALID_PARAMETER になること。
  - `tests/test_message.rs` の `subscribe_tracks_id_rejected_outside_publish` が SUBSCRIBE / FETCH / REQUEST_UPDATE / TRACK_STATUS の encode と、SUBSCRIBE / FETCH / REQUEST_UPDATE の wire decode を `InvalidParameter` で拒否することを検証する。
  - `tests/test_session/parameter_rules.rs` の `subscribe_tracks_id_rejected_outside_publish` が受信した SUBSCRIBE / FETCH / TRACK_STATUS を `CloseSession(INVALID_PARAMETER)` にし、REQUEST_ERROR を送らないことを検証する。
  - 同ファイルの `request_update_with_subscribe_tracks_id_closes_session` が Established 購読への REQUEST_UPDATE を `INVALID_PARAMETER` で閉じることを検証する。
  - 同ファイルの `publish_ok_with_subscribe_tracks_id_rejected` が PUBLISH_OK の送信をスコープ検証で拒否し、購読状態を変えないことを検証する。
- 対応する namespace 要求がない場合も ID を保持・通知し、通常の PUBLISH の検証・受理判断へ進むこと。ID の照合 state や namespace 機能を追加しないこと。
  - `publish_with_subscribe_tracks_id_accepted_without_request` が SUBSCRIBE_TRACKS を一度も受信していないセッションで PUBLISH を受理し、`subscribe_tracks_id == Some(42)` を保持したまま PUBLISH_OK で受理でき、購読が PUBLISH 由来の 1 件だけで namespace 要求の state を作らないことを検証する。
  - `tests/test_message_parameter.rs` の `does_not_inherit_subscribe_tracks_id` が `resulting_publish_parameters` で SUBSCRIBE_TRACKS_ID を引き継がないことを検証する。
- 検証コマンド: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` がすべて成功した。
