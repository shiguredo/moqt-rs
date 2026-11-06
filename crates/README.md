# MoQ サンプル

Media over QUIC の publisher / subscriber クライアントのサンプルです。
draft-ietf-moq-transport-23、draft-ietf-moq-loc-05、draft-ietf-moq-msf-01、draft-ietf-moq-c4m-02 に準拠しています。

- [moq-pub](moq-pub/README.md)：catalog / audio / video track を PUBLISH する publisher
- [moq-sub](moq-sub/README.md)：catalog と audio / video を SUBSCRIBE して再生する subscriber
- [tokio-moq](tokio-moq/README.md)：publisher / subscriber が共有するトランスポート層 (ライブラリ)

[Tokio](https://github.com/tokio-rs/tokio)、[tobi-quic](https://github.com/shiguredo/tobi-quic)、[http3-rs](https://github.com/shiguredo/http3-rs)、[http2-rs](https://github.com/shiguredo/http2-rs)、[qmux-rs](https://github.com/shiguredo/qmux-rs) を利用しています。
QUIC の実装には tobi-quic を採用しています (WebTransport over HTTP/3 も tobi-quic)。ネイティブ QUIC は cargo feature で [s2n-quic](https://github.com/aws/s2n-quic) / [ngtcp2-rs](https://github.com/shiguredo/ngtcp2-rs) も選べます。詳細は [tokio-moq](tokio-moq/README.md) を参照してください。
接続先となる MoQ リレーは別途用意してください。

Rust 1.95 以降が必要です (tobi-quic と s2n-quic の要求。ライブラリと pbt は 1.93 でビルドできます)。macOS ではカメラとマイクへのアクセス許可が必要です。

## 実行例

```bash
# publisher (疑似キャプチャ)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device

# subscriber
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example
```

## 接続設定

`--url`、`--transport`、`--namespace`、C4M 認可トークン、TLS 証明書の扱いは publisher と subscriber で共通です。詳細は [tokio-moq](tokio-moq/README.md) を参照してください。

- 接続経路は `--url` の scheme ではなく `--transport` (`quic` / `wt-h3` / `wt-h2` / `qmux`) で選ぶ。省略時は MSF fragment の `connection` (`q` / `wt`)、それも無ければ `quic`
- `wt-h3` / `wt-h2` は `--transport wt-h3` / `--transport wt-h2` で選ぶ。`wt-h2` は `WT_RESET_STREAM` capsule を使うため MoQ リレーの capsule 対応に依存する
- `qmux` は TCP + TLS 上で QUIC のストリームと DATAGRAM を多重化する経路で、TCP の再送による遅延や DATAGRAM の破棄があり得る
- WebTransport (`wt-h3`) の QUIC 層は tobi-quic 固定である。cargo feature で選べるのはネイティブ QUIC (`--transport quic`) のバックエンドだけである
- `--namespace` を省略すると MSF fragment の track-identifier の namespace を使う。どちらも無ければエラー
- MSF fragment の `c4m` は SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN として送信する
- `--cert` を省略すると TLS の証明書検証をスキップする。本番環境では必ず指定する

```bash
# C4M トークン付き (シェルでは & を引用符で囲む)
cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&c4m=<base64 token>' --fake-capture-device
```

各サンプルのオプションは [moq-pub](moq-pub/README.md) / [moq-sub](moq-sub/README.md) と `--help` で確認できます。
