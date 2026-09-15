# 削除済み SUBSCRIBE_TRACKS に追従していないコメントと未使用コードを整備する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-follow-subscribe-tracks-removal
- Polished: {YYYY-MM-DD}

## 目的

relay 専用機構の削除 (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS とその応答)
に追従していないコメントと未使用コードが残っている。存在しない関数を参照していたり、
削除済みの経路を現在の挙動の根拠として説明していたりするため、読み手が実装の現状を誤解する。
本 issue はこれらを現行 draft と実装の実態に一致させる。

## 現状

削除自体は完了しているが、次の 3 種の残存物がある。

- 存在しない関数の参照。`send_subscribe_tracks` は `src/session/subscription/send.rs` の
  SUBSCRIBE / PUBLISH の parameter scope 検証コメントに、`handle_peer_subscribe_tracks` は
  `src/session/subscription/validation.rs` の FORWARD / INCLUDE_PROPERTIES 検証コメントに
  残っているが、どちらも既に存在しない。
- 削除済み経路を根拠にする記述。`src/session/subscription/send.rs` と
  `src/session/subscription/recv.rs` の GROUP_ORDER コメントは、PUBLISH に載る GROUP_ORDER を
  「SUBSCRIBE_TRACKS からの伝播 (§9.18.1)」と説明している。draft-ietf-moq-transport-21
  §9.20.9 (GROUP ORDER Parameter) は GROUP_ORDER を SUBSCRIBE / PUBLISH / SUBSCRIBE_TRACKS /
  FETCH に出現可能と列挙しており、PUBLISH への出現は伝播に限らない。
- 未使用コード。`src/session/subscription/validation.rs` の `prefix_overlaps` は本番コードからの
  呼び出しが無く、`src/session/tests.rs` の自身の単体テストからのみ参照されている。doc も
  削除済みの §9.15 (SUBSCRIBE_NAMESPACE) / §9.18 (SUBSCRIBE_TRACKS) の "shares a common
  prefix" 判定を根拠にしている。

## 設計方針

- 存在しない関数を参照する記述から、その参照を外す。
- 「SUBSCRIBE_TRACKS からの伝播」を根拠にしている記述は、draft-ietf-moq-transport-21
  §9.20.9 (GROUP ORDER Parameter) の出現先列挙に置き換える。
- `prefix_overlaps` と `src/session/tests.rs` の対応する単体テストを削除する。
- `REQUEST_PREFIX_OVERLAP` (0x30) は draft 由来の wire 定義であり、削除した機構のエラーコードを
  ライブラリから消す理由がないため `src/error.rs` に残す。
- draft が SUBSCRIBE_TRACKS を定義していること自体を述べる記述 (§9.20.3 の AUTHORIZATION TOKEN
  出現先列挙、§9.20.15 の TRACK PROPERTY FILTER が SUBSCRIBE_TRACKS 専用である旨など) は
  現行 draft と一致しているため変更しない。
- 挙動を変えない。コメントと未使用コードのみを対象とする。

## 完了条件

- `send_subscribe_tracks` / `handle_peer_subscribe_tracks` を参照する記述が残っていないこと
- PUBLISH に載る GROUP_ORDER の根拠が §9.18.1 の伝播ではなく §9.20.9 の出現先列挙になっていること
- `prefix_overlaps` と対応する単体テストが削除されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
- 挙動が変わっていないこと (既存テストがすべて通ること)
- 機能に直接影響しない変更のため `CHANGES.md` の `### misc` に `[UPDATE]` エントリが
  追加されていること
