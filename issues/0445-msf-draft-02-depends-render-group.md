# MSF カタログの depends に renderGroup 参照を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.15 (Dependencies) は、`depends` の要素に track name (JSON String) と renderGroup (JSON Number) の両方を許した。

- String 要素は、カタログ内の「依存元以外の track object」の Track Name にちょうど 1 つ一致しなければならない (MUST)。
- Number 要素は、カタログ内のちょうど 1 つの renderGroup の識別子に一致しなければならない (MUST)。
- 解決できない dependency を含むカタログは invalid である (MUST)。
- v01 の「依存先の namespace は依存元と同一と仮定する」は削除された。

現行実装は String 専用で、Number 要素を型エラーとして拒否する。また、依存の解決検証を行わない。

## 現状

- `MsfTrack.depends: Vec<String>`、`MsfCloneTrack.depends: Option<Vec<String>>` で、`decode_common_track_fields` は JSON Number を `MessageError::InvalidCatalog` にする。
- `validate_full_track` は media timeline / event timeline の `depends` が非空かどうかだけを見て、参照先が解決できるかを検証しない。
- `validate_track_name_uniqueness` は namespace 付きの Full Track Name で一意性を見るため、同名別 namespace の track を許す。`depends` の String がその名前に一致すると 2 件に一致し、仕様の「ちょうど 1 つ」に反するが、現行はこれを検出しない。
- `publishTracks` の要素も `decode_track` を通り、`depends` を持てる。依存解決の対象を tracks だけにするか publishTracks も含めるかが未定義である。
- 既存のテストと PBT には、カタログ内に存在しない track 名を `depends` に入れた fixture がある (`tests/test_msf/track_fields.rs` の `sd` / `md`、`tests/test_msf/encode_decode_roundtrip.rs` の `audio` / `video`、`pbt/tests/prop_msf.rs` のランダム名)。解決検証を追加すると、これらは invalid になる。

## 設計方針

- `depends` の要素を表す公開 enum `MsfDependency::TrackName(String)` / `MsfDependency::RenderGroup(u64)` を追加し、`DisplayJson` で String / Number を出し分ける。
- `MsfTrack.depends` と `MsfCloneTrack.depends` の型をこの enum の列に変更する。公開型の変更を伴う破壊的変更として doc に明記する。
- カタログ全体の依存解決検証を追加する。対象は `tracks` と `publishTracks` の両方とする。`publishTracks` は通常の track object と同じ構造を持ち、通常の track が publish track を参照することも、その逆もあり得る。
- String 要素は、依存元の track を除いた track object の Track Name にちょうど 1 つ一致すること。0 件 (未解決または自己参照) と 2 件以上 (同名の track が複数) は invalid とする。
- Number 要素は、`tracks` と `publishTracks` のいずれかの track が持つ renderGroup の識別子に一致すること。複数の track が同じ renderGroup を共有する場合は 1 つの renderGroup として扱い、有効とする。
- 検証は full decode の `decode_full_catalog`、full encode の `validate_full_catalog_for_encode`、delta 適用後の `validate_after_delta` の 3 入口から呼ぶ。encode が自身の decode で拒否されるカタログを出力しないことを保証する。
- 既存 fixture を、カタログ内に実在する track 名 / renderGroup を参照するように修正する。PBT の生成器は、生成したカタログ内の track 名 / renderGroup から `depends` を選ぶように変更する。
- media timeline / event timeline の「depends 非空」の検証は 0456 が担当する。本 issue では型と解決検証に限定する。

## 完了条件

- `"depends": ["video"]` と `"depends": [1]` の両方が decode / encode で往復一致すること。
- 存在しない track name、存在しない renderGroup の参照が `MessageError::InvalidCatalog` で拒否されること。
- 同じ Track Name を持つ track が 2 つあるカタログで、その名前を参照する `depends` が拒否されること。
- 依存元自身の Track Name を参照する `depends` が拒否されること。
- 複数の track が同じ renderGroup を共有している場合に、その renderGroup への参照が解決されること。
- `publishTracks` の track の `depends` も検証され、通常の track が publish track を、publish track が通常の track を参照できること。
- 既存 fixture と PBT の生成器が更新され、`cargo test --workspace` が通ること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/delta_apply.rs`、`tests/test_msf/track_fields.rs`、`pbt/tests/prop_msf.rs` で固定し、`cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
