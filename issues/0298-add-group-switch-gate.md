# 前の Group の stream が開いている間、次の Group の映像 Object を保留する

- Created: 2026-10-17
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-02

## 目的

moqt-js は `src/groupSwitchGate.ts` (`GROUP_SWITCH_HOLD_MS` = 50) で、前の Group の stream が
開いている間、次の Group の映像 Object を保留する。次の Group のキーフレームを先に復号して
しまうと、遅れて届いた前の Group の末尾が `VideoDecodeOrder` に古い Group として捨てられ、
表示されない。GOP の末尾が 1〜数枚欠け、30 fps で 33〜100 ms の停止として見える。

moqt-rs には待ち合わせが無く、同じ欠落が起きる。`src/video_decode_order.rs` の `admit` が
「復号中の Group より古い Group」を `Stale` にし、`crates/moq-sub/src/pipeline.rs` の
`deliver_video_object` がその Object を `frame_tx` へ送らない。前の Group の stream を待つ
箇所は無い (`pending_subgroup_buffer` は Track Alias 未確立の stream 用であり、Group の順序
とは無関係である)。本 issue は、次の Group のキーフレームを復号する前に、前の Group の
stream が終わるのを最大 50 ms 待つ gate をライブラリに追加し、example の live 映像経路へ
配線する。

## 現状

- 発生経路は 2 つある。(a) 前の Group の末尾が損失で再送されている間 (stream の FIN が届か
  ない) に、次の Group のキーフレームが届いて復号が進む。(b) 前の Group の stream task が
  遅れている間に、次の Group のタスクが走る。moq-pub は映像を 1 Group = 1 Subgroup = 1
  unidirectional stream で送る。プロトコル上は 1 Group が複数の Subgroup stream で届き得る
  (draft-ietf-moq-transport-22 §2.2 (Subgroups)) ため、gate は stream 単位で追跡する。
  stream ごとにタスクを作り (`spawn_stream_task`)、同時実行数はデータ stream 全体の
  semaphore (`MAX_CONCURRENT_STREAMS` = 4) で制限する。
- 配信側は前の Group の stream を FIN してから次を開くため窓は狭いが、ゼロではない。
- 各 stream task の読み取りは期限内の待ち合わせを持たず、`receive_registered_stream_data_outcome`
  はチャンクが届くまで無期限に待つ。stream の終端は FIN / RESET_STREAM / STOP_SENDING /
  読み取りエラーで起きる。
- 表示側は `VideoDecodeOrder` が古い Group の Object を復号しないため、前の Group の末尾は
  表示に回らない。`frame_tx` への送出順は Group の順に限定されない (復号は複数の stream task
  で並行し、`VideoDecodeOrder` のロックは判定の間だけ持つ)。表示の最終的な順序は時間軸が
  TIMESTAMP で決める。
- `run` のメインループは 100 ms の `tick_interval` を持つが、stream task の読み取りループには
  期限が無い。

## 設計方針

- 前の Group の末尾の欠落 (1〜3 枚、30 fps で 33〜100 ms の停止) より、Group の切り替えごとの
  最大 50 ms の復号保留を採る。moqt-js と同じ判断である。moq-sub はカタログの `targetLatency`
  (moq-pub の `--target-latency` の既定 200 ms) を時間軸の表示の遅れの下限として使うため、
  targetLatency がある場合は 50 ms の保留はその範囲に収まる。targetLatency が無い場合は
  表示の遅れの下限が 0 になるため、保留はそのまま表示の遅れとして加わるが、上限は 50 ms に
  収まる。FIN の遅延が 50 ms を超える場合は保留で覆えず、末尾の欠落が残る。
- gate はライブラリの新規公開モジュール `src/group_switch_gate.rs` (`pub mod
  group_switch_gate`) に置く。Sans-I/O の状態機械であり、payload は保持しない。Group の
  順序の判定は protocol の状態であり、`VideoDecodeOrder` と同じく複数の subscriber 実装で
  再利用できるためライブラリに置く。`alloc` だけで動作する形にし、時刻は引数
  (`i64` マイクロ秒) で受け取る。
