# LARGEST_OBJECT に依存せずにメディアへ途中参加できるようにする

- Created: 2026-10-15
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-02

## 目的

LARGEST_OBJECT に依存せずにメディアへ途中参加できる経路を確定する。現行実装は映像の `SUBSCRIBE` に
`NEW_GROUP_REQUEST=0` を載せており、LARGEST_OBJECT が無くても新しい Group の開始を要求できる。
残る作業は、仕様が示す参加手段と実測した寄与を踏まえて既定を (a) と確定し、音声の明示 FETCH を
削除し、判断と理由を `crates/moq-sub/README.md` に残すことである。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `fetch_recent_groups` は `SUBSCRIBE_OK` の Largest Object を
  終端にした直近 4 Group を要求し、`None` の場合は
  `Skipping FETCH for the ... track: SUBSCRIBE_OK carried no Largest Object` を残して何もしない。
- 起動時のメディア購読には Location Filter と FILL_PARAMETERS を載せていない。映像は
  `video_subscribe_parameters(None)` が `NEW_GROUP_REQUEST=0` だけを載せ、音声は `subscribe_track` で
  パイプラインのパラメータを載せない (どちらの `SUBSCRIBE` にも transport が既定の
  `SUBSCRIBER_PRIORITY` と C4M の `AUTHORIZATION_TOKEN` を付ける)。GOAWAY 移行の再購読では、
  最後に受信した Location が分かる場合に限り、映像と音声の両方に `LocationFilter::AbsoluteStart` を
  載せる (`video_subscribe_parameters` / `audio_subscribe_parameters`)。この移行経路のフィルタは
  本 issue の変更対象にしない。
- 実測 (relay の cache 無効、publisher を 5 秒先行、映像の録画基準までの時間): FETCH は最初の
  購読者では発行されず (relay の下流 `SUBSCRIBE_OK` が LARGEST_OBJECT を運ばないことが原因と
  考えられるが、0279 の解決方法では切り分け未検証と整理されている)、`NEW_GROUP_REQUEST` ありで
  23〜48 ms、なしで 1095〜1144 ms であった。録画の先頭は前者が購読の 4 ms 後、後者が 1124 ms 後
  だった。
- 実測 (relay の cache 有効): 起動を決めるのは relay の replay であり、FETCH の有無と
  `NEW_GROUP_REQUEST` の有無で差が無かった (録画基準までの遅延はローカル relay でいずれも 1 ms、
  実 relay でいずれも 20〜26 ms)。映像の FETCH が固有に届ける古い Group (2 Group 分 114 Object) は
  全て `VideoDecodeOrder` が stale として落とした。0280 で追加する購読境界も表示からは落とすが、
  録画は境界の判定より前に行うため落とさない。
- 実測 (音声、10 秒のトレース): 起動時の FETCH で届いた 4 Object は全て購読境界の規則で落ち、
  再生への寄与は 0 件だった。起動への寄与も無かった (FETCH あり 14 ms / なし 14 ms)。録画にも
  寄与しない。移行・再発行では境界が「最後に受信した Location」になるため、そのときの FETCH の
  Object は境界を超えうるが、移行先・再発行後の live 購読と重複するだけで、起動・再生を早める
  効果は確認していない。
- 音声の OpusHead は moq-pub が最初に publish した音声 Object にだけ載せる (購読ごとではない)。
  relay の replay (最新 Group の先頭から) でも FETCH (直近 4 Object = 80 ms) でも届かない
  (10 秒の実行で AUDIO_CONFIG を持つ Object は 0 件)。音声 FETCH の残る役割は「トラック開始から
  80 ms 以内に購読した場合の OpusHead 取得」だけである。OpusHead は再生に必須でなく、録画も
  catalog の値で代替できる (`build_audio_entry`)。0330 で購読ごとに Audio Config を載せるように
  なれば、この役割も不要になる。
- 上記の実測値は外部の relay と moq-pub を使った手動計測の記録であり、リポジトリ内のテストでは
  再現しない。
