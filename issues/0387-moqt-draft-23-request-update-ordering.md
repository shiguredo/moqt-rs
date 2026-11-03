# REQUEST_UPDATE の処理順序と fill の非 coalescing を保証する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-request-update-ordering
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §6.4.2.2、§9.5、§9.19.15、§9.19.20](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/subscription/recv.rs` の `handle_update_for_subscription` は `Subscription::pending_update_params` へ後続値を merge してイベントを出す。
FILL_PARAMETERS 自体は sticky state から除去するが、fill を含む更新でも前の累積値と coalesce する。
`src/session/core.rs` は主に未応答更新の個数を管理する。

## 設計方針

request stream ごとに受信順の更新を保持し、処理と REQUEST_UPDATE_OK の返却順を保証する。
FILL_PARAMETERS を含む更新は coalescing の境界にし、更新時点のフィルタ・PAUSED で個別に処理する。
通常更新の coalescing は許容しつつ、成功ごとの OK と失敗時の単一 ERROR、クレジット回復を整合させる。
片方向の FIN は取消と区別し、request を維持する経路では未応答の更新とクレジットを保持して必要な応答を順に返す。cancel または request 完了時に残るキューとクレジットを解放する。
TRACK_NAMESPACE_PREFIX も非 coalescing だが、未対応 namespace 機能の新規実装はしない。

## 完了条件

- pause → fill → resume や異なる Location Filter の連続更新で、中間の fill が後続値へ置き換わらないこと。
- 成功応答の順番と個数が受信順に一致すること。request を維持する片方向 FIN の後も未応答更新への必須応答を返し、cancel または request 完了時にはキューとクレジットが残らないこと。
- `src/session/subscription/dispatch.rs` / `send.rs` と fetch の更新応答も含めて検証すること。

## 依存・関連

- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)
- 関連: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)
