# 音声の冗長化を実リレーの E2E で固定する

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
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
