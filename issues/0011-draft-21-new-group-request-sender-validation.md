# REQUEST_UPDATE の NEW_GROUP_REQUEST を送信側で検証する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-new-group-request-sender-validation

## 目的

draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter) の MUST NOT を送信側で守る。DYNAMIC_GROUPS 非対応の Track に対する `REQUEST_UPDATE` に `NEW_GROUP_REQUEST` を載せて送信できないようにする。

## 現状

`src/session/subscription/send.rs` の `send_update_for_subscription` は、`FORWARD` を `validate_forward`、`LOCATION_FILTER` を `location_filter_update`、`FILL_PARAMETERS` を `validate_outgoing_fill_parameters` で送信前に検証しているが、`NEW_GROUP_REQUEST` は検証していない。`subscription` が `DYNAMIC_GROUPS` を持たない場合でも送出できてしまう。

受信側の `handle_update_for_subscription` (`src/session/subscription/recv.rs`) は同じ MUST を実装している。送信側だけが素通りしている。

根拠 (draft-ietf-moq-transport-21 §9.20.20):

> "A subscriber MUST NOT send this parameter in REQUEST_UPDATE if the Track did not include the DYNAMIC_GROUPS Property with value 1. A subscriber MAY include this parameter in SUBSCRIBE without foreknowledge of support."

`tests/test_session/parameter_rules.rs` の該当テストは「送信は成功し server 側で拒否される」ことを前提にしており、送信側の MUST NOT 違反を固定している。

## 設計方針

`send_update_for_subscription` で `parameters.new_group_request().is_some() && !subscription.dynamic_groups` を検出し、`SESSION_PROTOCOL_VIOLATION` の `SessionError` を返す。SUBSCRIBE での `NEW_GROUP_REQUEST` は foreknowledge 無しでも許可されるため、REQUEST_UPDATE のみを対象とする。

## 完了条件

- DYNAMIC_GROUPS 非対応 Track への `REQUEST_UPDATE` で `NEW_GROUP_REQUEST` が送信前に拒否されること
- `tests/test_session/parameter_rules.rs` のテストが送信側拒否を検証する形に更新されていること
- SUBSCRIBE での `NEW_GROUP_REQUEST` は従来どおり許容されること
