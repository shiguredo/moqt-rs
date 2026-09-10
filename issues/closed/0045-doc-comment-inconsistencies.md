# doc コメントの実装との不整合を修正する

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/doc-comment-inconsistencies
- Polished: 2026-09-10

## 目的

実装と食い違う doc コメントを修正し、読者とエージェントの誤解を防ぐ。doc のみの変更とし、挙動は変えない。

## 現状

1. `src/session/core.rs` のモジュール doc は、`send_subgroup_header` / `recv_subgroup_header` / `recv_object_datagram` 等の data stream / datagram 関数を `Session` の責務として列挙するが、実体は `src/session/data.rs` にある。分離先の兄弟モジュール一覧（`subscription.rs` / `fetch.rs` / `namespace.rs` / `goaway.rs`）に `data.rs` が無い。
2. `src/kvp.rs` は「トレイト・マクロ・クロージャ・汎用中間表現は導入しない (CODEBASE.md 規約)」と引用するが、`CODEBASE.md` に該当記述は無い。`shiguredo-rust` スキルが禁じているのは「トレイトを作らないこと」「マクロを作らないこと」のみで、クロージャ・汎用中間表現の禁止は存在しない。
3. `src/session/subscription/validation.rs` の `validate_group_order` doc は 1 行目と 2 行目が同義で重複している。
4. `src/msf.rs` の `MsfCatalogDocument::decode` 付近のコメントは「deltaUpdate が配列以外なら Full として扱う」と書くが、実コードは配列以外を `InvalidCatalog("deltaUpdate MUST be an array")` で拒否する。
5. `src/session/namespace/publish_namespace.rs` の `send_err_for_namespace_publication`、`src/session/namespace/subscribe_namespace.rs` の `send_err_for_namespace_subscription`、`src/session/namespace/track_subscription.rs` の `send_err_for_track_subscription` のコメントは「Session が close
意図を通知する SessionEvent variant は未実装」と書くが、`SessionEvent::ResetRequestStream` は fetch 経路 (`src/session/fetch.rs`) で実装済みで、`SessionEvent::StopSendingRequestStream` も定義済みである。
6. `src/message.rs` の `REQUEST_OK_ALLOWED_PARAMS` のコメントは「全応答 context の許可パラメータの和集合」と説明するが、context 別集合（`PUBLISH_OK_ALLOWED_PARAMS` / `REQUEST_UPDATE_OK_ALLOWED_PARAMS` / `TRACK_STATUS_OK_ALLOWED_PARAMS` / `NAMESPACE_OK_ALLOWED_PARAMS`）の和集合は `{EXPIRES, LARGEST_OBJECT}` の 2 種であり、定数の
12 種より狭い。実際は「REQUEST_OK で出現しうるパラメータを意図的に広く列挙したもの」である。

## 設計方針

- 1 は data 系関数の記載を `data.rs` の責務として書き分け、兄弟モジュール一覧に `data.rs` を加える。`Session` の責務列挙から data 系を外す方向で整理し、`src/session.rs` のサブモジュール構成の記述と整合させる。
- 2 は「トレイト・マクロは作らない (`shiguredo-rust` 規約)」と、クロージャ・汎用中間表現を「本モジュールの設計判断」として規約引用から切り離す。4 項目を一律に `shiguredo-rust` へ紐付けない。
- 3 は 1 行目に統合する。
- 4 は「deltaUpdate が存在しない場合のみ Full、存在して配列以外なら `InvalidCatalog`」に修正する。
- 5 は各関数のコメントを「`SessionEvent::ResetRequestStream` は fetch 経路で実装済み。本経路では Session は close を通知せず、bidi request stream の close は app が担う」に合わせる。
- 6 は「全応答 context の許可パラメータの和集合」という語を「REQUEST_OK で出現しうるパラメータを意図的に広く列挙したもの（context 別集合の和集合ではない）」に直す。codec 層がワイヤ妥当性のみを検証し context 別検証は session 層が担う旨は既存記述を維持する。

## 完了条件

- 上記 6 箇所の doc コメントが実装と一致していること
- doc コメントのみの変更であり、既存の挙動とテストに影響しないこと

## 解決方法

コード内 doc コメントを実装に合わせて修正した（doc のみの変更で挙動は変えない）。

- `src/session/core.rs` のモジュール doc から data 系関数を切り離し、`data.rs` の分離文に一本化した。兄弟モジュール一覧は制御メッセージ用の 4 モジュールのみにした。
- `src/kvp.rs` の実在しない規約引用 (CODEBASE.md / shiguredo-rust) を外し、本モジュールの設計判断として記述した。
- `src/session/subscription/validation.rs` の `validate_group_order` doc の重複行を統合した。
- `src/msf.rs` の `MsfCatalogDocument::decode` 付近の deltaUpdate コメントを実装に合わせた。
- namespace 3 ファイルの「SessionEvent variant は未実装」コメントを、`SessionEvent::ResetRequestStream` は fetch 経路で実装済み・本経路では Session は close を通知しない旨に修正した。
- `src/message.rs` の `REQUEST_OK_ALLOWED_PARAMS` の説明を、REQUEST_OK に出現しうるのは `EXPIRES` / `LARGEST_OBJECT` のみで、codec 層は context 別検証をセッション層に委ねて意図的に広く受理する旨に修正した。
- `CHANGES.md` の `### misc` に `[UPDATE]` エントリを追加した。
