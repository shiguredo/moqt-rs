# relay を介した publish / subscribe の往復 E2E テストを用意する

- Created: 2026-10-04
- Completed: 2026-10-04
- Branch: feature/add-e2e-roundtrip-test
- Polished: 2026-10-04

## 目的

relay を介した publish から subscribe までの往復が実環境で成立することを自動で検証したい。現状の E2E は publisher が PUBLISH を受理されて映像を配信するところまでしか見ておらず、subscriber が実際に映像を受け取れるかは検証されていない。また、同じ検証をローカルで再現する手段が無く、変更のたびに手作業で publisher と subscriber を起動して確認している。

## 現状

- `.github/workflows/e2e-test.yml` は `moq-pub` をビルドして起動し、`Starting publish loop` / `PUBDIAG ... video_groups=[1-9]` / SIGINT 後の graceful shutdown を確認する。subscriber を起動しないため、配信が relay を越えて購読側に届くかは分からない
- 接続先は `secrets.TEST_MOQT_URI` で渡す非公開の実環境で、未設定の場合はステップをスキップする (fork からの PR など)。CI のログでは `::add-mask::` で host / authority / fragment をマスクしている
- examples が macOS 専用依存 (Video Toolbox / CoreAudio 系) を持つため、E2E は `macos-15` で実行する
- ローカルで同じ検証を行う手順はリポジトリに無い
- `moq-pub` と `moq-sub` は lib ターゲットを持ち、外部クレートから `cli::Config` と `pipeline::run` を呼べる (lib 化は `issues/closed/0200-refactor-example-libs.md` で完了済み)

## 設計方針

- `e2e-tests/` に E2E テスト用のクレートを新設し、workspace のメンバーにする
- テストは `moq-pub` / `moq-sub` の lib API を使い、publisher と subscriber の pipeline を同一プロセス内で起動する。バイナリを起動してログを照合する方法は取らない (判定がログの文面に依存するため)
- 接続先は環境変数 `MOQT_E2E_URL` で受け取り、トランスポートは `MOQT_E2E_TRANSPORT` (既定 `quic`) で受け取る。`MOQT_E2E_URL` が未設定のときはテストを失敗させ、何が必要かを示すメッセージを出す
- テストには `#[ignore]` を付け、`cargo test -p e2e-tests -- --ignored` を実行したときだけ動かす。通常の `cargo test --workspace` では実行されない
- 検証する内容は次の 5 点とする
  1. publisher の pipeline が SETUP と PUBLISH を完了し、publish ループに入る
  2. subscriber の pipeline がカタログを取得して SUBSCRIBE を完了する
  3. subscriber がデコード済みの映像フレームを継続して受け取る (ログではなくチャネルで受け取るフレームで判定し、5 秒間で 30 フレーム以上かつ配信の終了間際まで届くことを見る。パイプラインが送ったフレーム数を表示側と同じように減らさないと、閾値を超えたとみなされて以降の group のデコードが省かれるため、テスト側で減算する)
  4. subscriber が書き出した MP4 が 0 バイトより大きい
  5. 両方の pipeline が shutdown で正常終了する
- subscriber は publisher の起動後に開始する。カタログが PUBLISH される前に起動すると FETCH が空になり、subscriber が即座に失敗するため
- 再生 (SDL) は行わない (SDL プレイヤーは lib に含まれない)。ただし `--no-play` は指定しない。`no_play: true` はデコード自体を無効にするため映像フレームが得られず、subscriber はデコード有効のまま受け取ったフレームをテスト側で破棄する
- ホスト名はリポジトリに残さない。CI では `::add-mask::` で host / authority / fragment をマスクする
- CI は `.github/workflows/e2e-test.yml` から `MOQT_E2E_URL` と `MOQT_E2E_TRANSPORT` を渡して `cargo test -p e2e-tests -- --ignored` を実行する。`secrets.TEST_MOQT_URI` が未設定の場合は従来どおりスキップする

## 完了条件

