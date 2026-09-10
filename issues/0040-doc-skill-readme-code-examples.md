# SKILL / README のコード例と API 一覧を実装に合わせる

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-skill-readme-code-examples

## 目的

`skills/shiguredo-moqt/SKILL.md` と `README.md` のコード例・API 一覧を実装と一致させ、エージェントや利用者がコンパイルできないコードを生成しないようにする。

## 現状

- `skills/shiguredo-moqt/SKILL.md` は「各ヘッダは `fn encode(&self) -> Vec<u8>` / `fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError>` を持つ」と一般化するが、`SubgroupObject` / `FetchStreamEntry` / `ObjectDatagram` / `FetchStreamObject` の実シグネチャはこれと異なる。`FetchStreamObject` には `decode` が無く
  `decode_after_flags` 経由。
- `skills/shiguredo-moqt/SKILL.md` の `subgroup_tracker` API 一覧は `open` / `mark_fin` / `mark_reset` / `mark_stop_sending` / `get` のみで、`remove_track_alias` / `record_priority` / `check_object_after_fin` / `SubgroupStreamState::can_reopen` が漏れている。
- `README.md` と SKILL のコード例は `?` をトップレベルで使い、そのままコピーするとコンパイルできない。`src/lib.rs` に `include_str!` が無く doctest も走らない。

## 設計方針

SKILL の一般化をやめ、型ごとに実シグネチャを列挙する。API 一覧を実装から再生成する。コード例は `no_run` フェンス + `fn main() -> Result<...>` に直すか、README を doctest 対象に含める。doctest 化の CI 追加は別 issue とする。

## 完了条件

- SKILL のシグネチャ・API 一覧が `src/` と一致すること
- README / SKILL のコード例がそのままコピーしてコンパイルできること
- 可能ならコード例が doctest で検証されること
