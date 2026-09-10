# 未解決の intra-doc link を修正する

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/doc-intra-doc-links
- Polished: 2026-09-10

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

## 解決方法

未解決の intra-doc link をフルパス化して解消した（doc のみの変更で挙動は変えない）。

- `src/session.rs` のモジュール doc の `Session` / `Role` / `TrackRole` / `SessionEvent` と各メソッドのリンクをフルパス化した。
- `src/stream.rs` の `SubgroupHeader` / `FetchHeader` のリンクをフルパス化した。
- 非公開 item に残っていた未解決 link (`TrackDataAcceptance::Discarded` / `Session::register_discarded_stream` / `Session::retain_discarded_stream_id`) と `src/msf.rs` の引用 `[JSON]` も修正した。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` と `--document-private-items` の両方が成功することを確認した。
- `CHANGES.md` の `### misc` に `[UPDATE]` エントリを追加した。