- gate の状態は、配信済みの最大 Group ID、開いている stream の集合、保留している Group ごとの
  期限である。stream は (`group_id`, `stream_id`) の組で追跡する。`stream_id` は example が
  持つ stream ごとの一意な識別子 (`DataStreamId` の値) であり、Subgroup ID の解決
  (`SubgroupIdMode`) に依存しない。1 Group が複数の stream で届いても、それぞれを追跡する。
  API は次のとおりとする。gate は世代番号 (`u64`) を持ち、`reset(generation)` で状態を
  初期化して現在の世代を更新する。`begin_stream` / `end_stream` / `admit` は世代を受け取り、
  現在の世代と異なる呼び出しは無視する。example は `AtomicU64` などで購読の世代を持ち、
  session 移行で世代を進めて `reset` し、新しい stream task には新しい世代を渡す。session
  移行で停止しきれなかった旧 task が reset 後に呼んでも、新しい世代の判定に影響しない
  ようにするためである。
  - `begin_stream(generation, group_id, stream_id)`: live の Subgroup stream を開いている
    ものとして登録する。`handle_stream_body` の映像 arm で、`register_subgroup_header` が
    true を返し、`build_stream_video_decoder` が `Ok(Some(decoder))` を返した後
    (`decode_video_stream` の直前) に呼ぶ。playback 無効で decoder が無い場合 (録画のみ) は
    gate を通さない。
  - `end_stream(generation, group_id, stream_id)`: `decode_video_stream` の呼び出しが戻った
    直後に呼ぶ。`decode_video_stream` 内部の return 経路 (FIN、RESET_STREAM、STOP_SENDING、
    読み取りエラー、`VideoDelivery::Terminated`) はすべてこの 1 箇所で吸収される。
    `decode_video_stream` を呼ぶ前に早期 return する経路 (codec 無し、
    `build_stream_video_decoder` のエラー) では `begin_stream` をまだ呼んでいないため
    `end_stream` は不要である。
  - `admit(generation, group_id, now_us) -> GroupSwitchDecision`: Object ごとに呼ぶ。
    `group_id` が配信済みの最大以下なら `Deliver`。より大きい Group で、配信済みの最大以上
    かつその Group より小さい Group の開いている stream が 1 つでもあれば
    `Hold { release_at_us }` (期限はその Group を最初に保留した時刻 + 50 ms。同じ Group の
    保留中は同じ値を返す)。開いている stream が無い場合と `now_us` が期限以上の場合は
    `Deliver` し、配信済みの最大を更新する。`Deliver` した Group より前の Group の
    開いている stream は以後の保留の判定に使わない (削除する)。Group ID が飛んでいる場合、
    飛んだ Group の stream は存在しないため待たない。飛ぶ前の Group の開いている stream が
    あれば、その終端までは保留する。
  - `reset(generation)`: 状態を初期化し、現在の世代を `generation` にする。
  - `expire` や `hold_deadline_us` は設けない。待つ側は `Hold` が返した `release_at_us` を
    使い、起床後に `admit` を呼び直す。期限の判定は `admit` の 1 箇所に閉じる。
- 保留する Object の所有者は、その Object を読んだ live の stream task である。task は
  payload と Properties をローカルに保持し、解除後も同じ task が同じ decoder で復号する。
  映像の decoder は stream ごとに作られ、1 stream = 1 Group であるため、別 task の decoder で
  復号すると参照フレームが無く映像が壊れる。ライブラリの gate は payload を保持しない。
