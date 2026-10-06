# moq-pub の datagram 配送を音声トラックに限定する

- Created: 2026-10-06
- Completed: 2026-10-06
- Branch: feature/change-publisher-datagram-tracks

## 目的

`moq-pub` の `--use-datagram` は値なしの真偽フラグで、映像と音声をまとめて datagram 配送に
切り替える。しかし draft-ietf-moq-loc-04 で datagram 配送が示されているのは §4.1 の音声の例
(1 chunk = 1 object = 1 group) だけで、映像の節 (§4.2〜§4.6) は stream 配送であり
datagram には一度も言及していない (§4.2 は "one unidirectional QUIC stream is setup to deliver
all the encoded video chunks within a MOQT group"、§4.3 以降も同じ手順を参照する)。
draft-ietf-moq-msf-01 にも datagram / Delivery Mode の記述は無い。

datagram は 1 つの QUIC パケットに収まる必要があり (draft-ietf-moq-loc-04 §4.1
"When mapped to QUIC datagrams, each object must fit entirely within a QUIC datagram")、
映像のキーフレーム (IDR) は数 KB〜数百 KB になり得て収まらない。映像を datagram で送る選択肢を
example から外し、音声トラックだけを datagram 配送に切り替えられるようにする。

## 現状

- `examples/moq-pub/src/cli.rs` の `--use-datagram` は `noargs::flag` (値なし) で、
  `Config::use_datagram: bool` に入る
- `examples/moq-pub/src/pipeline.rs` は映像 (2 か所) と音声 (1 か所) の送信分岐で
  `config.use_datagram` を一括で見るため、映像だけ subgroup stream に残せない
- catalog は `config.use_datagram` に関係なく常に subgroup stream である
- `examples/README.md` の `--use-datagram` の説明も「映像 / 音声オブジェクトを配信する」と
  一括の挙動を記載している
- draft-ietf-moq-transport-22 §2.1 (Objects) は
  "Every Object within a Group belongs to exactly one Subgroup or Datagram. An Original Publisher
  MAY use both Subgroups and Datagrams within a Group or Track." と混在を許容しており、
  「音声は datagram、映像は stream」は仕様の範囲内である

## 設計方針

- `--use-datagram` を削除し、`--audio-datagram` を追加する (音声トラックだけを datagram 配送に
  切り替える)。映像と catalog は常に subgroup stream とする
- `Config` の `use_datagram: bool` を `audio_datagram: bool` に置き換え、pipeline の映像分岐は
  datagram を使わない (SubgroupWriter のみ)、音声分岐だけが `audio_datagram` を見る
- 起動ログに配送方法を出す (どちらの配送方法で動いているかを実機で判別できるようにする)
- `examples/README.md` の該当行を `--audio-datagram` に更新し、CHANGES.md に `[CHANGE]` を
  追記する (example の CLI は後方互換を保証しない)
- 映像を datagram で送る選択肢は用意しない。必要になった場合は datagram のサイズ上限を
  扱う別の issue で検討する

## 解決方法

`moq-pub` の datagram 配送を音声トラックだけに限定した。

- `--use-datagram` (映像 + 音声の一括指定) を削除し、`--audio-datagram` を追加した。
  既定は無効で、指定したときだけ音声トラックを datagram で送る
- `Config::use_datagram: bool` を `Config::audio_datagram: bool` に置き換え、映像の送信分岐は
  常に `SubgroupWriter` を使うようにした。`current_video_datagram_writer` とその終端処理を削除し、
  キーフレーム時の writer の終端 → 新しい subgroup の開始という順序は変更前の subgroup モードと
  同一である。catalog は従来どおり常に subgroup stream
- 起動ログを `video_delivery=subgroup` / `audio_delivery=datagram|subgroup` に変更し、
  どちらの配送方法で動いているかを実機で判別できるようにした
- 音声トラックを送らない場合 (`--no-audio` / `--input-mp4`) は `--audio-datagram` を黙って
  捨てずに警告する (既存の無視オプションと同じ方針)
- `examples/README.md` の該当行を `--audio-datagram` に更新し、CHANGES.md に `[CHANGE]` を追記した
- 単体テストを 2 件追加した (`--audio-datagram` が既定で無効 / 指定で有効、削除した
  `--use-datagram` が未定義のオプションとしてエラーになること)
- 映像を datagram で送る選択肢は用意しない。datagram のサイズ上限の扱いが必要になった場合は
  別 issue で検討する

## 完了条件

- `--audio-datagram` の指定で音声トラックだけが datagram 配送になり、映像と catalog が
  subgroup stream のままであること (映像の送信分岐が `audio_datagram` を参照しないこと)
- `--use-datagram` が削除され、`--audio-datagram` の解析が単体テストで固定されていること
  (既定は無効、指定で有効)
- `examples/README.md` と CHANGES.md が追随していること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `prek run --all-files` が通ること
