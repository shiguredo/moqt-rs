# Multipath QUIC

tobi-quic の Multipath QUIC (RFC 10069) を tokio-moq から使うための設定と、moq-pub / moq-sub での指定方法をまとめる。

Multipath QUIC は 1 つの QUIC 接続に複数のネットワークパスを持たせる拡張である。Wi-Fi とモバイル回線のように複数の回線がある端末で、回線が切れたときの切り替え、複数回線の帯域の集約、待機回線の保持ができる。パスの開設・検証・切り替えは QUIC 層が行うため、MOQT の Session とメッセージはパスを意識しない。どのパスでデータが運ばれても、MOQT のストリームと DATAGRAM は同じように処理される。

## 利用できるビルドと経路

Multipath QUIC を利用できるのは、`quic-tobi` バックエンドを選んだ Windows 以外のビルドの native QUIC 経路 (`--transport quic`) だけである。

| バックエンド / 経路 | Multipath QUIC |
| --- | --- |
| `quic-tobi` (Windows 以外) | 利用できる |
| `quic-s2n` | 利用できない (s2n-quic が Multipath QUIC を実装していない) |
| `quic-ngtcp2` | 利用できない (このバックエンドは Multipath QUIC の API を公開していない) |
| `wt-h3` / `wt-h2` / `qmux` | 利用できない (Multipath は native QUIC の機能である) |
| Windows ビルド | 利用できない (tobi-quic が Windows 非対応である) |

`--transport auto` は WebTransport / QMux の候補も競争するため、Multipath を指定した場合はエラーになる。`--transport quic` を明示すること。

```bash
# Multipath を使うには quic-tobi ビルドにする
cargo run -p moq-sub --no-default-features --features quic-tobi -- \
  --url moqt://127.0.0.1:4443 --namespace moq-example --multipath handover
```

既定は `off` であり、`--multipath` を指定したときだけ Multipath QUIC を広告する。無効の場合は `initial_max_path_id` を広告しないため、ピアが追加パスを開くこともない。

## CLI オプション

moq-pub と moq-sub で同じオプションを使う。

| オプション | デフォルト | 説明 |
| --- | --- | --- |
| `--multipath` | `off` | パスの使い方 (`off` / `handover` / `aggregate` / `backup`)。`off` 以外で Multipath QUIC を広告する |
| `--multipath-interface` | (すべて) | 自動でパスを開くインターフェース名のカンマ区切り。`--multipath-interface-deny` とは同時に指定できない |
| `--multipath-interface-deny` | | 自動でパスを開かないインターフェース名のカンマ区切り |
| `--multipath-max-paths` | `2` | 自動で開くパスの同時数の上限 (1〜64)。`Connection::open_path` で明示的に開くパスは制限しない。Path ID の上限は `max(4, この値)` になる |
| `--multipath-network-monitor-ms` | (監視しない) | ネットワークインターフェースの増減を監視する間隔 (ms)。変化に応じてパスを自動で開閉する |
| `--multipath-keep-alive-ms` | (アイドルタイムアウトから算出) | 待機パスに keep-alive を送る間隔 (ms) |

`--multipath-interface` と `--multipath-interface-deny` は、ネットワーク監視を有効にしたときだけ意味を持つ。監視しない場合でも、ピアが開いたパスと `Connection::open_path` で開いたパスは Multipath の対象になる。

## パスの使い方

`--multipath` の値で、複数のパスをどのように使うかを選ぶ。

| モード | 動作 | 用途 |
| --- | --- | --- |
| `handover` | 最良の 1 本で送り、使えなくなったときやより良いパスが現れたときに切り替える | 回線が切れても接続を維持する |
| `aggregate` | 送信に使えるすべてのパスで同時に送る | 複数回線の帯域を束ねる |
| `backup` | 最初のパスで送り続け、使えなくなったときだけ待機パスへ切り替える。元のパスが回復しても戻らない | 予備の回線を待機させる |

`handover` と `backup` では待機させているパスを keep-alive で維持する。`--multipath-keep-alive-ms` を指定しない場合は接続のアイドルタイムアウトから間隔を決めるため、アイドルタイムアウトが無効だと keep-alive は動かない。この実装の transport parameter はアイドルタイムアウトを 30 秒に設定するため、指定しない場合も 22.5 秒間隔で keep-alive を送る。