- 解除の待ち方は 1 つに統一する。`admit` が `Hold` を返した task は、その Object をローカルに
  保持し、解除されるまでその stream から次の読み取りを行わない。待機の登録は `admit` を
  呼ぶ前に行う (`Notify::notified()` を作って `enable()` してから `admit` を呼ぶ)。`admit` が
  `Hold` を返した直後に他の task が `end_stream` を呼んでも通知を取りこぼさないためである
  (`crates/tokio-moq` の `StopSendingSignal::wait` と同じ形)。そのうえで `tokio::select!` で
  共有の `Notify` (他の task が `end_stream` を呼んだとき) と `tokio::time::sleep_until`
  (期限) を待ち、起床ごとに `admit` を呼び直す。`end_stream` は `notify_waiters()` で
  すべての待機を起こす。`Deliver` になったら保持した Object を到着順に復号して配送し、
  そのあと読み取りを再開する。待機中は Mutex を保持しない。stream 自身の FIN は解除後に
  読み取りを再開した時点で観測し、そのときに `end_stream` を呼ぶ。
- begin / end の対応は、`begin_stream` を呼んだ stream task が `decode_video_stream` の戻りで
  `end_stream` を呼ぶ 1 対 1 にする。session 移行では旧 session の stream task を
  `abort_all` でキャンセルし、`video_decode_order.reset()` と同じ箇所で購読の世代を進めて
  gate を `reset(新しい世代)` する (キャンセルされた task は `end_stream` を呼べないため、
  状態は reset で捨てる)。abort は await 点でしか効かず、drain の打ち切り後も task が動き
  得るため、reset 後に旧 task が `begin_stream` を呼ぶ競合は世代番号で無視する。
- 保留中の Object は `last_received_locations` に記録しない。`record_last_location` は
  `admit` が `Deliver` を返した直後、`deliver_video_object` の前で呼ぶ。`deliver_video_object`
  の後ろへ移すと、0280 が記録対象とする「境界以下として送出しなかった Object」が記録から
  漏れる。session 移行で保留 Object が破棄されても、新しい購読の Location Filter がその位置
  から取り直せるようにするためである。
- gate は購読で 1 つ持つ。`video_decode_order` と同じく `Arc<Mutex<...>>` で stream task 間で
  共有し、ロックは判定の間だけ持つ。`Notify` も購読で 1 つ共有する。
- FETCH 応答経路は gate を通さない。FETCH 応答は 1 本の stream に複数 Group を含み得て、
  Group ごとの終端通知が無く、Object は要求時点の Largest Object 以下 (古い Group) である
  ため、保留の対象外とする。live と同じ `deliver_video_object` へ直接流し、古い Group の
  選別は `VideoDecodeOrder` と 0280 の購読境界が行う。FETCH と live の相対順序は保証されない
  ため、FETCH が live の復号中 Group より新しい Group を含む場合の末尾欠落は残る。gate の
  配線は live の Subgroup stream 経路の 1 箇所である。
- 変更対象は次のとおり。
  - `src/group_switch_gate.rs` (新規)、`src/lib.rs`
  - `crates/moq-sub/src/pipeline.rs` (begin / end / admit の配線、共有状態、待機の協調、
    session 移行の reset、`last_received_locations` の記録位置)
  - `crates/moq-sub/README.md` (再生と遅延の節に 50 ms の復号保留を追記)
  - `skills/shiguredo-moqt/SKILL.md` (公開 API 一覧)
  - `tests/test_group_switch_gate.rs`、`pbt/tests/prop_group_switch_gate.rs`
- 0339 は同じ関数の引数整理を予定しており、0338 の完了後に着手する。本 issue は 0339 より
  先に実装してよく、0339 は本 issue で追加した引数を整理対象に含める。

## 完了条件

