# Object Forwarding Preference を Delivery Mode に追従する

- Created: 2026-10-02
- Completed: 2026-10-03
- Branch: feature/change-delivery-mode
- Polished: 2026-10-02

## 目的

draft-ietf-moq-transport-22 は「Object Forwarding Preference」を「Delivery Mode」に改名し、Delivery Mode が Original Publisher の初回送信で確定することを明記した (A.1 #1886, #1891, #1914、§2.1.1)。重複 Object の Malformed 条件も "different Delivery Mode than previously observed" に更新された (§12.1 条件 7、§7.1)。実装の用語・公開 API・仕様引用を draft-22 に合わせ、トレーサビリティを回復する。

## 現状

- `src/object_properties.rs` の `ObjectFieldTracker` / `ObjectFieldRecord` は `is_subgroup: bool` で mode (subgroup 経由 / datagram 経由) を表現する。`ObjectFieldTracker` と `observe_object_fields` / `observe_object_fields_with_content` は `pub` の公開 API で、`is_subgroup: bool` を引数に取る。
- 重複 Object の検出は実装済みで、subgroup / datagram で同じ `(group_id, object_id)` を観測して `is_subgroup` が異なる場合に `terminate_malformed_track` で当該購読を終了する (draft-22 §12.1 条件 7 / §7.1 相当)。reason 文字列と doc は "Forwarding Preference" のままである。
- mode は Object 単位で、同一 Track 内で Object ごとに異なる mode を許容する。draft-22 §5.1 のスケジューリングも datagram 優先を前提に mode 混在を許しており、この点は挙動変更不要である。
- `src/session/subscription/validation.rs`、`src/stream/fetch.rs`、`src/session/data.rs` のコメント、`tests/test_session/data_stream.rs` のコメント・テスト名・assert 文字列、`tests/test_object_properties.rs` の assert 文字列が旧用語のままである。
- 公開 API の呼び出し元は `src/session/data.rs` のほか `pbt/tests/prop_object_tracker.rs` と `fuzz/fuzz_targets/fuzz_object_trackers.rs` にもあり、シグネチャ変更に追随が必要である。fuzz は独立 workspace で CI でもビルドされないため、`cargo test --workspace` では壊れを検出できない。

## 設計方針

- `DeliveryMode` enum (`Subgroup` / `Datagram`) を新設し、`ObjectFieldRecord` の `is_subgroup: bool` と `observe_object_fields` / `observe_object_fields_with_content` の引数を置き換える (公開 API の破壊的変更)。
- reason 文字列を "different Delivery Mode" に変更し、テストの assert 文字列も追従する。
- 引用を draft-22 の正しい節へ更新する。用語と確定規則は §2.1.1 (Delivery Mode / Original Publisher が初回送信で確定 / 購読では Delivery Mode に従って送る MUST)、
  重複検出は §7.1 と §12.1 条件 7 (content 比較を根拠づける条件 6 はそのまま維持する)。
  同一 Track 内の mode 混在の許容は §2.1 の "An Original Publisher MAY use both Subgroups and Datagrams within a Group or Track" と、
  §5.1.2 (Scheduling Algorithm) の "If the two objects have different Delivery Modes the datagram is sent first" を根拠にする。
  draft-22 本文の §2.2 と §11.4.1.1 にある "see Section 11.1.2" は Object Properties を指す参照ミスなので Delivery Mode の根拠には使わない。
- 用語と引用の更新は本 issue の範囲に限定し、Delivery Mode に関わらない draft-21 の節番号・引用文面の同期は 0195 が扱う (0195 は本 issue と重複する箇所は本 issue の修正を優先すると明記している)。
- 受信側の Malformed 検出挙動は変えない。送信側で「確定済み mode と異なる送信」を強制する追跡は追加しない (relay 非対応であり、Original Publisher 自身の初回送信が確定行為であるため)。この規約を doc に明記する。
- 公開 API の破壊的変更のため `CHANGES.md` の `## develop` に `[CHANGE]` として記載する。

## 完了条件

- `DeliveryMode` が導入され、`ObjectFieldTracker` の公開 API と内部表現が Delivery Mode ベースになっていること
- 旧用語 (Forwarding Preference) が `src/object_properties.rs`・`src/session/data.rs`・`src/session/subscription/validation.rs`・`src/stream/fetch.rs` の
  コード・doc・reason 文字列・コメントと、`tests/test_object_properties.rs`・`tests/test_session/data_stream.rs`・
  `pbt/tests/prop_object_tracker.rs`・`fuzz/fuzz_targets/fuzz_object_trackers.rs` の呼び出し元・テスト名・assert 文字列から消え、
  draft-22 の用語と節 (§2.1 / §2.1.1 / §5.1.2 / §7.1 / §12.1 条件 6/7) が引用されていること
- 受信側の重複 Object 検出の挙動が変わらないこと (既存テストが通ること)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること

## 解決方法

- `src/object_properties.rs` に `DeliveryMode` enum (`Subgroup` / `Datagram`) を新設し、`ObjectFieldRecord` の `is_subgroup: bool` を `delivery_mode: DeliveryMode` に置き換えた。
  `ObjectFieldTracker::observe_object_fields` / `observe_object_fields_with_content` の引数も `DeliveryMode` に変更した (公開 API の破壊的変更)。
- 重複 Object の Malformed 理由文字列を "malformed track: duplicate Object with different Delivery Mode" に変更した。比較順序と検出挙動は変えていない (bool と 2 variant の全単射置換)。
- `src/session/data.rs` の subgroup 経路は `DeliveryMode::Subgroup`、datagram 経路は `DeliveryMode::Datagram` を渡すようにした。
- 用語と引用を draft-22 に更新した (§2.1 (Objects) / §2.1.1 (Object Fields) / §5.1.2 (Scheduling Algorithm) / §7.1 (Caching Relays) / §12.1 (Malformed Tracks) 条件 6/7)。
  同一 Track 内で Object ごとに Delivery Mode が異なることを許容する根拠も doc に明記し、送信側で確定済み mode と異なる送信を強制しない方針を `DeliveryMode` / `ObjectFieldTracker` の doc に書いた。
- `DeliveryMode::Datagram` のときは `subgroup_id` に `None` を渡す契約を公開メソッドの doc に追記した。
- `pbt/tests/prop_object_tracker.rs` を新 API に追随させ、Delivery Mode のみが異なる入力を生成する分岐とその観測ゲートを追加した (Delivery Mode 比較を消す退行を検出できる)。
  `fuzz/fuzz_targets/fuzz_object_trackers.rs` も bool から `DeliveryMode` への写像で追随させ、fuzz 独立 workspace の rustfmt 非準拠も解消した。
- `tests/test_object_properties.rs` の assert 文字列、`tests/test_session/data_stream.rs` のテスト名・コメント・assert 文字列を新用語に更新した。
- `CHANGES.md` の `## develop` に `[CHANGE]` を追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` / `prek run --all-files` /
  fuzz 独立 workspace の `cargo check`・`cargo clippy`・`cargo fmt --check` が通ることを確認した。
