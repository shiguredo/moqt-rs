# FetchStreamEncoder::encode_object がエラー時に出力バッファへ部分書き込みしないようにする

- Created: 2026-10-19
- Completed: 2026-11-10
- Branch: develop
- Polished: 2026-11-10

## 目的

呼び出し側の出力バッファを受け取るストリーム層の encode 系は「検証に失敗した場合、呼び出し側の出力バッファに部分バイトを残さない」契約で揃っている。`FetchStreamEncoder::encode_object` だけが Publisher Priority の検証前にエントリ全体を書き込むため、エラー時に不正なバイト列がバッファに残り、呼び出し側がバッファを再利用すると壊れた wire を作る。あわせて、エラー時に `subgroup_priorities` を変化させないようにし、失敗後の再試行で同一 Subgroup の Priority 変更を見逃さないようにする。

## 現状

- `src/stream/encoder.rs` の `encode_object` は
  `entry.encode(properties_data, self.prior_context, buf)?;` で `buf` に書き込んだ後、
  `if let Some(previous_priority) = self.subgroup_priorities.get(&key)` で
  同一 Subgroup 内の Publisher Priority 一貫性 (§12.1 (Malformed Tracks) 条件 1) を検証し、
  `return Err(MessageError::ProtocolViolation("publisher priority must not change within the same subgroup in a FETCH response"))`
  を返す。
- Priority 検証の失敗時は `prior_state` / `prior_context` を更新しないが、`buf` には書き込んだバイト列が残る。
- `subgroup_priorities` は group 変更時に
  `self.subgroup_priorities` の `.retain(|&(g, _), _| g >= input.group_id);`
  (descending では `.retain(|&(g, _), _| g <= input.group_id);`) で古い group の記録を先に破棄する。
  その後の `entry.encode` (`FetchStreamObject::encode` の properties 検証) や group ID delta のアンダーフロー検査が失敗すると、
  `prior_state` は元の group のまま `subgroup_priorities` だけが変化する。
  Priority 検証が失敗するのは同一 group の場合だけであり、このとき `retain` は走らない。
  group ID delta のアンダーフローで消える記録は prior group より後方 (Descending では前方) の group だけで、以後の入力では参照されないため、挙動では観測できない。一時テストで、Ascending のエンコーダを使い、
  group 5 の Object を成功させた後に group 6 の Object を properties 不正で失敗させ、
  続けて group 5 の同一 Subgroup で Priority を変更した Object を渡すと `Ok(())` になることを確認した。
- `SubgroupObject::encode` / `FetchStreamObject::encode` は書き込み前に全検証を行い、失敗時に `buf` を変化させないことをテストで固定している。呼び出し側の `buf` を受け取るストリーム層の encode 系では `FetchStreamEncoder::encode_object` だけが規約から外れている。
- 影響: `buf` を再利用する利用者で、エラー後に壊れた Object が送信され得る。また、エラー後に `subgroup_priorities` が欠落すると、本来拒否すべき Priority 変更を受理し得る。

## 設計方針

- Priority 一貫性の検証 (既存記録との比較) を `entry.encode` より前に移動する。検証が参照するのは `input.group_id`、Subgroup ID、`input.publisher_priority` と `subgroup_priorities` だけで、`entry.encode` の結果には依存しない。
  `FetchStreamObject::encode` は書き込み前に全検証を行うため、並べ替えだけで全経路で `buf` は不変になり、一時バッファは要らない。
- `subgroup_priorities` への insert と `retain`、`prior_state` / `prior_context` の更新は、`entry.encode` と Priority 検証を含むすべての失敗し得る処理が成功した後に行う。エラー時には `buf` と `subgroup_priorities` のどちらも変化させない。
- 失敗時に `buf` と `subgroup_priorities` が不変であることをテストで固定する。`subgroup_priorities` は private のため、挙動で観測する。

## 完了条件

- `encode_object` のすべてのエラー経路で `buf` が変化しないこと。
- `encode_object` のすべてのエラー経路で `subgroup_priorities` が変化しないこと。挙動で観測できる経路は次のテストで固定し、観測できない group ID delta アンダーフローの経路は `retain` を全成功後へ移す並べ替えで担保する。
  - Ascending: group 5 の Object を成功させ、group 6 の Object を `FetchStreamObject::encode` の properties 検証 (例: `has_properties: true` で `properties_data` が `None`) で失敗させた後、group 5 の同一 Subgroup で Priority を変更し Object ID を 1 手目より大きくした Object が拒否されること。
  - Descending: 同じ手順を group 5 → 4 (失敗) → 5 で行うこと (6 を使うと group ID delta のアンダーフローで先に拒否され、修正前でも通ってしまう)。
  - エラーは `ProtocolViolation` の variant だけでなく、メッセージ `publisher priority must not change within the same subgroup in a FETCH response` で照合すること (Object ID の非増加など別理由の拒否で通らないようにする)。
- Priority 検証の失敗時に `buf` が変化しないことをテストで固定すること (現行では失敗した Object のエントリ全体が残る)。
- 上記を `tests/test_stream/encoder.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

`src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` を、失敗し得る処理をすべて終えてから状態を更新する構成に並べ替えた。

- Publisher Priority の一貫性検証 (§12.1 条件 1) を `entry.encode` による `buf` への書き込みより前に移した。検証は入力と既存の記録だけを参照するため、一時バッファは使っていない。
- group の前進時の `subgroup_priorities` の `retain` (過去 group の記録の破棄) を、group ID delta の計算や `entry.encode` より後へ移した。前進するかどうかは match の前に `group_advances` として求める。
- `subgroup_priorities` への insert、`prior_context` / `prior_state` の更新は、`entry.encode` が成功した後にまとめて行う。書き込みの後に失敗し得る処理は無い。
- `encode_object` の doc に、失敗時は `buf` にも内部状態にも何も残さない契約を書いた。

テストは `tests/test_stream/encoder.rs` に次の 2 件を追加した。どちらも修正前の実装では失敗し、修正後に通る。

- `test_priority_change_rejection_leaves_buf_unchanged`: 同一 Subgroup の Priority 変更をメッセージで照合して拒否し、`buf` が変化しないこと、失敗後も同じ Priority の Object を続けてエンコードできること。
- `test_failed_group_change_keeps_priority_record`: Ascending (5 → 6 で失敗 → 5) と Descending (5 → 4 で失敗 → 5) で、group を前進させる Object を properties 不正で失敗させた後も、元の group の同一 Subgroup で Priority を変えた Object が拒否され、`buf` が変化しないこと。

group ID delta のアンダーフローで失敗する経路は記録の変化を挙動で観測できないため、`retain` を全成功後へ移す並べ替えで担保した。
`CODEBASE.md` の規定に従い、ブランチと PR を作らず develop に直接コミットし、`CHANGES.md` は更新していない。
