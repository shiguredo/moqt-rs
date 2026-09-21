# PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP の二重インスタンスを検出する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-prior-gap-duplicate-instance
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) と §10.9 (Prior Object ID Gap) は、それぞれのプロパティについて 1 つの Object に 2 つ以上含まれてはならない MUST NOT を定め、違反した Track を malformed とする。

```text
A Track is considered malformed (see Section 12.1) if any of the following conditions are detected:

*  An Object contains more than one instance of Prior Group ID Gap.
```

(条件の箇条書きは抜粋。§10.9 では "Prior Object ID Gap" を対象に同じ条件が並ぶ)

> An Object MUST NOT contain more than one instance of this property.

§10.7 (Immutable Properties) は、複数の値を許すプロパティに限り、同じ Property Type を mutable リストと IMMUTABLE_PROPERTIES 内側の両方に置けるとしている。

> If a Property allows multiple values, the same Property Type MAY appear in both the mutable list and inside Immutable Properties, unless prohibited by the Property specification.

PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP は複数の値を許さないため、両方に置かれた時点で 2 インスタンスとなり malformed になる。現状はこれを検出しない。

## 現状

- `src/object_properties.rs` の `ObjectProperties::find_varint` は mutable リストを先に探し、見つからなければ IMMUTABLE_PROPERTIES の内側を探す。インスタンス数は数えない
- `src/object_properties.rs` の `ObjectPropertyTracker::observe_decoded_object` は `ObjectProperties::prior_group_id_gap` / `prior_object_id_gap` が返す 1 つの値だけを使う。§10.8 / §10.9 の他の条件 (gap が group ID / object ID を超える、同一 group で値が異なる、gap が受信済みの group / object を覆う、gap 内の group / object を受信する) は実装済みで、二重インスタンスだけが未実装である
- `src/object_properties.rs` の `decode_kv_pairs` は同一リスト内の重複 (delta == 0) を拒否するが、mutable リストと内側をまたぐ重複は見えない。`validate_immutable_inner` も内側のリストだけを検証する
- `ObjectProperties::encode` も同じで、mutable リスト内の重複と内側の構造だけを検証する。このため mutable に 0x3C、内側に 0x3C を置いた Object は encode / decode とも通り、tracker は mutable 側の値だけを観測する
- `tests/test_object_properties.rs` の `mutable_list_takes_precedence_over_immutable_inner` が getter の現挙動 (mutable 優先) を固定している

## 設計方針

- 0x3C / 0x3E については mutable と IMMUTABLE_PROPERTIES 内側の両方を数え、2 つ以上あれば Malformed Track として扱う
- `ObjectProperties` に「型ごとのインスタンス数 (mutable + IMMUTABLE_PROPERTIES 内側)」を返す API を追加し、`ObjectPropertyTracker::observe_decoded_object` から 0x3C / 0x3E について呼ぶ。wire 経路 (`observe_object`) と API 経路 (`observe_decoded_object`) の両方が同じ 1 か所で判定される
- 返すエラーは `MessageError::ProtocolViolation` とし、Session の既存の malformed track 経路 (`session_error_from_data_message` から `terminate_malformed_track`) に乗せる。新しいエラー型は増やさない
- 他の型の「mutable 優先」は変えない (§10.7 の MUST search both の実装として維持する)。0x3C / 0x3E の getter も mutable 優先のままとし、二重インスタンスは tracker が拒否する。`tests/test_object_properties.rs` の `mutable_list_takes_precedence_over_immutable_inner` は getter の優先規則のテストとして残し、malformed 検出は tracker の責務であることをテストで区別する
- IMMUTABLE_PROPERTIES の内側が破損していてデコードできない場合、内側のインスタンスは数えられない。wire 経路では decode 時に破損が拒否されるため、この制約が観測されるのは `push` で組み立てた API 入力だけである。この境界を doc とテストに明記する

## 完了条件

- mutable と IMMUTABLE_PROPERTIES 内側の両方に 0x3C を置いた入力を `ObjectPropertyTracker` が Malformed Track として拒否することを固定するテストが追加されていること
- 同じ入力を 0x3E で行った場合も拒否されることを固定するテストが追加されていること
- Session 経由で受信した場合に subscription が Malformed Track で終端されることを固定するテストが追加されていること (subgroup / datagram のいずれか 1 経路以上)
- mutable のみに 1 個、内側のみに 1 個の場合はどちらも受理されることを固定するテストが追加されていること (非退行)
- §10.8 / §10.9 の他の条件 (値の不一致、gap の重なり) の既存テストが非退行であること
- 内側が破損している入力で内側のインスタンスを数えない境界が、テストまたはコメントで明示されていること
