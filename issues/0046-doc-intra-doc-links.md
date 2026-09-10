# 未解決の intra-doc link を修正する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-intra-doc-links

## 目的

`cargo doc` を警告なしで通せるようにする。現在は未解決の intra-doc link が 21 件あり、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が失敗する。

## 現状

`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` の結果、次のリンクが解決できない。

- `src/session.rs`: `[Session]` / `[SessionEvent]` / `[SessionEvent::SendControl]` / `[SessionEvent::CloseSession]` / `[TrackRole]` / `[Role]` 等
- `src/stream.rs`: `[SubgroupHeader]` / `[SubgroupHeader::encode]` / `[FetchHeader::encode]`

`session` モジュールは型を re-export しない方針のため、モジュール doc 内の短いリンクが解決できない (SKILL.md に記載)。

## 設計方針

`[`crate::session::core::Session`]` のようにフルパス化する。再発防止として rustdoc 検査を CI に追加するのは別 issue とする。

## 完了条件

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が成功すること
- doc の意味が変わっていないこと
