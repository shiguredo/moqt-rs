# MsfTrack::stream_mapping を loc の track に限る

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23

## 目的

MSF-02 §5.2.5 (Packaging metadata) は「Subscribers MUST ignore any key that is not defined for the current packaging value.」と定めている。`streamMapping` は §4.1.1 / §5.2.5 で `loc` 用に定義された key である。

しかし `MsfTrack::stream_mapping` は、packaging を問わず `streamMapping` を解釈する。そのため、loc 以外の track が `streamMapping` に不正な値を持つと `InvalidCatalog` になる。

現状の moq-sub は、`select_video_track` が loc 以外の track を先に除外するので、この経路で失敗することはない。しかし 0472 で cmaf の track を選べるようにすると、他の publisher が出す cmaf の track で購読に失敗する。

この API の挙動は cmaf に限らず、loc 以外のすべての packaging に当てはまる。そのため、CMSF の機能追加 (add) とは分けて、API の change として扱う。CMSF 対応の一連の issue と同じ Branch で進める。

## 現状

- `src/msf.rs` の `MsfTrack::stream_mapping` は `Result<StreamMapping, MessageError>` を返す。doc には「`streamMapping` は `loc` packaging 向けのキーであり、他の packaging では未知キーとして無視されるが、本メソッドは互換のため値を解釈する。」とある。
- decode / encode 時の値域検証 `validate_loc_stream_mapping` は、`loc` の track に限られる。
- 呼び出し元は次のとおりである。
  - `crates/moq-sub/src/pipeline.rs` の `extract_video_info`。値が不正なら `catalog video track {:?} has an invalid streamMapping` で失敗する。結果は `VideoTrackInfo` の `stream_mapping` (`StreamMapping`) に入る。
  - `crates/moq-sub/src/pipeline.rs` のテスト `video_track_info_reads_the_stream_mapping` (`extract_video_info` を経由する間接的な呼び出し)
  - `crates/moq-pub/src/catalog.rs` のテスト `catalog_announces_the_video_stream_mapping`
  - `tests/test_msf/track_fields.rs` の `stream_mapping` のテスト

## 設計方針

- `MsfTrack::stream_mapping` の戻り値を `Result<Option<StreamMapping>, MessageError>` に変える。
  - loc の track では、現状どおり解釈する。key がなければ `Some(Group)`、不正値なら `Err` を返す。
  - loc 以外の track では、`packagingMetadata` の内容によらず `Ok(None)` を返す。
- `extract_video_info` は `None` を `StreamMapping::Group` として `VideoTrackInfo` に入れる。
  - MSF-02 §6 は、packaging が定めない限り Object を Subgroup 0 に置く (SHOULD) としている。publisher がこの SHOULD に従う場合、受信は group mapping と同じになる。
  - §6 は SHOULD なので、loc 以外の publisher が 1 Group を複数の Subgroup に分けることは仕様上ありえる。そうした publisher には対応しない。moq-sub の group 経路は stream ごとに復号器を作る (`build_stream_video_decoder`) ため、Subgroup をまたぐ参照が切れる。
  - この割り切りは既知の制約として扱う。ただし、本 issue の完了時点では、moq-sub はまだ loc 以外の track を選ばない (`select_video_track` が先に除外する)。そのため、受信の前提と対応しない publisher についての記録は、loc 以外の track を選べるようにする 0472 で行う。
- 既存の呼び出し元とテストを新しい戻り値に追従させる。loc 以外の track で `None` になるテストを追加する。
- `docs/msf.md` の Catalog の節には、Track (`MsfTrack`) の箇条に `streamMapping` の記述 (「`MsfTrack::stream_mapping()` で解釈する (§4.1.1)。キーがない場合は `group`」) がある。ここに、loc 以外では解釈せず `None` を返すことを追記する。

## 完了条件

- loc の track では、`stream_mapping` が現状どおり `Some(Group)` / `Some(Object)` / `Err` を返すこと。
- loc 以外の track では、`streamMapping` の値が不正でも `Ok(None)` を返すこと。
- `extract_video_info` が loc 以外の track で失敗せず、`StreamMapping::Group` を使うこと。
- `docs/msf.md` の `streamMapping` の記述が新しい挙動に一致していること。
- 上記を `tests/test_msf/track_fields.rs` と moq-sub / moq-pub のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
