# E2E テスト

実 relay を介した publish / subscribe の往復を検証する E2E テストです。

`moq-pub` と `moq-sub` の lib API を使い、publisher と subscriber のパイプラインを同一プロセス内で起動します。バイナリを起動してログを照合する方法は取らず、判定はチャネルで受け取るフレームと MP4 の実体で行います。

検証する内容は次の 5 点です。

1. publisher のパイプラインが SETUP と PUBLISH を完了する
2. subscriber のパイプラインが catalog を取得して SUBSCRIBE を完了する (最初のフレームが publisher の生成するサイズでデコードできること)
3. subscriber がデコード済みの映像フレームを継続して受け取る (5 秒間で 30 フレーム以上、かつ配信の終了間際まで届くこと)
4. subscriber が書き出した MP4 が 0 バイトより大きい
5. どちらのパイプラインも shutdown で正常終了する

Track Namespace は実行ごとに一意な値を使うため、同じ relay を同時に使う実行とは混ざりません。

## ローカルでの実行

実 relay が必要なため、通常の `cargo test` では実行しません (`#[ignore]` を付けています)。接続先の URL は環境変数で渡します。ホスト名はリポジトリに残さないでください。

```console
$ MOQT_E2E_URL=moqt://<HOST>/ cargo test -p e2e-tests -- --ignored
running 1 test
test publish_and_subscribe_roundtrip ... ok
```

## 環境変数

| 変数 | 既定 | 内容 |
| --- | --- | --- |
| `MOQT_E2E_URL` | (必須) | 接続先 URL (`moqt://host[:port]/path`) |
| `MOQT_E2E_TRANSPORT` | `quic` | トランスポート (`quic` / `wt-h3` / `wt-h2`) |
| `MOQT_E2E_KEEP` | (未設定) | `1` を指定すると受信した MP4 を残す (残した場所は `--nocapture` を付けたときに表示される) |

`MOQT_E2E_URL` が未設定の場合は、何が必要かを示すメッセージでテストが失敗します。受信した MP4 は一時ディレクトリ (OS の一時ディレクトリ配下の `moqt-e2e-<PID>/subscriber.mp4`) に置き、終了時に削除します。

## CI

`.github/workflows/e2e-test.yml` がこのテストを実行します。接続先は `secrets.TEST_MOQT_URI`、トランスポートは `vars.TEST_MOQT_TRANSPORT` から渡し、`secrets.TEST_MOQT_URI` が未設定の場合はテストを実行しません (fork からの PR など)。

examples が macOS 専用依存 (Video Toolbox / CoreAudio 系) を持つため、runner は `macos-15` を使います。
