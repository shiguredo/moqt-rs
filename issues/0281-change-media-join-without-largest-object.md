# LARGEST_OBJECT に依存せずにメディアへ途中参加できるようにする

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/change-media-join-without-largest-object
- Polished: 2026-10-25

## 目的

moq-sub のメディアの途中参加は、明示的な FETCH の range を `SUBSCRIBE_OK` の
LARGEST_OBJECT からしか作れない (`plan_fetch_range`)。LARGEST_OBJECT が無い購読では
FETCH を発行せず、映像の起動は `NEW_GROUP_REQUEST` か次のキーフレーム待ちに落ちる。
現行実装は映像の SUBSCRIBE に `NEW_GROUP_REQUEST` を載せており、LARGEST_OBJECT が
無くても新しい Group の開始は要求できる。仕様が示す別の参加手段と、実測した各手段の
寄与を踏まえて、既定の参加手段とメディア FETCH の要否を決め、LARGEST_OBJECT に
依存しない経路を明確にする。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` は `SUBSCRIBE_OK` の Largest Object を終端にした直近 4 Group を要求し、`None` の場合は `Skipping FETCH for the ... track: SUBSCRIBE_OK carried no Largest Object` を残して何もしない。
- 起動時のメディア購読には Location Filter を載せていない (映像は `PARAM_NEW_GROUP_REQUEST` のみ、音声はパラメータ無し)。GOAWAY 移行の再購読では、映像と音声の両方に `LocationFilter::AbsoluteStart` を載せる (`video_subscribe_parameters` / `audio_subscribe_parameters`)。この移行経路のフィルタは本 issue の変更対象にしない。
- 実測 (relay の cache 無効、publisher を 5 秒先行、映像の録画基準までの時間): FETCH は最初の購読者では発行されず (relay の下流 SUBSCRIBE_OK が LARGEST_OBJECT を運ばないためであり、0279 の解決方法で relay の責務と整理済み)、`NEW_GROUP_REQUEST` ありで 23〜48 ms、なしで 1095〜1144 ms であった。録画の先頭は前者が購読の 4 ms 後、後者が 1124 ms 後だった。
- 実測 (relay の cache 有効): 起動を決めるのは relay の replay であり、FETCH の有無と `NEW_GROUP_REQUEST` の有無で差が無かった (録画基準までの遅延はローカル relay でいずれも 1 ms、実 relay でいずれも 20〜26 ms)。映像の FETCH が固有に届ける古い Group (2 Group 分 114 Object) は全て `VideoDecodeOrder` が stale として落とした (映像は 0280 で購読境界の選別を追加する予定であり、FETCH の古い Group はその境界でも落ちる)。
- 実測 (音声、10 秒のトレース): FETCH で届いた 4 Object は全て購読境界の規則で落ち、再生への寄与は 0 件。relay の replay が運ぶ境界の Object も落ちる。起動への寄与も無かった (FETCH あり 14 ms / なし 14 ms)。録画にも寄与しない。
- 音声の OpusHead は moq-pub が最初に publish した音声 Object にだけ載せる
  (購読ごとではない)。relay の replay (最新 Group の先頭から) でも
  FETCH (直近 4 Object = 80 ms) でも届かない (10 秒の実行で AUDIO_CONFIG を持つ
  Object は 0 件)。音声 FETCH の残る役割は「トラック開始から 80 ms 以内に購読した
  場合の OpusHead 取得」だけである。OpusHead は再生に必須でなく、録画も catalog の
  値で代替できる (`build_audio_entry`)。0330 で購読ごとに Audio Config を載せる
  ようになれば、この役割も不要になる。
- 仕様の選択肢 (draft-ietf-moq-transport-22 §3.5 (Joining an Ongoing Track)):
  - (a) `NEW_GROUP_REQUEST` を使う参加: 購読者は LARGEST_OBJECT を知る必要がない。
    cache 無効の relay でも動く。Next Group の Location Filter
    (`LocationFilter::RelativeGroup { start_group: 0 }`、Type 0x01 Relative Start の
    StartGroup=0) を併用すると、購読は次の Group から始まる。Group の先頭 =
    キーフレームまで待つが、`NEW_GROUP_REQUEST` を併用すると publisher が前倒しする
    (実測で約 1100 ms → 23〜48 ms)。映像の起動に必要な最小の手段。
  - (b) FILL_PARAMETERS 付き SUBSCRIBE (§3.5 の「現在 Group から参加」): 外側の
    Location Filter を Next Object (`LocationFilter::NextObject`)、内側の Location
    Filter を Relative Start の StartGroup=1 にする。`NEW_GROUP_REQUEST` は載せない
    (現在 Group から参加するため)。購読者は LARGEST_OBJECT を知る必要がない。
    ただし、実測に使った relay は cache だけで覆える fill range しか配らず、覆えない
    範囲を上流へ要求する経路が未実装であるため、cache 無効の配備では使えない。
    ライブラリの Session は fill fetch stream の開設と FETCH stream の encode /
    decode を実装済みだが、`send_fill_fetch_response` は `examples/tokio-moq` の
    `MoqtClient` にあり、複数 Object の fill range を返す writer は未整備である。
    moq-pub は catalog 以外の fill を reset し、moq-sub もメディアの fill fetch
    stream を配送経路へ流していない (`fetch_stream_action` が Ignore)。(b) を採用する
    場合は、これらの実装が必要である。
  - (c) 現状の明示 FETCH: LARGEST_OBJECT が必要。publisher が FETCH に応答する場合の fallback として意味がある。
- cache 無効の実測では、明示 FETCH は上流へ 1:1 転送され、LARGEST_OBJECT さえ分かれば publisher の保持から取得できた。仕様 (§3.2.3) は relay が cache と上流 FETCH を組み合わせることを許すため、この挙動は relay 依存である。

## 設計方針

- 参加手段を (a)(b)(c) に整理し、どれを既定にするかを決めて doc に残す。既定は「起動時の購読で LARGEST_OBJECT を必要としない手段」とし、(c) は LARGEST_OBJECT がある場合の補助に限るか、削除するかを決める。
- SUBSCRIBE の送信時点では `SUBSCRIBE_OK` の LARGEST_OBJECT は分からないため、LARGEST_OBJECT の有無で購読の Location Filter を切り替えることはできない。切り替えるとすれば、SUBSCRIBE 後に REQUEST_UPDATE でフィルタを変更するか、常に (a) か (b) のどちらかを選ぶことになる。
- 既定を (a) にする場合は、現行どおり `NEW_GROUP_REQUEST` を載せ (Next Group の Location Filter を追加するかは次項で決める)、LARGEST_OBJECT がある場合だけ FETCH を追加する。既定を (b) にする場合は、`NEW_GROUP_REQUEST` を載せず、外側 Next Object + 内側 StartGroup=1 の FILL_PARAMETERS を載せる。既定を (a) にする場合、LARGEST_OBJECT が無い場合に (c) の FETCH を発行しない、というのが現行のフォールバックである。
- (a) で Next Group の Location Filter を追加するかは、relay の replay を捨てるトレードオフを踏まえて決める。cache 有効時の起動は replay に依存するという実測があるため、フィルタを追加しない現行の unfiltered 購読 + `NEW_GROUP_REQUEST` を維持する選択肢も残す。Next Group の Location Filter を追加するかどうかを doc に明記する。
- (b) を採用する場合は、moq-pub のメディア fill 応答 (複数 Object の fill range を返す writer の実装を含む) と moq-sub のメディア fill stream 配送を実装する。採用しない場合は、現行どおりメディアの fill fetch を使わない。
- メディア FETCH の扱いを決める。音声 FETCH は起動・再生・録画には寄与せず、OpusHead の取得だけが残る役割である。OpusHead は再生に必須でなく録画も catalog の値で代替できるため、削除を第一候補とする。残す場合は、その理由 (OpusHead の検証) を doc に明記する。映像 FETCH は、LARGEST_OBJECT がある場合の補助として残すか、実測で古い Group が表示に寄与しなかったことを踏まえて削除するかを決める。
- 削除する場合も `AudioDeliveryOrder` の購読境界の規則は防御として残す (relay の replay と将来の fill 経路で効く)。LARGEST_OBJECT が無い場合は境界を設定せず、すべて live として扱う (0278 の設計)。
- 変更対象は `examples/moq-sub/src/pipeline.rs` と `examples/moq-sub/README.md` を主とする。(b) を採用する場合は `examples/moq-pub/src/pipeline.rs` と `examples/tokio-moq/src/moqt_client.rs` も対象になる。`docs/missing-features.md` の同期は 0323 が扱うため、本 issue では変更しない。

## 完了条件

- 採用する参加手段と、メディア FETCH を残すか削除するかの判断が `examples/moq-sub/README.md` と実装に反映されていること。
- 採用した手段で、LARGEST_OBJECT が無い購読でも映像が次のキーフレーム待ちだけに落ちないこと。
  - (a) を採用する場合: `SUBSCRIBE` に `NEW_GROUP_REQUEST` が載り、実測と同じ条件で `NEW_GROUP_REQUEST` ありの起動が次のキーフレーム待ち (約 1.1 秒) より早いことを確認する。publisher が `NEW_GROUP_REQUEST` を無視する場合は、次のキーフレームまで待つことを許容する。
  - (b) を採用する場合: relay の cache が現在 Group の先頭を覆うとき、fill range が現在 Group の先頭から届き、映像のキーフレームから復号を始められることを確認する。publisher にまだ Object が無い場合は、次のキーフレームまで待つことを許容する。
- LARGEST_OBJECT がある場合、購読境界より前の音声が送出も録画もされないこと (0278 の非回帰)。LARGEST_OBJECT が無い場合は境界を設定せず、すべて live として送出されること。
- (b) を採用する場合、moq-pub のメディア fill 応答と moq-sub のメディア fill stream 配送が動くこと。採用しない場合は、メディアの SUBSCRIBE に FILL_PARAMETERS を載せないこと。
- 実 relay の E2E (`publish_and_subscribe_roundtrip` / `publish_and_subscribe_audio_datagram_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
