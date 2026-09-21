# PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP の二重インスタンスを検出する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-prior-gap-duplicate-instance
- Polished: 2026-09-21

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

§12.1 の malformed 条件の列挙は網羅ではないため、§10.8 / §10.9 の条件も malformed に含まれる。

> The above list of conditions is not considered exhaustive.

## 現状

- `src/object_properties.rs` の `ObjectProperties::find_varint` は mutable リストを先に探し、見つからなければ IMMUTABLE_PROPERTIES の内側を探す。インスタンス数は数えない
- `src/object_properties.rs` の `ObjectPropertyTracker::observe_decoded_object` は `ObjectProperties::prior_group_id_gap` / `prior_object_id_gap` が返す 1 つの値だけを使う。§10.8 / §10.9 の他の条件 (gap が group ID / object ID を超える、同一 group で値が異なる、gap が受信済みの group / object を覆う、gap 内の group / object を受信する) は実装済みで、二重インスタンスだけが未実装である
- `src/object_properties.rs` の `decode_kv_pairs` は同一リスト内の重複 (delta == 0) を拒否するが、mutable リストと内側をまたぐ重複は見えない。`validate_immutable_inner` も内側のリストだけを検証する
- `ObjectProperties::encode` も同じで、mutable リスト内の重複と内側の構造だけを検証する。このため mutable に 0x3C、内側に 0x3C を置いた Object は encode / decode とも通り、tracker は mutable 側の値だけを観測する
- `tests/test_object_properties.rs` の `mutable_list_takes_precedence_over_immutable_inner` が getter の現挙動 (mutable 優先) を固定している

## 設計方針

- 0x3C / 0x3E については mutable と IMMUTABLE_PROPERTIES 内側の両方を数え、2 つ以上あれば Malformed Track として扱う
- `ObjectProperties` に private な `fn count_instances(&self, prop_type: u64) -> usize` を追加し、mutable リストと IMMUTABLE_PROPERTIES 内側の両方でその型を持つインスタンス数を返す。同一モジュール内の `ObjectPropertyTracker` からしか使わないため公開しない
- `ObjectPropertyTracker::observe_decoded_object` から 0x3C / 0x3E について呼び、2 以上なら malformed とする。wire 経路 (`observe_object`) と API 経路 (`observe_decoded_object`) の両方が同じ 1 か所で判定される
- 返すエラーは `MessageError::MalformedTrack` とする。これは 0111 が `MessageError` に追加する variant であり、§12.1 の列挙条件に対応する検出を分類する 0111 の方針に従う。`MessageError::ProtocolViolation` を返すと 0111 の分類を戻す手戻りになる
- **本 issue は 0111 の実装後に着手する**。0111 が未実装の間は `MessageError::MalformedTrack` が存在しないため、`ProtocolViolation` で暫定実装しない
- Session 側の写像 (0111 が定める `RecvDataStreamError::MalformedTrack` と `Session::terminate_malformed_track` の呼び出し) は 0111 の担当であり、本 issue では変更しない。§12.1 の検出は購読単位の cancel であり、セッションは閉じない
- 他の型の「mutable 優先」は変えない (§10.7 の MUST search both の実装として維持する)。0x3C / 0x3E の getter も mutable 優先のままとし、二重インスタンスは tracker が拒否する。`tests/test_object_properties.rs` の `mutable_list_takes_precedence_over_immutable_inner` は getter の優先規則のテストとして残し、malformed 検出は tracker の責務であることをテストで区別する
- IMMUTABLE_PROPERTIES の内側が破損していてデコードできない場合、内側のインスタンスは数えられない。wire 経路では decode 時に破損が拒否されるため、この制約が観測されるのは `push` で組み立てた API 入力だけである。この境界を doc とテストに明記する
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加する (公開 API の変更は無い)

## 完了条件

- mutable と IMMUTABLE_PROPERTIES 内側の両方に 0x3C を置いた入力を `ObjectPropertyTracker` が `MessageError::MalformedTrack` で拒否することを固定するテストが追加されていること
- 既存の §10.8 / §10.9 条件のテスト (`tracker_rejects_prior_group_gap_covering_received_group` ほか 4 件) は 0111 の再分類後に `MalformedTrack` を期待する形になっていること。0111 で更新されていない場合は本 issue で更新する
- 同じ入力を 0x3E で行った場合も拒否されることを固定するテストが追加されていること
- Session 経由で受信した場合に subscription が Malformed Track で終端されることを固定するテストが追加されていること (subgroup / datagram のいずれか 1 経路以上)
- mutable のみに 1 個、内側のみに 1 個の場合はどちらも受理されることを固定するテストが追加されていること (非退行)。両方を同時に持つ Object の拒否と対にして、1 個ずつの入力が受理されることを新規に固定する
- §10.8 / §10.9 の他の条件 (値の不一致、gap の重なり) の既存テストが非退行であること
- 内側が破損している入力で内側のインスタンスを数えない境界が、テストまたはコメントで明示されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
