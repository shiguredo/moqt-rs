# FETCH にも track に紐づく AUTHORIZATION_TOKEN を付与する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-auth-token-must
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §11.4.3 (Presenting Authorization) は「When a token is associated with a
track, it MUST be included in ALL control messages that accept the AUTHORIZATION TOKEN parameter
and are associated with that track. For end subscribers, this includes SUBSCRIBE,
SUBSCRIBE_NAMESPACE, FETCH, and REQUEST_UPDATE messages. This requirement applies regardless of
whether the token was also provided in SETUP.」と MUST を定める。現状は FETCH だけがトークンを
選別しており、付与漏れが起きる。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::fetch` は
  `auth_message_parameters_for` を使い、C4M の `moqt` クレームが FETCH と Full Track Name を
  認可すると判定したトークンだけを AUTHORIZATION_TOKEN (0x03) として付ける。クレームを
  decode できないトークンは FETCH に付かない。
- 同じファイルの `subscribe_track_inner` (SUBSCRIBE) と `send_request_update`
  (REQUEST_UPDATE) は `with_auth_parameters` を使い、保持するトークンをすべて付ける。
- `with_auth_parameters` の doc は「どのトークンが track に紐づくかは catalog の authInfo と
  視聴側の URI で決まるため、SUBSCRIBE / PUBLISH と同じく保持しているトークンをすべて載せる。
  アクションごとの絞り込みは relay 側の認可判断であり、絞り込むと MUST の付与漏れになりうる」
  と書いており、同じファイル内で FETCH だけ判断が異なっている。
- そのため、クレームを decode できないトークンや FETCH を認可しないと判定されたトークンは、
  SUBSCRIBE には載るのに FETCH には載らない。relay が FETCH でもトークンを要求する場合に
  認可されない。

## 設計方針

- FETCH も `with_auth_parameters` に統一し、track に紐づくトークンを付与する。
- C4M の claim による事前判定を残す場合は、「トークンが track に紐づく」条件と、判定できない
  ときの扱いを先に決める。判断できないときは付与する側に倒し、MUST の付与漏れを作らない。
- `auth_message_parameters_for` を使う箇所が無くなる場合は、関数・テスト・doc を整理する。
  残す場合は、どのメッセージで claim 判定を使ってよいかの根拠を doc に書く。
- SETUP へ載せたトークンも免除されないため、SETUP 経路の実装は変えない。

## 完了条件

- クレームを decode できないトークンでも、SUBSCRIBE / FETCH / REQUEST_UPDATE への付与が
  一致することのテスト。
- track に紐づくトークンが FETCH から落ちないことのテスト。
- claim による選別を残す場合、その条件と根拠が doc とテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
