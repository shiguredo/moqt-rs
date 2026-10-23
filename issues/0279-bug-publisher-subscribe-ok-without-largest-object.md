# publisher 役 subscription を forget した後も Track 最大を LARGEST_OBJECT に載せる

- Created: 2026-10-15
- Completed: 2026-10-23
- Branch: feature/fix-largest-object-in-subscribe-ok
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been
published on this Track the Publisher MUST include this parameter." と定める。§3.1.4 (Largest
Object) の Largest Object は Track 単位の値であるため、自側が publisher 役として Object を
送信した実績は、その publisher 役 subscription を forget した後も Track の最大として残る
必要がある。しかし現行実装は subscription が保持する値だけから算出するため、publisher 役
subscription をすべて forget すると、以降の SUBSCRIBE_OK に LARGEST_OBJECT が載らない。
本 issue は自側の publisher 役送信のみを対象とし、relay の下流 SUBSCRIBE_OK は対象外とする
(CODEBASE.md の「MoQ リレーには対応しない」に従う。relay の §7.4 / §9.20.17 の MUST は relay
の責務であり、relay アプリは公開 API `MessageParameters::set_largest_object` で上流値を明示
できる)。

## 再現手順

公開 API の `Session` ペア (テスト用の in-process 構成) で再現できる。

1. client が SUBSCRIBE を送り、server (publisher) が SUBSCRIBE_OK で確立する
2. server が subgroup stream で Location (3, 7) の Object を送信する
3. server が `send_data_stream_closed` で送信 stream を終端し、`send_publish_done` で
   PUBLISH_DONE を送る
4. server が `forget_subscription(rid)` で publisher 役 subscription を破棄する
5. client が同じ Track へ新しい SUBSCRIBE を送り、server が SUBSCRIBE_OK を返す

現行コードでは 5 の SUBSCRIBE_OK の `largest_object()` が `None` になる。Object を publish
済みであるため §9.20.17 の MUST に反する。既存テスト
`tests/test_session/subscription/largest_object_track_aggregate.rs` の
`subscribe_ok_uses_track_wide_largest_object` は subscription を保持したままの 2 本目を固定して
おり、forget 後は対象外である。

## 現状

- `src/session/subscription/delivery.rs` の `publisher_track_largest` は
  `aliases.subscriptions_by_track` (キーは Track Namespace + Track Name + `TrackRole`) を引き、
  現在保持している publisher 役 subscription の `effective_largest_object` の最大だけを返す。
  subscription を forget すると索引が除去されるため `None` を返す。
- `src/session/data.rs` の `send_subgroup_object` / `send_object_datagram` は、送信時に
  `Subscription::record_largest_received_location` で当該 subscription の最大を更新するが、
  Track 単位では保持しない。
- `issues/0304-draft-22-track-status-largest-object.md` は「publisher 役 subscription を
  すべて `forget_subscription` した後は自動補完できない。Track 単位で largest を永続保持する
  改善は本 issue のスコープ外とする」と既知の制約として明記している。本 issue でその制約を
  解消する。
- `examples/moq-pub` は `send_subscribe_ok` に `MessageParameters::new()` を渡している。
  ライブラリ側の補完で MUST を満たすため、example 側の変更は不要とする。
- 元の実測 (publisher / relay / moq-sub の 3 者構成で 1 人目の購読だけ FETCH が出ない) は、
  relay の下流 SUBSCRIBE_OK の挙動を指しており、本ライブラリの変更では直らない。relay は
  §7.4 / §9.20.17 の MUST を自ら満たす必要があり、本 issue の対象外である (実測の切り分けは
  未検証。relay 側の issue 候補として報告する)。

## 設計方針

- `Session` に Track 単位の publish 済み最大 Location を保持する
  (`HashMap<(TrackNamespace, Vec<u8>), Location>`。キーに `TrackRole` は含めない)。値は
  自側が publisher 役として送信した Object の Location の最大だけを記録する (subscriber 役で
  受信した Object は含めない)。
- 更新点は `send_subgroup_object` と `send_object_datagram` の
  `record_largest_received_location` を呼ぶ 2 箇所とし、同じ Location を Track 単位の最大へ
  も反映する。比較は `Location` の (group_id, object_id) の辞書順最大
  (`Subscription::record_largest_received_location` と同じ) とする。
- 保持期間はセッション終了までとする。`forget_subscription` / PUBLISH_DONE / request stream
  の終端では削除しない。エントリ数はセッション中に publish した Track 数に比例する
  (アプリが publish しない限り増えない)。`Session` の drop で全解放される。
- `publisher_track_largest` は、現行の subscription 群からの最大と保持した Track 最大の
  大きい方を返す。これにより SUBSCRIBE_OK だけでなく、REQUEST_UPDATE_OK /
  PUBLISH_STATE_NOTIFY / FETCH の range 解決 / fill range / 相対フィルタ解決の全呼び出し
  経路で同じ Track 最大が使われる。
