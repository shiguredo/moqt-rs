# FETCH ストリームの malformed track 分類と End of Range 後続検証を仕様に合わせる

- Created: 2026-10-19
- Completed: 2026-10-19
- Branch: feature/fix-fetch-stream-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) と §11.4.1.2 (End of Range) に照らし、FETCH 応答ストリームの検証に 2 つの不備がある。1 つは同一 Group の Object ID 非増加と Group 順序違反が `MessageError::ProtocolViolation` で返るのにメッセージは "malformed track" であり、同じ関数内の Priority 変更・最終 Object 超過の `MalformedTrack`
と分類が食い違うこと。もう 1 つは End of Range が宣言した範囲の内側に後続 Object が来ても検出しないこと。

## 現状

- `src/stream/decoder.rs` の `validate_fetch_object` は、同一 Group の Object ID 非増加と Group 順序違反を `ProtocolViolation` で返し、Priority 変更と最終 Object 超過は `MalformedTrack` で返す。
- `tests/test_stream/decoder.rs` は Group 順序違反に `ProtocolViolation` を期待しており、分類の誤りを固定している。
- End of Range を処理した後も `validation.last_object` (最後の実 Object) としか比較しないため、Ascending の実 Object (10, 5) → EOR (10, 20) → 続く Object (10, 15) を受理する。§11.4.1.2 は「last serialized Object から EOR Location まで inclusive の Location」を宣言範囲とする。
- 影響: 分類を型で分岐する利用者が、購読 cancel で足りる違反をセッション終了として扱う。EOR 範囲内の不正 Object を見逃す。

## 設計方針

- 同一 Group の Object ID 非増加・Group 順序違反を `MalformedTrack` に統一する。テストの期待値も更新する。
- 最後の EOR Location を保持し、後続 Object が範囲内なら `MalformedTrack` として拒否する。Ascending / Descending を区別する。
- 同一 Group で Object ID が減少するケースと EOR 範囲内のケースのテストを追加する。

## 完了条件

- `validate_fetch_object` の違反分類が §12.1 の malformed track に統一されていること。
- EOR 範囲内の後続 Object を拒否すること。
- 上記を `tests/test_stream` と PBT で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

実ファイルと一次資料を照合した結果、本 issue が求める 2 つの変更はいずれも現行仕様と既存の設計判断に反するため、コード変更を行わず closed にする。

### malformed track 分類の統一について

`src/stream/decoder.rs` の `validate_fetch_object` は、同一 Group の Object ID 非増加を次のように返す。

> `return Err(MessageError::ProtocolViolation("malformed track: object IDs in the same FETCH response group must be strictly increasing"));`

Group 順序違反も同じく `ProtocolViolation` を返す。

> `return Err(MessageError::ProtocolViolation("malformed track: FETCH response groups are not in ascending order"));`

これは closed 0111 (`issues/closed/0111-bug-malformed-track-cancel-classification.md`) が意図的に行った分類である。0111 の「解決方法」は次のとおり。

> 分類しなかった検出 (`ProtocolViolation` のまま):
> - `FetchStreamDecoder::validate_fetch_object` の Object ID 昇順違反 (§2.2 (Subgroups)) と Group 順序違反 (§9.11 (FETCH))

0111 の完了条件も「昇順違反と Group 順序違反では `MessageError::ProtocolViolation` が返ることがテストで固定されていること」としており、`tests/test_stream/decoder.rs` の `test_fetch_rejects_ascending_group_in_descending_mode` が `Err(MessageError::ProtocolViolation(_))` を期待している。

draft-ietf-moq-transport-22 §12.1 は malformed track の条件を列挙したうえで「The above list of conditions is not considered exhaustive.」とするが、0111 は §2.2 と §9.11 由来の違反を malformed track に含めないと決めている。本 issue はこの判断を覆す根拠を示していない。実装すると 0111 の完了条件とテストを破るため、分類の変更は行わない。

なお、メッセージ文字列が "malformed track: ..." で始まるために `Display` が "protocol violation: malformed track: ..." となる点は文言の不整合である。variant を `MalformedTrack` に変える根拠にはならないため、本 issue では扱わない。

### End of Range 後の Object 検証について

draft-ietf-moq-transport-22 §11.4.1.2 は次のとおり定める。

> All Objects with Locations between the last serialized Object, if any, and this Location, inclusive, either do not exist (when Serialization Flags is 0x8C), are unknown (0x10C), or timed out (0x20C).

しかし §2.1.2 は、非存在を記録した後に Object を受信する場合について次のとおり定める。

> All signals that an Object does not exist are authoritative.
>
> Since Objects can be delivered out of order, an endpoint can receive an Object after it has already recorded that the Object does not exist (e.g., via a FETCH gap from one source and delayed delivery via a subscription). This is not a protocol error and the Track is not malformed.

したがって、EOR が宣言した範囲内の Location に後続 Object が来ても、それは protocol error でも malformed track でもない。受信側は非存在を維持してその Object を破棄する。`FetchStreamDecoder` が `MalformedTrack` を返して拒否する設計は §2.1.2 に反する。

さらに、本 issue の具体例「Ascending の実 Object (10, 5) → EOR (10, 20) → 続く Object (10, 15)」は wire 上実現できない。`src/stream/decoder.rs` の `resolve_object` は、Group ID Delta が無い場合の Object ID を prior Object ID + Object ID Delta（Delta が無ければ +1）として解決する。draft-ietf-moq-transport-22 §11.4.1.1 も次のとおり定める。

> When the Group ID Delta field is not present, the Object ID is the prior Object's ID plus the Object ID Delta if present. If Object ID Delta is not present, the Object ID is the prior Object's ID plus one, regardless of which group it belongs to.

EOR 後の prior Object ID は EOR の 20 なので、同一 Group 内の後続 Object の解決値は 20 以上になる。Object ID 15 は到達できない。一時的な統合テストで次を実測した。

- `FetchStreamEncoder` の `encode_object` は (10, 5) → EOR (10, 20) → (10, 15) を `ProtocolViolation` で拒否する。
- 生バイトで Object ID Delta を最大値 (2^62-1) にしても解決値は 4611686018427387923 となり、15 にはならない。
- EOR (10, 20) 直後に Object ID Delta 0 を送ると、decoder は (10, 20) を受理する。これは §2.1.2 が許容する「非存在記録後の Object 受信」に該当する。

したがって、EOR 範囲内の後続 Object を `MalformedTrack` として拒否する変更は、§2.1.2 と §11.4.1.1 の delta 解決規則に反する。

### 結論

- malformed track 分類の変更は closed 0111 の設計判断を覆すもので、根拠がない。
- EOR 範囲内の後続 Object を `MalformedTrack` として拒否する設計は §2.1.2 に反する。
- 具体例 (10, 15) は delta 解決の制約上実現できない。
- したがって、本 issue の 2 つの変更は仕様・既存方針と整合せず、実装すると誤った変更が入る。コード変更を行わず closed にする。