`aggregate` を指定したときは、複数のパスへデータを振り分けるために tobi-quic の
`WeightedRoundRobin` スケジューラー (ACK はデータを受信したパスで返す `SamePath`) と
BBRv3 を選ぶ。tobi-quic の既定の `MinRtt` は最短 RTT の 1 本にだけ新しいデータを載せるため、
そのままでは `aggregate` でも複数パスが使われない。`handover` / `backup` は tobi-quic の既定
(`MinRtt` / `ShortestPath` / CUBIC) のままである。ほかのスケジューラーや輻輳制御を使う場合は、
tokio-moq の API では選べないため tobi-quic のビルダーを直接使う必要がある。

## ライブラリ API

`tokio_moq::ConnectParams` の `multipath` に `tokio_moq::multipath::MultipathOptions` を設定する。

```rust
use std::time::Duration;

use tokio_moq::ConnectParams;
use tokio_moq::multipath::{InterfaceFilter, MultipathMode, MultipathOptions};

let params = ConnectParams {
    url: &url,
    transport: tokio_moq::Transport::Quic,
    // ... ほかのフィールド ...
    multipath: MultipathOptions {
        mode: MultipathMode::Handover,
        interface_filter: InterfaceFilter::Allow(vec!["en0".to_string()]),
        max_paths: 2,
        initial_max_path_id: 4,
        network_monitor_interval: Some(Duration::from_secs(1)),
        keep_alive_interval: Some(Duration::from_secs(10)),
    },
    happy_eyeballs: happy_eyeballs::Config::default(),
};
```

`MultipathOptions::default()` は `mode: MultipathMode::Off` であり、Multipath を広告しない。`mode` を `Off` 以外にした場合は、接続前に次を検証する。

- `quic-tobi` バックエンドの Windows 以外のビルドであること
- `transport` が `Transport::Quic` であること
- `max_paths` が 1〜64、`initial_max_path_id` が 1〜64 であること
- 監視間隔と keep-alive 間隔が 0 でないこと
- インターフェース名の一覧が空でないこと

`initial_max_path_id` は接続の生存期間で使い回さない Path ID の上限であり、接続中に開ける追加パスの総数の上限になる。パスを何度も開き直す場合は、接続前に大きくするか、接続後に tobi-quic の `Connection::set_max_path_id` を使う。tokio-moq は接続後の Path ID 上限の変更 API を公開していない。

## パスの観測

Multipath を有効にすると、接続確立後にパスイベントを観測するタスクを 1 本起動する。次のイベントをログに出す。

| イベント | ログレベル | 意味 |
| --- | --- | --- |
| `Opened` | info | パスが作られた。まだ使えない |
| `Validated` | info | パスが使えるようになった |
| `ValidationFailed` | warn | パスの検証に失敗した |
| `StatusChanged` / `PeerStatusChanged` | debug | パスの状態 (送信に使う / 待機) が変わった |
| `Closed` | debug | 自側が廃棄したパスが閉じた |
| `Abandoned` | warn | ピアがパスを廃棄した |
| `Blocked` | debug | ピアが Path ID の上限に達した |
| `ObservedAddress` | debug | Address Discovery で観測した自分のアドレス |

ログには Path ID とローカル / リモートのアドレスが出る。どの回線が使われているか、切り替えが起きたかを追うために使う。

## 制約

- Windows では使えない。tobi-quic を依存に含めないため、`quic-tobi` も選べない
- s2n-quic / ngtcp2 バックエンドでは使えない。`--multipath` を指定すると接続前にエラーになる
- WebTransport over HTTP/3 / HTTP/2 と QMux では使えない。native QUIC だけの機能である
- `--transport auto` とは併用できない。`--transport quic` を明示する
- 送信パスを指定する API はない。どのパスで送るかはモードと QUIC 層のスケジューラーが決める
- パスは短い間隔で続けては開けない。tobi-quic の `Connection::open_path` が拒否した場合は間隔を空けて再試行する
- パスの検証には時間がかかる。`open_path` が返った時点ではまだ使えない

## テスト

tokio-moq の `quic::tobi_backend` のテストが、ループバックで実際に Multipath をネゴシエーションし、追加パスを開いて `PathState::Active` になることと、その状態でストリームのデータが届くことを検証する。

```bash
cargo test -p tokio-moq --no-default-features --features quic-tobi multipath
```

moq-pub / moq-sub の CLI テストも同じ feature で実行する。

```bash
cargo test -p moq-pub -p moq-sub --no-default-features --features quic-tobi multipath
```
