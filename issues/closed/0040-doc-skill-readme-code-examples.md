# SKILL / README のコード例と API 一覧を実装に合わせる

- Created: 2026-09-10
- Completed: 2026-09-17
- Polished: 2026-09-17
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

## 解決方法

### SKILL.md のシグネチャ・API 一覧

- 「各ヘッダは `fn encode(&self) -> Vec<u8>` / `fn decode(...)` を持つ」という一般化をやめ、
  型ごとの実シグネチャを列挙した (SubgroupHeader / FetchHeader / SubgroupObject /
  FetchStreamEntry / FetchStreamObject / ObjectDatagram)。`SubgroupObject::decode` が
  `(Self, Option<Vec<u8>>, usize)` を返すこと、`FetchStreamObject` に公開の `decode` が
  無く `FetchStreamEntry::decode` 経由であることも明記した
- `subgroup_tracker` の API 一覧に `new` / `remove_track_alias` / `record_priority` /
  `check_object_after_fin` / `SubgroupStreamState::can_reopen` を追加し、引数名と戻り値を
  実装に合わせた (`record_priority` は `subgroup_id` を取り `Result<(), SessionError>`、
  `check_object_after_fin` は `&self` で `Option<&'static str>` を返す)
- 一覧の再生成として、SKILL.md に現れる関数名 142 個がすべて `src/` に存在することを
  確認した。セッション API の `recv_*` は引数・戻り値を実装と突き合わせた

### コード例

- README / SKILL のコード例のうち `?` を使う 5 箇所 (README 4 / SKILL 3、重複を除く) を
  `rust,no_run` フェンス + `fn main() -> Result<(), _>` に直した。`no_run` は
  コンパイルのみ行われるため、コピーして動く例であることを検証できる
- エラー型は no_std のため `std::error::Error` を実装しない。各例が実際に返す
  `MessageError` / `SessionError` を使うようにした
- `src/lib.rs` に `#[cfg(doctest)] #[doc = include_str!("../README.md")] struct ReadmeDoctests;`
  を追加し、README のコード例を doctest でコンパイルするようにした (実行はしない)。
  `pub` を付けないため公開 API ドキュメントには現れない
- SKILL.md の `rust,no_run` の 3 例は一時クレートから `cargo check` して
  コンパイルできることを確認した

### 検証

- `cargo test --doc -p shiguredo_moqt` が 5 件通る (README の 4 例 + 既存 1 件)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通る