- 仕様の選択肢 (draft-ietf-moq-transport-22):
  - (a) `NEW_GROUP_REQUEST` を使う参加 (§3.5.1 (Dynamically Starting New Groups) /
    §9.20.19 (NEW GROUP REQUEST Parameter)): 購読者は LARGEST_OBJECT を知る必要がない。
    cache 無効の relay でも動く。Next Group の Location Filter
    (`LocationFilter::RelativeGroup { start_group: 0 }`、Type 0x01 Relative Start の
    StartGroup=0) を併用すると、購読は次の Group から始まる。Group の先頭 = キーフレームまで
    待つが、`NEW_GROUP_REQUEST` を併用すると publisher が前倒しする (実測で約 1100 ms →
    23〜48 ms)。映像の起動に必要な最小の手段。
  - (b) FILL_PARAMETERS 付き SUBSCRIBE (§3.5 の「現在 Group から参加」): 外側の Location Filter を
    Next Object (`LocationFilter::NextObject`)、内側の Location Filter を Relative Start の
    StartGroup=1 にする (§9.20.15 (FILL PARAMETERS Parameter))。`NEW_GROUP_REQUEST` は載せない。
    購読者は LARGEST_OBJECT を知る必要がない。ただし、実測に使った relay は cache だけで覆える
    fill range しか配らず、覆えない範囲を上流へ要求する経路が未実装であるため、cache 無効の配備では
    使えない (§3.2.3 (Relay Fetch Handling) の上流 FETCH を実装した relay なら成立しうる)。
    ライブラリの Session は fill fetch stream の開設と FETCH stream の encode / decode を実装済み
    だが、`send_fill_fetch_response` は `crates/tokio-moq` の `MoqtClient` にあり、複数 Object の
    fill range を返す writer は未整備である。moq-pub は catalog 以外の fill を reset し、moq-sub は
    メディア購読に FILL_PARAMETERS を載せず、メディアの fill fetch stream の Request ID を配送先の
    対応表へ登録しないため、届いても `fetch_stream_action` が Ignore として読み捨てる。
    (b) を採用する場合は、これらの実装が必要である。
  - (c) 現状の明示 FETCH (§3.2 (Fetch)): LARGEST_OBJECT が必要。publisher が FETCH に応答する
    場合の補助として意味がある。
- cache 無効の別の実測 (LARGEST_OBJECT が分かる構成) では、明示 FETCH は上流へ 1:1 転送され、
  publisher の保持から取得できた。§3.2.3 は relay が cache と上流 FETCH を組み合わせることを
  許すため、この挙動は relay 依存である。

## 設計方針

- 既定の参加手段は (a) とする。起動時の映像 `SUBSCRIBE` は現行どおり Location Filter と
  FILL_PARAMETERS を載せず `NEW_GROUP_REQUEST=0` だけを載せ、音声はパイプラインのパラメータを
  載せない。Next Group の Location Filter は追加しない (cache 有効時の起動は relay の replay が
  決めており、フィルタを追加すると replay を捨てるため)。`NEW_GROUP_REQUEST` は publisher が
  `DYNAMIC_GROUPS=1` を告知していなくても `SUBSCRIBE` に載せてよい (§9.20.19)。publisher が要求を
  無視する場合は、次のキーフレームまで待つことを許容する。
- (b) は採用しない。理由は、複数 Object の fill range を返す writer が未整備であり、moq-sub の
  メディア fill stream 配送と moq-pub のメディア fill 応答を実装する必要があり、実測に使った
  relay は cache 無効の配備で fill range を配れないことである。将来必要になった場合は別 issue と
  する。
- (c) は映像に限り、LARGEST_OBJECT がある場合の補助として残す (現行どおり)。映像の FETCH は
  表示には寄与しない (0280 で追加する購読境界で落ちる) が、FETCH の Object が live より先に
  届いた場合は録画の基準を早めうる (README の「購読より前の Group の取得 (FETCH)」節と同じ)。
  実測では FETCH が live より先に届く条件を作れておらず、この寄与は未実測である。
- 音声の明示 FETCH は削除する。起動時の実測で起動・再生・録画への寄与が無く、残る OpusHead の
  取得も再生に必須でなく録画は catalog の値で代替できるためである。削除する範囲は、
  `fetch_recent_groups` の音声呼び出し (起動時・`resubscribe_tracks`・`reissue_request`)、
  音声の FETCH 応答経路 (`read_audio_fetch_stream`、`FetchStreamAction::DeliverAudio`)、
  関連テストとする。