- `MOQT_E2E_URL` を設定して `cargo test -p e2e-tests -- --ignored` を実行すると、上記 5 点が検証されて成功すること
- `MOQT_E2E_URL` が未設定の場合は、何が必要かを示すメッセージで失敗すること
- 通常の `cargo test --workspace` では実行されず、既存のテスト結果が変わらないこと
- CI の `E2E Test` が feature ブランチの push で成功し、develop へのマージ後も成功すること
- リポジトリ内に接続先のホスト名が残らないこと
- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`prek run --all-files` が通ること

## 解決方法

- `e2e-tests/` を workspace のメンバーとして追加し、`moq-pub` / `moq-sub` の lib API で publisher と subscriber のパイプラインを同一プロセス内で起動する往復テスト (`tests/roundtrip.rs`) を置いた。
  - `cli::parse_args` でコマンドラインと同じオプションから `Config` を組み立てる
  - publisher は `--fake-capture-device --no-audio`、subscriber は `--no-audio --mp4 <一時ファイル>` で起動する (`--no-play` は指定しない。`no_play` はデコード自体を無効にするため映像フレームが得られず、SDL プレイヤーは lib に含まれないため再生は行われない)
  - subscriber は catalog が PUBLISH される前に起動すると FETCH が空になって終了するため、最初の映像フレームが届くまで理由を残しながら起動し直す (上限 30 秒、試行ごとの待ち 4 秒、間隔 1 秒)
  - Track Namespace はプロセス ID と時刻から実行ごとに一意な値を作る
- 判定はログの文字列ではなく、チャネルで受け取る `DecodedVideoFrame` と MP4 の実体で行う。
  1. publisher のパイプラインが SETUP と PUBLISH を完了する
  2. subscriber のパイプラインが catalog を取得して SUBSCRIBE を完了する (最初のフレームが publisher の生成する 1280x720 でデコードできること)
  3. subscriber がデコード済みの映像フレームを継続して受け取る (5 秒間で 30 フレーム以上、かつ最後の受信が配信の終了間際 (1 秒以内) であること)
  4. subscriber が書き出した MP4 が 0 バイトより大きい
  5. どちらのパイプラインも shutdown で正常終了する (`Result` が `Ok` であること)
- パイプラインはフレームを送るたびに表示待ちフレーム数を増やし、閾値 (`MAX_DISPLAY_BACKLOG`) を超えると以降の group のデコードを省く。表示側が行う減算をテスト側でも行い、継続受信の判定が最初の group だけで成立しないようにした。
- 実 relay が必要なため `#[ignore]` を付け、通常の `cargo test` では実行しない。接続先は `MOQT_E2E_URL`、トランスポートは `MOQT_E2E_TRANSPORT` (既定 `quic`) で受け取り、どちらも空文字は未設定として扱う (CI では secrets / vars が未設定のとき空文字が渡る)。`MOQT_E2E_URL` が未設定の場合は何が必要かを示すメッセージで失敗する。
- 受信した MP4 は一時ディレクトリに置き、テストの終了時 (panic 時も) に削除する。`MOQT_E2E_KEEP=1` のときは残し、場所を出力する。shutdown とタスクの終了には 15 秒の上限を設けた。
- `.github/workflows/e2e-test.yml` を publisher のみの検証からこのテストの実行に置き換えた。`secrets.TEST_MOQT_URI` が未設定の場合は従来どおりスキップし、host / authority / fragment の `::add-mask::` も維持している。テスト側で参照しない `TEST_MOQT_TRANSPORT` のジョブ変数は削除した。
- `CHANGES.md` の `### misc` に `[UPDATE]` を追加し、lib 化は別途 `[ADD]` として記載した。
- 検証は次のとおり。
  - `MOQT_E2E_URL` を設定して `cargo test -p e2e-tests -- --ignored` が成功する (実 relay に対して 6 秒台)
  - `MOQT_E2E_URL` 未設定で `--ignored` を付けると、必要な環境変数を示して失敗する
  - 通常の `cargo test -p e2e-tests` では `ignored` になり、`cargo test --workspace` の結果は変わらない
  - CI の `E2E Test` が feature ブランチの push で成功する (skip ステップはスキップされ、往復検証のステップが実行される)
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`prek run --all-files` が通る
  - リポジトリ内に接続先のホスト名が残っていない
