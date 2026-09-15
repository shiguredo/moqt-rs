# PUBLISH に LARGEST_OBJECT を付与する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-largest-object
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) は LARGEST_OBJECT を PUBLISH に
出現できるパラメータとして列挙し、Track に Object が publish 済みなら Publisher は必ず
含めなければならない (MUST) としている。`send_publish` は LARGEST_OBJECT を補完しないため、
既に Object を publish した Track を PUBLISH で再告知する経路で MUST 違反になる。

## 現状

- `src/session/subscription/send.rs` の `send_publish` は引数の `parameters` をそのまま送信し、
  `update_largest_object_in_parameters` を呼ばない。同関数の呼び出しは `send_subscribe_ok`、
  `send_publish_state_notify`、`src/session/subscription/dispatch.rs` の `send_ok_for_subscription` の
  3 経路だけである。
- `src/message.rs` の `PUBLISH_ALLOWED_PARAMS` は `PARAM_LARGEST_OBJECT` を含むため、
  アプリが明示すれば載せられる。補完が無いためアプリ任せになっている。
- 同一 Track の publisher 役購読を横断した観測最大値は `src/session/subscription/fill.rs` の
  `publisher_track_largest` で取得できる。
- 新規 Track に対する最初の PUBLISH ではまだ Object を publish していないため付与の必要はない。
  問題は、自側が既に Object を publish した Track に対して PUBLISH を送る経路である。
- open issue 0078 は応答 3 経路の LARGEST_OBJECT の算出元を `publisher_track_largest` に揃えるもので、
  PUBLISH 経路の欠落は扱っていない。

根拠 (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter)):

> It MAY appear in SUBSCRIBE_OK, PUBLISH, REQUEST_UPDATE_OK, TRACK_STATUS_OK, or PUBLISH_STATE_NOTIFY.

> If Objects have been published on this Track the Publisher MUST include this parameter.

再現手順:

1. 同一 Track の publisher 役購読で Object を publish する
2. 同じ Track に対して `send_publish` を呼ぶ
3. 送信される PUBLISH に LARGEST_OBJECT が付かない

## 設計方針

- `send_publish` で `publisher_track_largest` を呼び、`Some` のとき
  `update_largest_object_in_parameters` で `parameters` を補完する。`None` のときは付与しない。
- `update_largest_object_in_parameters` の「アプリ指定値との max を取る」挙動に合わせ、
  アプリが明示した LARGEST_OBJECT を上書きしない。
- 補完は既存の `parameters.validate_scope(PUBLISH_ALLOWED_PARAMS)` の後、`request_id` の発行前に置き、
  エラー時に request ID の欠番を作らない。補完した `parameters` は送信イベントと状態登録の
  両方で使う。
- 0078 と同じ `publisher_track_largest` を使うが対象経路が異なるため、本 issue では応答 3 経路の
  算出元を変更しない。
- 既に Object を publish した Track を再告知する経路と、観測値が無い Track で付与されない経路の
  回帰テストを追加する。

## 完了条件

- 既に Object を publish した Track への PUBLISH に LARGEST_OBJECT が付くこと
- その Track に Object の観測値が無い場合は LARGEST_OBJECT が付かないこと
- アプリが明示した LARGEST_OBJECT が上書きされないこと
- 回帰テストが `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること
