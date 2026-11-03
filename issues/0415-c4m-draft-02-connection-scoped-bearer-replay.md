# catreplay=1 を接続単位の再利用制限として扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-connection-scoped-bearer-replay
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 §3、§7 の Replay Protection for Bearer Tokens](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/cat.rs` は CLAIM_CAT_REPLAY を定義するが、`CatClaims` で型付きに解釈せず raw に保持する。
`CatClaims::validate` は時刻・issuer・audience を確認し、cti と接続の束縛は扱わない。
proof の jti を扱う `DpopReplayCache` は bearer token の接続束縛とは別物である。

## 設計方針

Sans I/O のまま、呼び出し側から渡す接続識別子と時刻で bearer token の接続束縛を検証できる API と状態を用意する。
catreplay=1 では cti / exp を必須にし、同じ接続上の SETUP と後続 action で再使用を許し、別接続で拒否する。
切断しても exp と許容 leeway の期間は再利用を防ぐ。
catreplay=0 / 省略は replay 保護なし、2 は実装方針を明示する。
ネットワークやリレー・クラスタの実装は追加しない。

## 完了条件

- 同一接続の複数 action は通り、同じ cti の別接続利用は切断後も受理可能期限まで拒否されること。
- cti / exp 欠落、失効時刻と leeway の境界、状態解放を検証すること。
- 署名・scope・通常の時刻検証を省略せず、検証失敗した token が束縛状態を汚染しないこと。
- 接続識別子と cache の所有者・保持期間を API の説明で明確にすること。

## 依存・関連

- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