- 映像の FETCH を残すため、0272 の映像 FETCH / 0336 / 0375 / 0280 の前提は維持される。0338 の
  統合テスト計画は `resubscribe_tracks` の音声 FETCH を前提にしているため、同計画の該当箇所を
  映像 FETCH に限定する更新を 0338 側で行う。音声 FETCH の削除に伴い、
  `crates/moq-sub/README.md` のメディア FETCH の説明を映像のみに更新する。
  `docs/missing-features.md` の同期は 0323 が扱うため、本 issue では変更しない。
- 削除する場合も `AudioDeliveryOrder` の購読境界の規則は防御として残す (relay の replay と
  将来の fill 経路で効く)。LARGEST_OBJECT が無い場合は境界を設定せず、すべて live として扱う
  (0278 の設計)。
- 変更対象は `crates/moq-sub/src/pipeline.rs` と `crates/moq-sub/README.md` とする。moq-pub と
  `crates/tokio-moq` は変更しない。
- moq-pub は `SUBSCRIBE` の `NEW_GROUP_REQUEST` を受理したときに新しい Group の開始を要求する
  (受理判定は既存テストで固定されている)。

## 完了条件

- 起動時の映像 `SUBSCRIBE` でパイプラインが載せるパラメータが `NEW_GROUP_REQUEST=0` だけで、
  Location Filter と FILL_PARAMETERS を載せないことがユニットテストで固定されていること。
  音声 `SUBSCRIBE` にはパイプラインがパラメータを載せないこと。
- メディアの `SUBSCRIBE` に FILL_PARAMETERS を載せないこと (現行どおり)。
- 音声の FETCH が発行されないこと (起動時・`resubscribe_tracks`・`reissue_request`)。
  音声の FETCH 応答経路 (`read_audio_fetch_stream`、`FetchStreamAction::DeliverAudio`) と
  関連テストが削除されていること。
- 映像の FETCH が LARGEST_OBJECT がある場合だけ発行されること (既存の `plan_fetch_range` の
  テストを維持する)。
- 映像の `SUBSCRIBE` の `NEW_GROUP_REQUEST=0`、moq-pub の受理判定
  (`accepted_new_group_request_checks_the_track_and_the_value`)、`GopTracker` のキーフレーム
  強制 (`a_new_group_request_forces_a_keyframe_after_the_minimum_duration`) の各テストが
  維持されていること。受理後に `gop_tracker.request_new_group()` を呼ぶ配線はコードで確認する。
  受理から新しい Group の開始までの統合経路の確認は 0338 の統合テストに委ねる。起動時間の実測値
  (23〜48 ms / 1095〜1144 ms、録画開始 4 ms / 1124 ms) は外部実測の記録であり、CI では再現
  しないため本 issue の完了条件に含めない。実 relay の E2E
  (`publish_and_subscribe_roundtrip`) が通ること。
- LARGEST_OBJECT がある場合の音声境界の非回帰 (0278) が維持されていること。
- `crates/moq-sub/README.md` に、既定の参加手段 (a)、Next Group の Location Filter を追加
  しない理由、(b) を採用しない理由、映像 FETCH の位置づけ (表示には寄与せず録画の基準を
  早めうる)、音声 FETCH を削除した理由が書かれていること。
- 実 relay の E2E (`publish_and_subscribe_roundtrip` /
  `publish_and_subscribe_audio_datagram_roundtrip`) が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

### 確定した内容

- 既定の参加手段を (a) `NEW_GROUP_REQUEST` に確定した。起動時の映像 `SUBSCRIBE` は
  `NEW_GROUP_REQUEST=0` だけを載せ、Location Filter と FILL_PARAMETERS は載せない。音声の
  `SUBSCRIBE` はパイプラインのパラメータを載せない (現行どおり)。
- 次 Group の Location Filter は追加しない (cache 有効時の起動は relay の replay が決めており、
  フィルタを追加すると replay を捨てるため)。
