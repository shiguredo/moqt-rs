# 音声の冗長化を実リレーの E2E で固定する

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/test-audio-redundancy-e2e
- Polished: {YYYY-MM-DD}

## 目的

音声 Object の冗長化 (moqt-js の移植) は、送信側が同じ Object の datagram を冗長度の数だけ
出し、受信側が同じ識別子の 2 回目を捨てる。単体テストで固定できたのは「送る回数の決定」
(`datagram_copies` の clamp) と「同じ識別子を 2 回 `deliver` したときに 1 回だけ再生する
こと」であり、次の 2 つは固定できていない。

- 送信側が実際に冗長度の数だけワイヤへ出していること (実トランスポートが要る)
- 実リレーを経由した冗長化 datagram が、受信側で重複として捨てられること

モック・スタブを使わない規約のため単体テストでは届かない範囲である。実リレーへ接続できる
環境ができたため、E2E で「冗長度 2 でも同じ音が 2 回鳴らない」ことを固定する。

## 現状

- `e2e-tests/tests/roundtrip.rs` の `publish_and_subscribe_audio_datagram_roundtrip` は音声を
  既定の冗長度 (1) で配信し、受信した音声の PTS を `>=` で検査する。この検査は同じ
  TIMESTAMP の重複を許すため、冗長化した datagram が二重に再生されても検出しない
  (テストのコメントにも明記されている)。
- `examples/moq-pub/src/cli.rs` に `--audio-redundancy` があるが、E2E はこれを使っていない。
- `examples/moq-sub/src/pipeline.rs` の重複の判定は、重複を捨てたときに debug ログ
  (`Skipping a duplicated audio object: ...`) を出す。実リレー越しに冗長化が働いたことは、
  このログの件数で観測できる。

## 設計方針

- `e2e-tests/tests/roundtrip.rs` に、音声だけを `--audio-redundancy 2` で配信する publisher と
  音声を購読する subscriber の往復テストを追加する。
- 検証は「鳴った音」で行う。受信した音声フレームの PTS が厳密に進むこと (同じ PTS の
  フレームが 2 つ届かないこと) を固定する。冗長化した 2 回目が再生へ渡れば、同じ PTS の
  フレームが 2 つ届くため検出できる。
- 継続性 (フレーム数、配信の終了間際までの到達、PTS の幅) と MP4 への保存、graceful shutdown
  は既存の音声の E2E と同じ条件で確かめる。
- 送信側が実際に 2 回送っていることは、同じ実行を debug ログ付きで行い、重複として捨てた
  件数が 1 件以上あることを観測して確かめる (モックを使わずに観測できる唯一の点)。観測した
  事実は解決方法に記録する。
- 接続先は環境変数 `MOQT_E2E_URL` で渡し、ホスト名はリポジトリに残さない。

## 完了条件

- 冗長度 2 の音声往復の E2E テストが追加され、実リレーで成功すること。
- 受信した音声フレームの PTS が厳密に進むことがテストで固定されていること。
- 既定 (冗長度 1) の音声の E2E と映像の E2E が変わらず成功すること。
- 冗長度 2 の実行で、重複として捨てられた音声 Object が 1 件以上観測されること (送信側が
  実際に冗長化していることの確認)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ホスト名をリポジトリに残していないこと。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `e2e-tests/tests/roundtrip.rs` に `publish_and_subscribe_audio_redundancy_roundtrip` を
  追加した。音声だけを `--audio-redundancy 2` で配信する publisher と音声を購読する
  subscriber を起動し、受信した音声フレームの PTS が厳密に進むこと (同じ PTS のフレームが
  届かないこと) を固定する。継続性・MP4 への保存・graceful shutdown は既存の音声の E2E と
  同じ条件で確かめる
- 冗長度を指定して publisher を起動する `start_audio_publisher_with_redundancy` を足し、
  既存の `start_audio_publisher` は既定 (1) のまま使うようにした
- `e2e-tests/README.md` を 3 テストに追随させ、冗長化した音声では PTS が厳密に進むことも
  検証内容として書いた
- 送信側が実際に冗長化していることは、実リレーでこのテストを単独実行し、受信側が重複として
  捨てた Object を観測して確かめた。冗長度 2 では 253 件であり、5 秒間に届く約 250 Object の
  ほぼ全てで 2 回目が届いて捨てられている。冗長度 1 (既定) の音声の E2E を単独実行した
  ときは 1 件であり、これは購読境界の Object が live と replay の両方で届いた分である
- テストの感度も確かめた。重複の判定を無効にする変更 (`is_duplicate` が常に `false` を返す)
  を一時的に入れてこのテストを実行すると、同じ PTS のフレームが届いて 1.20 秒で失敗する
  (`冗長化した 2 回目を鳴らさないこと`)。確認後に変更を戻し、ファイルの SHA-256 が元と
  一致することを確かめた
- 実リレーでの E2E は 3 件とも成功した (7.26 秒)。`cargo test --workspace` (2927 passed /
  0 failed)、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all -- --check` も成功した
- 接続先は環境変数 `MOQT_E2E_URL` のみで渡し、ホスト名はリポジトリに残していない
