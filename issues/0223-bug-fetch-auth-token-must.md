# FETCH にも track に紐づく AUTHORIZATION_TOKEN を付与する

- Created: 2026-10-09
- Completed: 2026-10-12
- Branch: feature/fix-fetch-auth-token-must
- Polished: 2026-10-11
- Updated: 2026-10-11

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

- FETCH も `with_auth_parameters` に統一し、track に紐づくトークンを付与する。§11.4.3 の列挙には
  SUBSCRIBE_NAMESPACE も含まれるが、`MoqtClient` に SUBSCRIBE_NAMESPACE の送信経路が無いため
  本 issue の対象外とする。
- C4M の claim による事前判定を残す場合は、「トークンが track に紐づく」条件と、判定できない
  ときの扱いを先に決める。判断できないときは付与する側に倒し、MUST の付与漏れを作らない。
- `auth_message_parameters_for` を使う箇所が無くなる場合は、関数・テスト・doc を整理する。
  残す場合は、どのメッセージで claim 判定を使ってよいかの根拠を doc に書く。
- SETUP へ載せたトークンも免除されないため、SETUP 経路の実装は変えない。

## 完了条件

- クレームを decode できないトークンでも、SUBSCRIBE / FETCH / REQUEST_UPDATE への付与が
  一致することのテスト。
- track に紐づくトークンが FETCH から落ちないことのテスト。FETCH の送信経路は実 I/O を
  要するため、共通ヘルパ `with_auth_parameters` の契約テストで固定する (下記「解決方法」)。
- claim による選別を残す場合、その条件と根拠が doc とテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MoqtClient::fetch` を `with_auth_parameters` に統一し、C4M の `moqt` クレームによる選別を
  廃止した。PUBLISH (`publish_track`) / SUBSCRIBE (`subscribe_track_inner`) / FETCH (`fetch`) /
  REQUEST_UPDATE (`send_request_update`) は、保持しているトークンをすべて
  AUTHORIZATION_TOKEN (0x03) として付与する (draft-ietf-moq-msf-01 §11.4.3 の MUST)。
- 選別のためだけに存在した `auth_message_parameters_for`、クレームを保持していた `AuthToken`
  (claims と `CatToken::decode_moqt_auth_token`)、claim 選別のテスト 3 本を削除した。
  `pub fn auth_message_parameters` と `MoqtClient::auth_parameters` も `with_auth_parameters`
  に一本化し、Token Value → Token 構造 (`c4m_authorization_token`。SETUP と共用) →
  メッセージパラメータ (`authorization_parameter`) の 2 段に整理した。
- client 側で選別しない根拠 (本 client は catalog を解析しないため、送信時点で track と
  トークンの対応を決められない) を `with_auth_parameters` の doc に明記した。あわせて、
  複数トークンを提示したときの relay の評価は一次資料に規定が無く実装依存であり、
  C4M §7.1.1 のトークン検証失敗によるアクション拒否と、§8.9 のメッセージ単位拒否
  (MALFORMED_AUTH_TOKEN) がありうることを書いた。SETUP は `establish_*` の `c4m_tokens`
  引数から `build_setup_options` が option として組み立てる別経路であることも書き分けた。
- テストは 2 本追加した。`with_auth_parameters` の契約 (呼び出し側のパラメータを保持し、
  トークンを入力順にすべて付与し、0 件なら追加しない) と、デコードできる有効な C4M トークン
  であっても FETCH を認可しないトークン (SUBSCRIBE スコープ) を選別せず付与することである。
  どちらも fixture の前提 (C4M として解釈できない / FETCH を認可しない) を assert している。
- README.md / examples/README.md / examples/moq-sub の C4M トークンに関する記述を実装に
  合わせて更新した (付与先の列挙、`MoqtClient` がクレームで選別しないこと、catalog を FETCH
  せず SUBSCRIBE で取得していること)。
- 完了条件 1 は共通ヘルパの契約として固定した (4 経路が同一ヘルパを通ることはコードで確認)。
  完了条件 2 の「FETCH から落ちないことのテスト」は、`MoqtClient` をモック無しに構築するには
  実 I/O が必要で、example には MOQT セッションをサーバー側で駆動する仕組みが無いため、
  実行テストとしては追加していない。example 内に `MoqtClient::fetch` の呼び出し元も無い
  (0224 で削除済み)。in-process のトランスポート試験基盤 (QUIC / WebTransport) はあるため、
  MOQT サーバー側ドライバを用意して往復試験を追加する作業は別途 issue とする。
