# example の SubgroupWriter が FIRST_OBJECT を正しく設定する

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/fix-example-first-object-bit
- Polished: 2026-09-11

## 目的

draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) の FIRST_OBJECT bit の主張と example の実際の送信内容を一致させる。

## 現状

`examples/moqt-publisher/src/stream_writer.rs` の `SubgroupWriter` は `first_object: true` を常に設定し、`SubgroupWriter::new` の時点でヘッダをワイヤへ送る。`write_object` はフィルタ不通過の Object をワイヤへ送らず `ObjectFilterOutcome::Skip` を返すため、Object 0 が Skip されると、ワイヤ上の最初の Object は original publisher が subgroup に公開した最初の Object ではなくなる。
実装コメント自身がこの矛盾を認め、ヘッダ送信の遅延を先送りしている。`examples/moqt-publisher/src/main.rs` は draft-21 準拠を宣言している。

根拠:

- draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): FIRST_OBJECT bit は「その subgroup stream の最初の Object が original publisher により subgroup に公開された最初の Object である」ことを示す。
- §2.2: original publisher は新しい subgroup を開くとき、最初の Object がその subgroup に公開された最初の Object であることを示すよう FIRST_OBJECT bit を設定しなければならない (MUST)。subgroup の先頭が公開済みの最初の Object でない場合に bit を立てると MUST 違反になる。

## 設計方針

- ヘッダのワイヤ送信だけを最初の Pass まで遅延し、ストリーム開設と Session 登録 (`data_plane.send_subgroup_header`) は `new` の時点で行う。`send_subgroup_object` のフィルタ評価と `send_data_stream_closed` は Session 登録を前提とするため、登録まで遅延してはならない。
- 最初の Pass が Object 0 なら `first_object: true`、Object 0 が Skip されて Object 1 以降が最初の Pass なら `first_object: false` とする。
- 一度も Pass しなかった Writer の `finish` は FIN ではなく reset で終端する。draft §11.3.2 (Closing Subgroup Streams): FIN は「Start Location 未満の Object を除き、配送すべき全 Object を配送した」場合のみに限られ、配送前に閉じる場合は MUST reset (省略理由には Forward State 起因も含まれる)。フィルタ不通過は reset 側に該当する。
  draft は objects を送らない Subgroup に SUBGROUP_HEADER + RESET_STREAM_AT (reliable_size = ヘッダ長) を MAY で示すが、example の transport API に RESET_STREAM_AT が無いため、ヘッダを送らず RESET_STREAM で代替し、その制約を実装コメントに残す。reset の error code は `DataStreamResetReason` から選ぶ。
- `first_object` の決定とヘッダの遅延送信のロジックは I/O から分離し、単体テストで確認できる形にする。

## 完了条件

- Object 0 が Pass した場合、最初の Pass で送るヘッダの `first_object` が true になること
- Object 0 が Skip されて Object 1 以降が最初の Pass になった場合、最初の Pass で送るヘッダの `first_object` が false になること
- 一度も Pass しなかった Writer の `finish` が FIN ではなく reset で終端されること (FIN で終端しないこと。draft-ietf-moq-transport-21 §11.3.2)
- ヘッダのワイヤ送信が最初の Pass まで遅延され、Session 登録 (`send_subgroup_header`) は `new` の時点で完了していること
- `first_object` の決定と全 Skip 時の挙動が回帰テストで固定されていること
- example のビルドと `--fake-capture-device` による疑似キャプチャ動作が維持されること

## 解決方法

`SubgroupWriter` のヘッダ送信を最初の Pass まで遅延し、FIRST_OBJECT bit と終端方法を仕様に合わせた。

- ストリーム開設と Session 登録は `new` で行い、ワイヤへの SUBGROUP_HEADER 送信は最初の Pass まで遅延した。最初の Pass が Object 0 なら `first_object: true`、Object 0 が Skip された場合は false とする。
- Object ID 追跡と FIRST_OBJECT 決定を sans-I/O の `SubgroupObjectState` に統合し、Skip を含む delta と reset 要否を単体テストで固定した。
- 一度も Pass しなかった場合と、フィルタ不通過で省略した Object が 1 つでもある場合は、FIN ではなく `DataStreamResetReason::Cancelled` で reset し、`RequestStreamEnd::Reset` を通知する (draft-ietf-moq-transport-21 §11.3.2 の MUST)。
- `catalog.rs` のカタログ Skip 経路でも `finish` を呼び、reset で終端するようにした。
- `CHANGES.md` の `[FIX]` にエントリを追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