- `tests/test_group_switch_gate.rs` で次を固定すること。
  - `begin_stream` で登録した前の Group の stream が開いている間は、より新しい Group の
    Object が `Hold` になり、`end_stream` のあとの `admit` が `Deliver` になること
  - 前の Group の stream が先に終わっていれば保留しないこと
  - 期限 (50 ms) を過ぎた `admit` が `Deliver` になること。同じ Group の保留中は期限が
    延びないこと
  - 同じ Group と配信済みより前の Group は保留しないこと
  - 配信済みの最大より前の Group の stream が開いていても、より新しい Group を保留しない
    こと。`Deliver` した Group より前の stream は以後の判定に使わないこと
  - 1 Group に複数の stream がある場合、すべて終わるまで保留すること
  - Group ID が飛んでいる場合、飛んだ Group の stream を待たないこと (飛ぶ前の Group の
    stream が開いていれば、その終端までは保留すること)
  - `reset` で状態が消えること。`reset` 後に旧世代の `begin_stream` / `admit` を呼んでも、
    新しい世代の判定に影響しないこと
- `pbt/tests/prop_group_switch_gate.rs` で、次の不変条件を固定すること。
  - 配信済みの最大以上かつ新しい Group より小さい Group の開いている stream がある間は、
    期限前の `Deliver` が起きないこと
  - 保留の期限が最初の `Hold` の時刻 + 50 ms を超えないこと
  - 同じ Group の保留中に `Hold` の期限が変わらないこと
  - `Deliver` を返した Group 以下の `admit` は `Hold` にならないこと (配信済みの最大が
    単調であることを `admit` の振る舞いで観測する)
- example の配線を、テスト可能な協調の型または関数 (gate の共有状態、`Notify`、待機、
  `begin_stream` / `end_stream` の呼び出し) に切り出し、tokio のユニットテストで次を固定
  すること。
  - 新しい Group の Object が、前の Group の `end_stream` で解除されること
  - 前の Group の `end_stream` が来なければ期限で解除されること
  - 待機の登録が `admit` の前に行われ、`admit` が `Hold` を返した直後に `end_stream` が
    起きても解除されること
  - 複数の task が待っていても、`end_stream` の `notify_waiters` で同時に解除されること
- 保留した Object の配送順と実デコーダでの復号を、実エンコーダで作った VP8 キーフレームを
  使うユニットテストで固定すること (Windows 以外)。stream task 間の結線そのものは、上の
  協調テストと、`decode_video_stream` の戻りで `end_stream` を呼ぶことのコード確認で
  担保する。実 relay の E2E (`publish_and_subscribe_roundtrip`) は非回帰として通すこと。
- `crates/moq-sub/README.md` の再生と遅延の説明に、Group の切り替えで最大 50 ms の復号保留を
  行うことと、FIN の遅延が 50 ms を超える場合は末尾の欠落が残ることを書くこと。
