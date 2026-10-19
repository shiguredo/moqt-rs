# FetchStreamEncoder::encode_object がエラー時に出力バッファへ部分書き込みしないようにする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-encoder-partial-write
- Polished: 2026-10-20

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
  その後の `entry.encode` や Priority 検証が失敗すると、`prior_state` は元の group のまま
  `subgroup_priorities` だけが変化する。一時テストで、Ascending のエンコーダを使い、
  group 5 の Object を成功させた後に group 6 の Object を properties 不正で失敗させ、
  続けて group 5 の同一 Subgroup で Priority を変更した Object を渡すと `Ok(())` になることを確認した。
- `SubgroupObject::encode` / `FetchStreamObject::encode` は書き込み前に全検証を行い、失敗時に `buf` を変化させないことをテストで固定している。呼び出し側の `buf` を受け取るストリーム層の encode 系では `FetchStreamEncoder::encode_object` だけが規約から外れている。
- 影響: `buf` を再利用する利用者で、エラー後に壊れた Object が送信され得る。また、エラー後に `subgroup_priorities` が欠落すると、本来拒否すべき Priority 変更を受理し得る。

## 設計方針

- Priority 一貫性の検証 (既存記録との比較) を `entry.encode` より前に移動する。難しければローカルの一時バッファへ encode し、成功時だけ `buf` へ追記する。
- `subgroup_priorities` への insert と `retain`、`prior_state` / `prior_context` の更新は、`entry.encode` と Priority 検証を含むすべての失敗し得る処理が成功した後に行う。エラー時には `buf` と `subgroup_priorities` のどちらも変化させない。
- 失敗時に `buf` と `subgroup_priorities` が不変であることをテストで固定する。`subgroup_priorities` は private のため、挙動で観測する。

## 完了条件

- `encode_object` のすべてのエラー経路で `buf` が変化しないこと。
- `encode_object` のすべてのエラー経路で `subgroup_priorities` が変化しないこと。具体的には、group 変更の encode が失敗した後、元の group の同一 Subgroup で Priority を変更した Object が `ProtocolViolation` で拒否されること。
- 上記を `tests/test_stream/encoder.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