- FILL_PARAMETERS 付き SUBSCRIBE (`§3.5` の「現在 Group から参加」) は採用しない (複数 Object の
  fill range を返す writer が未整備であり、実測に使った relay は cache 無効の配備で fill range を
  配れないため)。
- 音声の明示 FETCH を削除した。削除したのは起動時 (`run`)、session 移行
  (`resubscribe_tracks`)、request stream 単位の再発行 (`reissue_request`) の 3 箇所の呼び出しと、
  音声の FETCH 応答経路 (`read_audio_fetch_stream`、`FetchStreamAction::DeliverAudio`、
  音声 Object の位置を取る `fetch_object_location`)、および関連テストである。
- FETCH の配送先を `FetchTarget` (Video / Catalog) に型付けし、音声が配送先に現れないことを
  型で保証した。`fetch_recent_groups` は `fetch_recent_video_groups` に改名し、映像だけを
  対象にした。`FETCH_RECENT_GROUPS` と `plan_fetch_range` の doc も映像に限定した。
- 音声の購読境界の規則 (`AudioDeliveryOrder`) は防御として残した。LARGEST_OBJECT が無い場合は
  境界を設定せず、すべて live として扱う (変更なし)。
- `crates/moq-sub/README.md` に、既定の参加手段・Next Group の Location Filter を追加しない理由・
  (b) を採用しない理由・映像 FETCH の位置づけ・音声 FETCH を削除した理由を残した。
- `issues/0338` の統合テスト計画を映像 FETCH に限定し、session 移行と request 再発行の前後で
  音声が欠落しないことを確認する項目を追加した。

### 音声 FETCH 削除の検証 (実 relay での計測)

音声 FETCH の寄与を、削除前 (削除前の develop) と削除後の 2 版で比較した。計測は次のとおり。

- publisher: `moq-pub --fake-capture-device --no-video` (440 Hz 正弦波、Opus 20 ms、datagram 配送)
- subscriber: `moq-sub --mp4 --no-play` で音声を MP4 に録画
- 欠落の判定: 録画した MP4 の音声パケットのタイムスタンプを `ffprobe` で取り出し、40 ms
  (2 フレーム) を超える飛びを欠落とした。20 ms 周期なので 1 フレームの欠落は 40 ms の飛びになる
- session 移行の起こし方: 移行中の publisher は別ノードに接続したまま、subscriber が接続する
  ノードに relay の管理 API で GOAWAY を送らせた (移行先は明示)
- request 再発行の起こし方: relay の管理 API では request 単位の GOAWAY を起こせないため、
  実装の再発行手順 (`STOP_SENDING` で旧 request を閉じ、同じ session で Absolute Start 付きで
  再購読する) を一定時刻に実行する計測用の分岐を一時的に入れて測った (計測後に削除済み)。
  さらに旧購読を閉じてから再購読するまでの隙間を 200 ms / 500 ms に広げても測った

#### 1. 起動時

| 版 | 音声 FETCH | `before_boundary` の累計 | 起動への寄与 |
| --- | --- | --- | --- |
| 削除前 | 直近 4 Group を要求 | 1 → 4 (4 Object を取得) | なし (すべて購読境界以下で破棄) |
| 削除後 | なし | 1 (relay が replay する最新 1 Object のみ) | 変化なし |

`SUBSCRIBE_OK` の Largest Object が購読境界になるため、起動時の FETCH で遡る範囲の Object は
すべて境界以下になる。削除前後で起動時間・再生・録画に差は無かった。

#### 2. session 移行 (GOAWAY)

削除前 3 試行 / 削除後 9 試行。GOAWAY の Timeout 0 / 5000 / 20000 ms、GOAWAY を購読開始の
1 秒後 / 6 秒後に送る条件、音声を datagram と subgroup stream の両方で配送する条件を含む。

| 版 | 試行 | 移行成功 | 音声 FETCH の発行 | 音声パケット | 記録時間 | 移行 ±1.5 秒の欠落 |
| --- | --- | --- | --- | --- | --- | --- |
| 削除前 | 3 | 3/3 | 2 回 (起動時 + 移行時) | 1267〜1347 | 25.3〜26.9 秒 | 0 件 |
| 削除後 | 9 | 9/9 | 0 回 | 1096〜1348 | 21.9〜27.0 秒 | 0 件 |

