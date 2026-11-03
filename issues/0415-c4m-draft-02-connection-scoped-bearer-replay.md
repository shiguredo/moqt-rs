# catreplay=1 を接続単位の再利用制限として扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-connection-scoped-bearer-replay
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §3、§7 の Replay Protection for Bearer Tokens](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/cat.rs` は CLAIM_CAT_REPLAY を定義するが、`CatClaims` で型付きに解釈せず raw に保持する。
`CatClaims::validate` は時刻・issuer・audience を確認し、cti と接続の束縛は扱わない。
proof の jti を扱う `DpopReplayCache` は bearer token の接続束縛とは別物である。

## 設計方針

Sans I/O のまま、呼び出し側から渡す接続識別子と時刻で bearer token の接続束縛を検証できる API と状態を用意する。
`CatClaims` に `catreplay` の型付き表現を追加し、decode / encode と raw claim の重複判定を揃える。0 / 1 / 2 以外の値や整数でない値を拒否する。
catreplay=1 では cti / exp を必須にし、同じ接続上の SETUP と後続 action で再使用を許し、別接続で拒否する。
bearer 専用の `BearerReplayCache` は受信者が所有し、その受信者が受理する token 群と全接続で共有する。接続ごとに作り直さず、アプリケーションが発行する opaque な接続識別子を cti に束縛する。接続識別子は cache に記録が残る間は再利用しない。
同じ cache で許容 leeway を固定し、通常の期限検証と保持期限の両方に同じ値を使う。`exp + leeway` が有限であることを確認し、現行の `CatClaims::validate` と同じく参照時刻がこの期限と等しい間は受理可能とする。
切断で束縛を削除せず、参照時刻が保持期限を超えた後にだけ解放する。未失効の記録を追い出して別接続での再利用を許す処理は追加しない。
catreplay=0 / 省略は replay 保護なしで cache の記録を追加しない。2 は本実装では未対応として拒否し、0 と同じ扱いにはしない。
`CatToken` に bearer 用の公開検証入口を追加し、署名検証 → `CatClaims::validate` → 要求 action の scope 認可 → cache の接続照合・登録の順に実行する。全検証が成功したときだけ束縛状態を変更する。`cnf` 付き token はこの入口では拒否し、0418 の DPoP 検証へ渡す。
既存の `CatClaims::validate` は stateless な claims 検証として維持し、単独では接続束縛を保証しないことを API の説明で区別する。
ネットワークやリレー・クラスタの実装は追加しない。

## 完了条件

- 同一接続の複数 action は通り、同じ cti の別接続利用は切断後も受理可能期限まで拒否されること。
- cti / exp 欠落、失効時刻と leeway の境界、状態解放を検証すること。
- 署名・scope・通常の時刻検証を省略せず、検証失敗した token が束縛状態を汚染しないこと。
- 接続識別子と cache の所有者・保持期間を API の説明で明確にすること。
- catreplay=2 と不正な値を拒否し、0 / 省略では cache に記録しないこと。

## 依存・関連

- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