- アプリが明示した LARGEST_OBJECT は従来どおり `update_largest_object_in_parameters` の max
  合流で扱い、観測値より小さければ観測値まで引き上げ、大きければそのまま載せる
  (アプリ指定値を上書き・削除しない)。
- `publisher_track_largest` の doc (「forget 済みの Track では `None` を返す」) を保持の
  仕様に合わせて更新する。`issues/0304` の「Track 単位の永続保持はスコープ外」という記述も、
  本 issue が保持を導入する旨に更新する。
- relay の上流受信 Object の集約は行わない。relay 対応が必要なら CODEBASE.md の変更を含む
  別 issue とする。
- テストは `tests/test_session/subscription/largest_object_track_aggregate.rs` に追加する。
  `#[ignore]` は付けず、モック / スタブは使わない (Session ペアの実 API で検証する)。

## 完了条件

- 再現手順 1〜5 で、forget 後の SUBSCRIBE_OK の `largest_object()` が `Some((3, 7))` に
  なること (実装前は `None` で失敗するテストで固定する)。
- forget 後も Track 最大が単調に更新されること。forget 後に新しい publisher 役 subscription
  で (4, 2) を送信した後、別の SUBSCRIBE_OK が `Some((4, 2))` になること。
- Object を 1 つも publish していない Track への SUBSCRIBE_OK は、ライブラリが
  LARGEST_OBJECT を自動補完しないこと (`None`)。アプリが明示した LARGEST_OBJECT は、その
  Track で publish していなくてもそのまま載ること。
- アプリが明示した LARGEST_OBJECT が観測値より小さい場合は観測値まで引き上げられ、大きい
  場合はそのまま載ること。
- 既存の `subscribe_ok_uses_track_wide_largest_object` / `request_update_ok_uses_track_wide_largest_object` /
  `publish_state_notify_uses_track_wide_largest_object` (subscription 保持中の 2 本目以降) が
  変わらないこと。
- `publisher_track_largest` を呼ぶ他の経路 (FETCH の range 解決 / fill range / 相対フィルタ)
  が、既存テストを含めて退行しないこと。
- `publisher_track_largest` の doc、`issues/0304` のスコープ外記述、`docs/moqt.md` の
  LARGEST_OBJECT の節が実装と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

publisher 役 subscription をすべて forget した後も §9.20.17 (LARGEST OBJECT Parameter) の MUST を
満たすようにした。

- `Session` に `published_track_largest` (Track Namespace + Track Name → Location) を追加し、
  `send_subgroup_object` と `send_object_datagram` で自側が publisher 役として送信した Object の
  Location の最大を記録した。subscriber 役で受信した Object は含めない。比較は `Location` の
  (group_id, object_id) の辞書順最大であり、保持はセッション終了までで、エントリ数はセッション中に
  publish した Track 数に比例する。
- `publisher_track_largest` は live な publisher 役 subscription 群の最大値と保持値の大きい方を
  返すようにした。これにより SUBSCRIBE_OK / PUBLISH / 購読の REQUEST_UPDATE_OK /
  PUBLISH_STATE_NOTIFY / FETCH の range 解決 / fill range / 相対フィルタ解決の全経路で同じ
  Track 最大を使う。
- アプリが明示した LARGEST_OBJECT は従来どおり `update_largest_object_in_parameters` の max 合流で
  扱い、観測値より小さければ観測値まで引き上げ、大きければそのまま載せる (アプリ指定値を
  上書き・削除しない)。
- relay の上流受信 Object の集約は行わない。CODEBASE.md の「クライアントとサーバーのみで MoQ
  リレーには対応しない」に従い、relay の下流 SUBSCRIBE_OK は relay の責務とする。元の実測
  (relay 経由の 1 人目の購読で FETCH が出ない) は relay の下流 SUBSCRIBE_OK の挙動を指しており、
  本 issue の対象外である。
- doc を実装に合わせて更新した。対象は `Session::publisher_track_largest` /
  `record_published_track_largest` の doc、`docs/moqt.md` の Message Parameters の Largest Object、
  `issues/0304-draft-22-track-status-largest-object.md` の「Track 単位の永続保持はスコープ外」
  という記述である。
- `tests/test_session/subscription/largest_object_track_aggregate.rs` にテストを追加した。
  subgroup で publish した後と datagram で publish した後に publisher 役 subscription を forget
  しても SUBSCRIBE_OK に保持値が載ること、より古い Location を送っても最大が変わらないこと、
  forget 後に新しい subscription で publish した値で単調に更新されること、未 publish では自動
  補完せずアプリ指定値はそのまま載ること、アプリ指定値が観測値より小さい場合は引き上げることを
  固定した。subgroup 経路と datagram 経路の記録を無効化する変異で対応するテストが失敗することも
  確認した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る。