音声パケット数 × 20 ms が記録時間と一致し、フレームの欠落は無かった。削除前の移行時の FETCH は
4 Object を取得したが、`before_boundary` が 1 → 4 に増えただけで、すべて購読境界以下として
破棄された。観測された 40 ms 超の飛び (削除後の 3 試行で計 3 件) は移行の 7〜20 秒後であり、
datagram のランダムな損失であった。

欠落しない理由は次のとおりである。relay は GOAWAY の Timeout まで購読を維持して配送を続け
(§6.6.1)、切替時の購読境界は「切り替え直前に読み直した最後の受信位置」になる。FETCH の range の
終端は再購読時の `SUBSCRIBE_OK` の Largest Object であり、移行に時間がかかるほど「最後の受信
位置」より前になる。したがって FETCH の Object はすべて境界以下になる。実測でも Largest Object が
…8753、切替時の最後の受信位置が …9022 であった。

#### 3. request stream 単位の再発行

移行検証モードが無効な relay では request 単位の GOAWAY を外部から起こせないため、実装と同じ
手順 (`STOP_SENDING` → 同じ session で Absolute Start 付き再購読) を強制する計測用の分岐で
測った。

| 試行 | STOP から再購読までの遅延 | 音声パケット | 記録時間 | 再発行地点の欠落 | 重複・境界落ち |
| --- | --- | --- | --- | --- | --- |
| 6 | 0 / 200 / 500 ms | 966〜1094 | 19.3〜21.9 秒 | 0 件 | 増加なし |

再発行地点の前後で `before_boundary` / `duplicated` / `stale` の累計が増えず (バックフィルも
重複配送も無い)、音声も途切れなかった。旧購読を閉じてから再購読するまでの 500 ms の隙間を
人工的に空けても欠落しなかったのは、計測に使った relay が `STOP_SENDING` の後も購読を維持して
配送を続けるためである。

#### 結論と限界

- 起動時・session 移行・request stream 単位の再発行の 3 経路すべてで、音声の明示 FETCH は
  起動時間・再生・録画に寄与しなかった。削除は妥当である。
- この結論は、relay が GOAWAY の Timeout まで購読を維持し、`STOP_SENDING` の後も配送を続ける
  という挙動に依存する。仕様は §6.6.1 で購読の維持を定めるが、Timeout より前に購読を止める
  実装では、移行中に publish された音声が欠落しうる。この前提は `crates/moq-sub/README.md` と
  `issues/0338` に注記した。
- 計測は 1 つの relay 実装 (クラスタ構成) による。別の relay 実装での再確認は 0338 の統合テストと
  実 relay の E2E に委ねる。
- 計測に使った環境のホスト名・アドレスはリポジトリに残していない。計測用の分岐は削除済みで、
  最終的な差分には含まれない。

### テストと確認

- 追加・更新:
  - `FetchTarget` (Video / Catalog) を導入し、FETCH の配送先に音声が現れないことを型で保証した。
    `fetch_stream_action_routes_media_fetches` は映像と catalog の振り分けを固定する。
  - `video_subscribe_parameters_request_a_new_group` に、起動時の映像 `SUBSCRIBE` が
    `NEW_GROUP_REQUEST=0` だけを載せ、Location Filter と FILL_PARAMETERS を載せない断言を追加した。
  - `video_subscribe_parameters_use_the_last_location_as_absolute_start` と
    `audio_subscribe_parameters_use_the_last_location_as_absolute_start` に、再購読でも
    FILL_PARAMETERS を載せない断言を追加した。
- 削除: `read_audio_fetch_stream`、`FetchStreamAction::DeliverAudio`、`fetch_object_location` と
  そのテスト (音声 Object の位置が datagram と subgroup stream の 2 経路から取れることは残した)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ることを確認した。
- 実 relay の E2E (`publish_and_subscribe_roundtrip` /
  `publish_and_subscribe_audio_datagram_roundtrip`) は CI の E2E Test ワークフローで確認する。
