# LARGEST_OBJECT に依存せずにメディアへ途中参加できるようにする

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/change-media-join-without-largest-object
- Polished: {YYYY-MM-DD}

## 目的

moq-sub のメディアの途中参加は、明示的な FETCH の range を `SUBSCRIBE_OK` の LARGEST_OBJECT でしか作れない (`plan_fetch_range`)。LARGEST_OBJECT が無い購読では FETCH を発行せず、起動は `NEW_GROUP_REQUEST` か次のキーフレーム待ちに落ちる。仕様が示す別の参加手段と、実測した各手段の寄与を踏まえて参加方法を決め直し、LARGEST_OBJECT に依存しない経路を用意する。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` は `SUBSCRIBE_OK` の Largest Object を終端にした直近 4 Group を要求し、`None` の場合は `Skipping FETCH for the ... track: SUBSCRIBE_OK carried no Largest Object` を残して何もしない。
- メディアの購読には Location Filter を載せていない (映像は `PARAM_NEW_GROUP_REQUEST` のみ)。
- 実測 (relay の cache 無効、publisher を 5 秒先行、映像の録画基準までの時間): FETCH は最初の購読者では発行されず (issue 0279 の原因)、`NEW_GROUP_REQUEST` ありで 23〜48 ms、なしで 1095〜1144 ms であった。録画の先頭は前者が購読の 4 ms 後、後者が 1124 ms 後だった。
- 実測 (relay の cache 有効): 起動を決めるのは relay の replay であり、FETCH の有無と
  `NEW_GROUP_REQUEST` の有無で差が無かった (録画基準までの遅延はローカル relay でいずれも
  1 ms、実 relay でいずれも 20〜26 ms)。映像の FETCH が固有に届ける古い Group (2 Group 分
  114 Object) は全て `VideoDecodeOrder` が stale として落とした。
- 実測 (音声、10 秒のトレース): FETCH で届いた 4 Object は全て購読境界の規則で落ち、再生への寄与は 0 件。relay の replay が運ぶ境界の Object も落ちる。起動への寄与も無かった (FETCH あり 14 ms / なし 14 ms)。録画にも寄与しない。
- 音声の OpusHead は moq-pub が最初の音声 Object にだけ載せるが、relay の replay (最新 Group の先頭 = 最新 1 Object) でも FETCH (直近 4 Object = 80 ms) でも届かない (10 秒の実行で AUDIO_CONFIG を持つ Object は 0 件)。音声 FETCH の残る役割は「トラック開始から 80 ms 以内に購読した場合の OpusHead 取得」だけである。
- 仕様の選択肢 (draft-ietf-moq-transport-22 §3.5 (Joining an Ongoing Track)): 「現在 Group から
  参加」は FILL_PARAMETERS 付き SUBSCRIBE (内側の Location Filter を相対 StartGroup=1 に
  する) で、LARGEST_OBJECT を必要としない。ただし relay は cache だけで覆える fill range しか
  配らず、覆えない範囲を上流へ要求する経路が未実装である。「Next Object で即時参加」
  「Next Group から参加」も LARGEST_OBJECT を必要としない (映像は Group の先頭 =
  キーフレームまで待つ)。
- 一方、明示 FETCH は relay が上流へ 1:1 転送するため、LARGEST_OBJECT さえ分かれば cache 無効でも publisher の保持から取得できる。

## 設計方針

- 参加手段を次の 3 つに整理し、どれを既定にするかを決めて doc に残す。
  - (a) Next Group の Location Filter + `NEW_GROUP_REQUEST`: LARGEST_OBJECT 不要。cache 無効の relay でも動く。Group の先頭 = キーフレームまで待つが、`NEW_GROUP_REQUEST` を併用すると publisher が前倒しする (実測で 1098 ms → 26 ms)。映像の起動に必要な最小の手段。
  - (b) FILL_PARAMETERS 付き SUBSCRIBE (§3.5 の「現在 Group から参加」): LARGEST_OBJECT 不要で、Group の先頭から届く。ただし relay が cache からのみ配るため、cache 無効の配備では使えない。ライブラリは publisher 側の送信 (`send_fill_fetch_response`) と subscriber 側の受信を実装済みで、moq-pub も fill に応答する。
  - (c) 現状の明示 FETCH: LARGEST_OBJECT が必要。publisher が協力しない場合の fallback として意味がある。
- LARGEST_OBJECT が無い場合は (a) にフォールバックする (購読時の Location Filter を切り替えるか、`NEW_GROUP_REQUEST` を併用する)。
- メディア FETCH の扱いを決める。音声は起動・再生・録画・OpusHead のいずれにも寄与しないため削除を第一候補とする。映像は (c) の fallback として残すか、publisher が協力しない場合に限るかを決める。
- 削除する場合も `AudioDeliveryOrder` の購読境界の規則は防御として残す (relay の replay と将来の fill 経路で効く)。

## 完了条件

- 採用する参加手段と、メディア FETCH を残すか削除するかの判断が doc と実装に反映されていること。
- LARGEST_OBJECT が無い購読でも、映像が次のキーフレーム待ちだけに落ちないこと (採用した手段で確認する)。
- 音声の参加と再生の挙動が実測どおりであること (購読境界より前の音声が送出も録画もされないこと)。
- 実 relay の E2E (`publish_and_subscribe_roundtrip` / `publish_and_subscribe_audio_datagram_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