- 公開 API の doc を付け、`skills/shiguredo-moqt/SKILL.md` の公開 API 一覧を更新すること。
- `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` と
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- `make test` / `make clippy` / `make pbt` と `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 調査の記録

- moqt-js の実装: `src/groupSwitchGate.ts` (gate 本体)、`src/createMediaSubscriber.ts`
  (`videoGroupGate` の配線)。保留の長さは `groupSwitchGate.ts` の定数に、理由 (再生の遅延の
  範囲に収まること) が書かれている。
- moqt-rs で同じ害が出る根拠: `src/video_decode_order.rs` の `admit` の `Stale` 判定と、
  `crates/moq-sub/src/pipeline.rs` の `deliver_video_object` の送出条件。
- 似ているが別の issue: 0280 は購読の境界 (`SUBSCRIBE_OK` の Largest Object) 以前を表示
  しない話であり、本 issue の Group の切り替えの保留とは別である。0339 は pipeline の分割と
  引数整理であり、本 issue の挙動変更とは別である。

## pending にした理由

(以下は pending にした時点の理由である。現在は reopened 済みで、下の reopened 理由で判断を
確定している。)

前の Group の末尾の欠落 (33〜100 ms の停止) と、Group の切り替えごとの最大 50 ms の復号保留
のどちらを取るかは、映像の見え方と表示の遅延の交換であり、実装だけでは決められない。moqt-js
は保留を選んでいるが、moqt-rs の example は `targetLatency` (既定 200 ms) と時間軸で表示
する構成であり、同じ判断がそのまま当てはまるかを確かめたうえで決める必要がある。判断が付く
まで実装に着手しないため `issues/pending/` に置く。

## reopened にした理由

pending にした理由だった「前の Group の末尾の欠落を取るか、Group の切り替えごとの最大 50 ms の
復号保留を取るか」の判断を確定できる材料が揃ったため、実装対象に戻す。moq-sub はカタログの
`targetLatency` (moq-pub の `--target-latency` の既定 200 ms) を時間軸の表示の遅れの下限として
使い、`crates/moq-sub/src/main.rs` の `timeline.set_target_latency_ms` で反映している。50 ms の
保留はこの 200 ms の範囲に収まるため、moqt-js の `GROUP_SWITCH_HOLD_MS` と同じ保留を採用する。
採用の判断と根拠は本文の設計方針に反映した。

## 解決方法

### 実装した内容

- ライブラリに `src/group_switch_gate.rs` (`pub mod group_switch_gate`) を追加した。sans-I/O の
  状態機械 [`GroupSwitchGate`] で、配信済みの最大 Group、開いている stream の集合
  (`(group_id, stream_id)`)、保留している Group ごとの期限を持つ。`alloc` だけで動作し、
  時刻は `i64` マイクロ秒で受け取る。
  - `begin_stream` — live の Subgroup stream を開いているものとして登録する
  - `end_stream` — 終端したものとして登録を外す
  - `admit` — Object を配送してよいかを判定する。配信済みの最大 Group 以上で、判定する
    Group より小さい Group の stream が開いていれば `Hold { release_at_us }` を返し、期限は
    最初に保留した時刻 + `GROUP_SWITCH_HOLD_US` (50 ms) にする。同じ Group の保留中は期限を
    延ばさない。`Deliver` を返したら、その Group より前の stream は以後の判定に使わない
  - `reset(generation)` — 状態を初期化し、世代を更新する
- 購読の世代 (`u64`) で旧い呼び出しを無視する。session 移行で stream task を止めきれない
  場合があるため、`GroupSwitchHandle` (example 側) が stream task の生成時に世代を固定し、
  `begin_stream` / `end_stream` / `admit` へ渡す。`reset` のあとに旧世代の task が呼んでも
  新しい購読の判定を変えない。
- example の `crates/moq-sub/src/group_switch.rs` に、stream task 間で共有する
  `SharedGroupSwitchGate` (状態機械 + `Notify` + 世代) と `GroupSwitchHandle` を追加した。
  保留の解除は `end_stream` の通知か期限のどちらか早い方で起き、待機の登録 (`Notified::enable`)
  を `admit` より前に行って取りこぼしを防ぐ。時計は単調な `tokio::time::Instant` に統一した。
- `crates/moq-sub/src/pipeline.rs` に配線した。
  - `handle_stream_body` の映像 arm で、decoder を作った後 (再生する場合だけ) に
    `begin_stream`、`decode_video_stream` の戻りで `end_stream` を呼ぶ (全 return 経路を
    1 箇所で吸収する)
  - `decode_video_stream` は Object ごとに `wait_until_video_deliverable` を通し、保留中は
    payload を持ったまま待ち、解除後に同じ decoder で復号する。FETCH 応答経路は gate を
    通さない
  - 保留した Object は「最後に受信した位置」に記録しない (解除後に記録する)。移行で保留を
    捨てたときに、新しい購読の Location Filter がその位置から取り直せるようにするためである
  - session 移行の `video_decode_order.reset()` と同じ箇所で `group_switch.reset()` を呼ぶ
- `crates/moq-sub/README.md` の再生と遅延の節に、Group の切り替えで最大 50 ms の復号保留を
  行うこと、FIN の遅延が 50 ms を超える場合は末尾の欠落が残ることを書いた。
  `skills/shiguredo-moqt/SKILL.md` のモジュール一覧に `group_switch_gate` を追加した。

### 判断

- 前の Group の末尾の欠落 (30 fps で 1〜3 枚、33〜100 ms の停止) を避けるため、Group の
  切り替えごとに最大 50 ms の復号保留を許容する (moqt-js の `GROUP_SWITCH_HOLD_MS` と
  同じ判断)。moq-sub は catalog の `targetLatency` (既定 200 ms) を表示の遅れの下限に
  するため、50 ms の保留はその範囲に収まる。FIN の遅延が 50 ms を超える場合は保留で覆えず、
  末尾の欠落が残る。
- 保留した Object の所有者は、その Object を読んだ live の stream task にする。decoder は
  stream ごとに作られ、1 stream = 1 Group であるため、別の task の decoder で復号すると
  参照フレームが無い。ライブラリの gate は payload を保持しない。

### テスト

- `tests/test_group_switch_gate.rs` — 完了条件の 8 項目を固定した。前の Group の stream が
  開いている間の保留と `end_stream` による解除、先に終わっていれば保留しないこと、期限
  (50 ms) と期限の非延長、同じ Group と配信済みより前の Group の非保留、配信済みより前の
  Group の stream を以後の判定に使わないこと、1 Group に複数の stream がある場合、Group ID が
  飛んでいる場合、`reset` による状態の初期化と世代分離 (旧世代の `begin_stream` /
  `end_stream` / `admit` が新しい世代の判定を変えないこと)。
- `pbt/tests/prop_group_switch_gate.rs` — stream の開閉・世代の更新・配送の観測から独立に
  期待値を数え、4 つの不変条件 (期限前の `Deliver` が無いこと、期限が最初の Hold + 50 ms を
  超えないこと、同じ Group の期限が変わらないこと、配信済みの最大以下の `admit` が `Hold` に
  ならないこと) を固定した。判定の分岐ごとの到達数も確かめる。
- `crates/moq-sub/src/group_switch.rs` のユニットテスト — 前の Group の `end_stream` による
  解除、期限による解除、`admit` 直後の `end_stream` を取りこぼさないこと、複数の待機が 1 回の
  通知で解除されること、`reset` が旧世代の待機を解除すること、`reset` 後に旧世代の
  `begin_stream` / `end_stream` が新しい世代の判定へ影響しないこと。
- `crates/moq-sub/src/pipeline.rs` の `group_switch_tests` — 実 VP8 エンコーダで作った
  キーフレームと inter frame を使い、解除後にキーフレーム → inter frame の順で `frame_tx` へ
  1 枚ずつ送出されること、保留した Object を解除まで「最後に受信した位置」に記録しないこと。
- テストの検出力は変異テストで確認した。世代チェックを削除した実装、`GroupSwitchHandle` が
  常に世代 0 を返す実装、保留の解除を待たずに記録する実装のいずれも、テストが失敗する。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `cargo test -p pbt` / `cargo build -p shiguredo_moqt --target
  thumbv7em-none-eabihf` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt
  -p moq-sub` が通ることを確認した。
- 実 relay での E2E (`publish_and_subscribe_roundtrip` / 音声 2 種) を実行し、3 件とも通る
  ことを確認した。映像の再生経路 (gate の配線を含む) はこの E2E で通る。

### 既知の限界

- 前の Group の stream が `begin_stream` で登録される前に次の Group の Object が `admit`
  されると、その Group が配信済みの最大になり、待つ対象が無いため保留しない。stream の到着
  順が逆転した場合と、semaphore / `pending_subgroup_buffer` / decoder 構築の待ちで
  `begin_stream` が遅れた場合に起きうる。library の doc に記載した。
- FIN の遅延が 50 ms を超える場合は保留で覆えず、前の Group の末尾の欠落が残る。
- `docs/moqt.md` のモジュール表には `group_switch_gate` を追加していない。同表には
  `video_decode_order` / `pending_subgroup_buffer` / `audio_clock` も未掲載であり、表の
  整備は本 issue の範囲外とした (公開 API の一覧は `skills/shiguredo-moqt/SKILL.md` を更新済み)。
