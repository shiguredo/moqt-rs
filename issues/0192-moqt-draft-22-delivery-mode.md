# Object Forwarding Preference を Delivery Mode に追従する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/change-delivery-mode
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 は「Object Forwarding Preference」を「Delivery Mode」に改名し、Delivery Mode が Original Publisher の初回送信で確定することを明記した (A.1 #1886, #1891, #1914、§2.1.1)。重複 Object の Malformed 条件も "different Delivery Mode than previously observed" に更新された (§12.1 条件 7、§7.1)。実装の用語・公開 API・仕様引用を draft-22 に合わせ、トレーサビリティを回復する。

## 現状

- `src/object_properties.rs` の `ObjectFieldTracker` / `ObjectFieldRecord` は `is_subgroup: bool` で mode (subgroup 経由 / datagram 経由) を表現する。`ObjectFieldTracker` と `observe_object_fields` / `observe_object_fields_with_content` は `pub` の公開 API で、`is_subgroup: bool` を引数に取る。
- 重複 Object の検出は実装済みで、subgroup / datagram で同じ `(group_id, object_id)` を観測して `is_subgroup` が異なる場合に `terminate_malformed_track` で当該購読を終了する (draft-22 §12.1 条件 7 / §7.1 相当)。reason 文字列と doc は "Forwarding Preference" のままである。
- mode は Object 単位で、同一 Track 内で Object ごとに異なる mode を許容する。draft-22 §5.1 のスケジューリングも datagram 優先を前提に mode 混在を許しており、この点は挙動変更不要である。
- `src/session/subscription/validation.rs`、`src/stream/fetch.rs`、`src/session/data.rs`、`tests/test_session/data_stream.rs`、`tests/test_object_properties.rs` のコメント・テスト名・assert 文字列が旧用語のままである。

## 設計方針

- `DeliveryMode` enum (`Subgroup` / `Datagram`) を新設し、`ObjectFieldRecord` の `is_subgroup: bool` と `observe_object_fields` / `observe_object_fields_with_content` の引数を置き換える (公開 API の破壊的変更)。
- reason 文字列を "different Delivery Mode" に変更し、テストの assert 文字列も追従する。
- 引用を draft-22 の正しい節へ更新する。用語と確定規則は §2.1.1 (Delivery Mode / Original Publisher が初回送信で確定 / 購読では Delivery Mode に従って送る MUST)、重複検出は §7.1 と §12.1 条件 7。draft-22 本文の "see Section 11.1.2" は Object Properties を指す参照ミスなので Delivery Mode の根拠には使わない。
- 受信側の Malformed 検出挙動は変えない。送信側で「確定済み mode と異なる送信」を強制する追跡は追加しない (relay 非対応であり、Original Publisher 自身の初回送信が確定行為であるため)。この規約を doc に明記する。
- 公開 API の破壊的変更のため `CHANGES.md` の `## develop` に `[CHANGE]` として記載する。

## 完了条件

- `DeliveryMode` が導入され、`ObjectFieldTracker` の公開 API と内部表現が Delivery Mode ベースになっていること
- 旧用語 (Forwarding Preference) が `src/object_properties.rs` のコード・doc・reason 文字列と関連テストから消え、draft-22 の節が引用されていること
- 受信側の重複 Object 検出の挙動が変わらないこと (既存テストが通ること)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること

## 解決方法

{未着手}
