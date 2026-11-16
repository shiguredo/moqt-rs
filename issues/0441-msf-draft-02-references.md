# MSF の仕様参照を draft-02 に更新する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-msf-02.txt` へ参照仕様を更新した。コードとドキュメントの仕様参照は draft-01 のままで、節番号と表番号が v02 と一致しない。v02 は節番号を再利用しているため、番号だけを機械的に置換すると別の節を指す誤りが残る。

本 issue は `src`、`crates`、`docs`、`tests`、`pbt`、`fuzz`、`Cargo.toml` の MSF 参照を v02 に同期する。`README.md` と `skills/shiguredo-moq/SKILL.md` の MSF の版表記と URL は 0437 が担当する。

## 現状

- `draft-ietf-moq-msf-01` の参照は、本 issue の対象範囲に 45 ファイル・544 箇所ある。内訳は `src` + `crates` が 26 ファイル・392 箇所 (節番号付き 379)、`tests` が 136 箇所、`docs` が 3 箇所、`pbt` + `fuzz` が 11 箇所、`Cargo.toml` が 2 箇所である。
- 節番号の変化: v01 §5.2.5〜§5.2.36 は v02 で +1 になる。v01 §5.2.37 (Token) は v02 で削除され、v01 §5.2.38〜§5.2.44 は同じ番号のままである。
- 表番号の変化: v01 Table 3 (Allowed packaging values) は v02 Table 5、v01 Table 4 (Reserved track roles) は v02 Table 7、v01 Table 5 (encryption schemes) は v02 Table 8、v01 Table 6 (cipher suites) は v02 Table 9、v01 Table 7 (Authorization Schemes) は v02 Table 10。
- 既存の誤りとして、`src/msf.rs` に §7.5.1 への参照が 4 箇所ある。v01 にも v02 にも §7.5 はなく、正しくは §7.4.1 (Template Format) である。
- エラーメッセージの文字列リテラルが `crates/tokio-moq/src/lib.rs` の 528 / 553 / 575 行目と `src/msf.rs` の 1760 / 1768 行目で msf-01 を引用している。参照を 0 件にするには文字列リテラルの変更が必要で、コメントと doc だけの差分にはできない。
- `docs/missing-features.md` の 220 行目と 594 行目は「MSF §11.3 は `SUBSCRIBE_DONE` と書くが、MOQT は `PUBLISH_DONE` に改名している」という対比を書いている。v02 §11.4 は `PUBLISH_DONE` なので、単純置換すると矛盾する。

## 設計方針

- v01 と v02 の対応表を「節番号ではなく節タイトル」で作る。v01 は `git show 8bf5904:refs/moq/draft-ietf-moq-msf-01.txt` で取得し、v02 の目次と突き合わせる。各参照は番号の存在ではなく、参照先の節タイトルで照合する。
- コメントと doc の節番号を v02 に更新する。表番号も上記の対応で更新する。§7.5.1 は §7.4.1 に直す。
- エラーメッセージの文字列リテラルを msf-02 の節番号に更新し、メッセージを期待しているテストも更新する。挙動の変更はエラーメッセージの文字列だけに限定する。
- `docs/missing-features.md` の 2 箇所は「v02 §11.4 は `PUBLISH_DONE` と定める」に書き直し、§11.3 (Initiating a broadcast) から §11.4 (Ending a live broadcast) への移動も反映する。
- 節番号なしの参照 (`§5.2.7` など) や表番号は `rg` の単純検索では検出できないため、対応表と目視で確認する。
- 実施順序: 機能 issue の実装で該当箇所を触るため、本 issue は機能対応の後半に実施する。0437 はさらにその後である。

## 完了条件

- `rg 'draft-ietf-moq-msf-01' src crates docs tests pbt fuzz Cargo.toml` が 0 件であること。
- コード中の MSF の節番号付き参照が、v02 の同じタイトルの節を指していること。少なくとも v01 §5.2.8 (Target latency) → v02 §5.2.9、v01 §5.2.36 (Connection URI) → v02 §5.2.37、§7.4.1 (Template Format)、§11.4 (Ending a live broadcast) を確認する。
- v01 の Table 3 / 4 / 5 / 6 / 7 への参照が、v02 の Table 5 / 7 / 8 / 9 / 10 に更新されていること。
- §7.5.1 への参照が §7.4.1 になっていること。
- エラーメッセージが msf-02 の節番号を引用し、その文言を期待するテストが更新されていること。
- `docs/missing-features.md` の `SUBSCRIBE_DONE` の対比が解消され、v02 §11.4 の `PUBLISH_DONE` として記述されていること。
- `README.md` と `skills/shiguredo-moq/SKILL.md` は本 issue では変更しないこと (0437 の担当)。
- コードの挙動が変わっていないこと (差分がコメント・doc・エラーメッセージ文字列・テスト・Markdown のみ)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
