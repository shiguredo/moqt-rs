//! MSF カタログの購読状態
//!
//! catalog track を SUBSCRIBE して受信した Object を、draft-ietf-moq-msf-01 §5 (Catalog) の
//! 配置規則で適用する状態を持つ。
//!
//! - 各 Group の最初の Object (Object ID 0) は独立した完全なカタログ (MUST)
//! - 同じ Group の以降の Object (Object ID >= 1) は delta update (MUST)
//! - 最新 Group の最初の Object より前の catalog update は無視する (MUST)
//!
//! 購読は Joining FETCH の代替として 2 経路でカタログを受信する
//! (draft-ietf-moq-transport-22 §3.5.1 (Dynamically Starting New Groups))。
//! 購読前に配られた分は FETCH 応答で、購読後に配られた分は SUBSCRIBE で届くため、
//! 同じ Location の Object が両方に現れうる。適用時に Location の重複と逆戻りを
//! 弾くことで、delta の二重適用を防ぐ。

use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::msf::{MsfCatalog, MsfCatalogDocument};

use crate::error::{Error, Result};

/// catalog track から受信した 1 Object
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatalogObject {
    /// Object の Location
    pub(crate) location: Location,
    /// デコードしたカタログドキュメント
    pub(crate) document: MsfCatalogDocument,
}

impl CatalogObject {
    /// Object の payload をカタログの 1 Object としてデコードする
    pub(crate) fn decode(location: Location, payload: &[u8]) -> Result<Self> {
        let document = MsfCatalogDocument::decode(payload).map_err(|e| {
            Error::Other(format!(
                "failed to parse MSF catalog: {e}, raw={}",
                String::from_utf8_lossy(payload),
            ))
        })?;
        Ok(Self { location, document })
    }
}

/// catalog Object を適用した結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogApplyOutcome {
    /// 独立したカタログで置き換えた (Object ID 0)
    Replaced,
    /// delta update を適用した (Object ID >= 1)
    DeltaApplied,
    /// 規則に合わない、または適用済みのため適用しなかった。文字列は英語のログ用の理由
    Ignored(&'static str),
}

/// 受信した catalog Object を順に適用する状態
#[derive(Debug)]
pub(crate) struct CatalogState {
    /// catalog track の namespace (draft-ietf-moq-transport-22 §8.8 (Representing Namespace and
    /// Track Names) の表現。draft-ietf-moq-msf-01 §11.1.2 (MSF Namespace-Name String Encoding) が
    /// URL fragment 用に同じ符号化を再掲する)
    ///
    /// delta update の track object は add / remove / clone のいずれも namespace を省略できるため
    /// (§5.1.6 (Delta update) / §5.2.2 (Track namespace))、省略時の継承先として
    /// [`MsfCatalog::apply_delta`] へ渡す。継承の解決は catalog JSON の `namespace` との
    /// 文字列比較であるため、この表現で書かれたカタログにだけ効く。
    catalog_namespace: String,
    /// 適用済みのカタログ
    catalog: Option<MsfCatalog>,
    /// 適用済みの最新 Group ID
    latest_group: Option<u64>,
    /// 最後に適用した Location (重複と逆戻りの検出に使う)
    last_applied: Option<Location>,
}

impl CatalogState {
    /// catalog track の namespace を受け取って空の状態を作る
    ///
    /// 継承先が不明な状態 (namespace を省略した delta を解決できない状態) を作らないため、
    /// 接続時に確定している catalog track の namespace を必須で受け取る。
    pub(crate) fn new(catalog_namespace: impl Into<String>) -> Self {
        Self {
            catalog_namespace: catalog_namespace.into(),
            catalog: None,
            latest_group: None,
            last_applied: None,
        }
    }

    /// 適用済みのカタログを返す
    pub(crate) fn catalog(&self) -> Option<&MsfCatalog> {
        self.catalog.as_ref()
    }

    /// 受信した 1 Object を適用する
    ///
    /// 適用しなかった場合は理由付きの [`CatalogApplyOutcome::Ignored`] を返す。delta の適用に
    /// 失敗した場合 (属性の不変条件違反など) は `Err` を返し、カタログは変更しない
    /// (`MsfCatalog::apply_delta` が複製へ適用してから差し替えるため)。
    pub(crate) fn apply(&mut self, object: &CatalogObject) -> Result<CatalogApplyOutcome> {
        let location = object.location;
        // 最新 Group の最初の Object より前の catalog update は無視する
        // (draft-ietf-moq-msf-01 §5 (Catalog))
        if let Some(latest) = self.latest_group
            && location.group_id < latest
        {
            return Ok(CatalogApplyOutcome::Ignored(
                "catalog object precedes the latest group",
            ));
        }
        // FETCH 応答と購読の両方に同じ Object が現れうる。location が前に戻る受信も
        // 含めて弾き、delta の二重適用を防ぐ
        if let Some(last) = self.last_applied
            && location <= last
        {
            return Ok(CatalogApplyOutcome::Ignored(
                "duplicate or out-of-order catalog object",
            ));
        }
        match &object.document {
            MsfCatalogDocument::Full(full) => {
                // 独立したカタログは各 Group の最初の Object (Object ID 0) だけ
                if location.object_id != 0 {
                    return Ok(CatalogApplyOutcome::Ignored(
                        "independent catalog without the first object of a group",
                    ));
                }
                self.catalog = Some(full.clone());
                self.latest_group = Some(location.group_id);
                self.last_applied = Some(location);
                Ok(CatalogApplyOutcome::Replaced)
            }
            MsfCatalogDocument::Delta(delta) => {
                // Object ID 0 は独立した完全なカタログでなければならない
                if location.object_id == 0 {
                    return Ok(CatalogApplyOutcome::Ignored(
                        "delta at the first object of a group",
                    ));
                }
                if self.catalog.is_none() {
                    return Ok(CatalogApplyOutcome::Ignored(
                        "delta without an independent catalog",
                    ));
                }
                // 直前の Group の delta は、その Group の独立したカタログにだけ適用できる
                if self.latest_group != Some(location.group_id) {
                    return Ok(CatalogApplyOutcome::Ignored(
                        "delta from a group without its first object",
                    ));
                }
                let catalog = self
                    .catalog
                    .as_mut()
                    .expect("catalog presence checked above");
                // namespace を省略した remove / clone は catalog track の namespace を継承する
                // (§5.2.2 の MUST)。継承先を渡さないと、namespace を宣言した track と
                // 省略した操作が一致せず、remove / clone は対象が見つからず失敗し、add は
                // 重複を検出できず同じ (namespace, name) の track が二重に登録される
                catalog
                    .apply_delta(delta, Some(&self.catalog_namespace))
                    .map_err(|e| Error::Other(format!("failed to apply MSF delta catalog: {e}")))?;
                self.last_applied = Some(location);
                Ok(CatalogApplyOutcome::DeltaApplied)
            }
        }
    }
}

/// カタログが持つ track 名をログ用に並べた文字列を返す
pub(crate) fn track_names(catalog: &MsfCatalog) -> String {
    catalog
        .tracks
        .iter()
        .map(|track| track.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストで使う catalog track の namespace (§8.8 表現)
    const TEST_NAMESPACE: &str = "moq-example";

    /// テストで使う Full カタログの JSON
    ///
    /// 必須フィールド (version / tracks) だけを持つ最小のカタログである。
    fn full_json(version_track: &str) -> Vec<u8> {
        format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"{version_track}","packaging":"loc","isLive":true}}]}}"#
        )
        .into_bytes()
    }

    /// テストで使う delta カタログの JSON (track を 1 つ追加する)
    ///
    /// delta update は version を持ってはならない (draft-ietf-moq-msf-01 §5.3 (Delta updates))。
    fn delta_json(add_track: &str) -> Vec<u8> {
        format!(
            r#"{{"deltaUpdate":[{{"op":"add","tracks":[{{"name":"{add_track}","packaging":"loc","isLive":true}}]}}]}}"#
        )
        .into_bytes()
    }

    /// 独立したカタログが最新 Group になり、置き換えとして扱われること
    ///
    /// draft-ietf-moq-msf-01 §5 (Catalog): 各 Group の最初の Object (Object ID 0) は
    /// 独立した完全なカタログ。
    #[test]
    fn apply_full_replaces_the_catalog() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let object = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json("video"),
        )
        .expect("カタログをデコードできること");
        assert_eq!(
            state.apply(&object).expect("適用結果が返ること"),
            CatalogApplyOutcome::Replaced,
            "Object ID 0 の Full はカタログを置き換えること"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video"
        );
    }

    /// 同じ Group の Object ID >= 1 が delta として適用されること
    ///
    /// draft-ietf-moq-msf-01 §5 (Catalog): 同じ Group の以降の Object は delta update。
    #[test]
    fn apply_delta_updates_the_catalog() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json("video"),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_json("audio"),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::DeltaApplied,
            "Object ID 1 の delta を適用すること"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video, audio",
            "delta の追加が反映されること"
        );
    }

    /// 新しい Group の Full が古い Group の delta を無効にすること
    ///
    /// 適用済みの Group と異なる Group の delta は、その Group の独立したカタログが
    /// 先に来ていないため適用しない。
    #[test]
    fn apply_delta_from_another_group_is_ignored() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json("video"),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 101,
                object_id: 1,
            },
            &delta_json("audio"),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::Ignored("delta from a group without its first object"),
            "独立したカタログを持たない Group の delta は適用しないこと"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video",
            "カタログが変わらないこと"
        );
    }

    /// 最新 Group より前の Group の Full が無視されること
    ///
    /// draft-ietf-moq-msf-01 §5 (Catalog): 最新 Group の最初の Object より前の
    /// catalog update は無視する (MUST)。
    #[test]
    fn apply_full_from_an_older_group_is_ignored() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json("video"),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let older = CatalogObject::decode(
            Location {
                group_id: 99,
                object_id: 0,
            },
            &full_json("legacy"),
        )
        .expect("カタログをデコードできること");
        assert_eq!(
            state.apply(&older).expect("適用結果が返ること"),
            CatalogApplyOutcome::Ignored("catalog object precedes the latest group"),
            "最新 Group より前の Full は無視すること"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video",
            "カタログが変わらないこと"
        );
    }

    /// 同じ Location の再受信が無視されること (FETCH 応答と購読の重複)
    ///
    /// 購読前に配られた分は FETCH 応答でも購読でも届きうる。同じ delta を 2 度適用すると
    /// カタログが壊れるため、適用済みの Location は弾く。
    #[test]
    fn apply_duplicate_location_is_ignored() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json("video"),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");
        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_json("audio"),
        )
        .expect("delta をデコードできること");
        state.apply(&delta).expect("delta を適用できること");

        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::Ignored("duplicate or out-of-order catalog object"),
            "同じ Location の再受信は適用しないこと"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video, audio",
            "delta が二重適用されないこと"
        );
    }

    /// 独立したカタログより先に delta が来たら適用しないこと
    ///
    /// 購読を始めた時点で Group の途中から受信する場合、独立したカタログより先に delta が
    /// 届きうる。適用先が無いため無視し、次の Full を待つ。
    #[test]
    fn apply_delta_without_full_is_ignored() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_json("audio"),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::Ignored("delta without an independent catalog"),
            "独立したカタログが無いときの delta は適用しないこと"
        );
        assert!(state.catalog().is_none(), "カタログが生まれないこと");
    }

    /// namespace を宣言した track を持つ Full カタログの JSON
    ///
    /// 引数は (namespace, track 名) の並び。draft-ietf-moq-msf-01 §5.6.1 (Time-aligned
    /// Audio/Video Tracks with single quality) が track に `namespace` を宣言する形の例である
    /// (同節の値は §11.1.2 の符号化形ではないため、構造だけを参考にする)。
    fn full_json_with_tracks(tracks: &[(&str, &str)]) -> Vec<u8> {
        let tracks_json = tracks
            .iter()
            .map(|(namespace, name)| {
                format!(
                    r#"{{"name":"{name}","namespace":"{namespace}","packaging":"loc","isLive":true}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"version":"draft-01","tracks":[{tracks_json}]}}"#).into_bytes()
    }

    /// namespace を省略した remove の delta の JSON
    ///
    /// draft-ietf-moq-msf-01 §5.6.5 (Delta update removing tracks) の例も track の namespace を
    /// 省略する (同節の例が持つ `generatedAt` は省略する)。
    fn delta_remove_json(tracks: &[&str]) -> Vec<u8> {
        let tracks_json = tracks
            .iter()
            .map(|name| format!(r#"{{"name":"{name}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"deltaUpdate":[{{"op":"remove","tracks":[{tracks_json}]}}]}}"#).into_bytes()
    }

    /// namespace を省略した clone の delta の JSON
    ///
    /// `parentNamespace` を省略すると親の namespace は catalog の namespace と仮定される
    /// (draft-ietf-moq-msf-01 §5.2.34 (Parent namespace))。
    fn delta_clone_json(name: &str, parent_name: &str) -> Vec<u8> {
        format!(
            r#"{{"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"{name}","parentName":"{parent_name}"}}]}}]}}"#
        )
        .into_bytes()
    }

    /// namespace を省略した remove が catalog track の namespace を継承すること
    ///
    /// draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は、track が namespace を宣言しない場合は
    /// catalog track の namespace を継承する MUST を定める。§5.6.5 の remove は track の
    /// namespace を省略するため、継承先を解決できないと削除対象を引き当てられず失敗する。
    #[test]
    fn apply_delta_removes_a_track_that_inherits_the_catalog_namespace() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json_with_tracks(&[(TEST_NAMESPACE, "video"), (TEST_NAMESPACE, "slides")]),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_remove_json(&["slides"]),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::DeltaApplied,
            "namespace を省略した remove を適用すること"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video",
            "namespace を継承した track が削除されること"
        );
    }

    /// namespace を省略した clone が catalog track の namespace を継承すること
    ///
    /// clone track 自身の namespace (§5.2.2) と `parentNamespace` (§5.2.34 (Parent namespace))
    /// の両方を省略している。`parentNamespace` の省略時は catalog の namespace を親の
    /// namespace と仮定するため、継承先を解決できないと親トラックを引き当てられない。
    #[test]
    fn apply_delta_clones_a_track_that_inherits_the_catalog_namespace() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json_with_tracks(&[(TEST_NAMESPACE, "video")]),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_clone_json("video-hd", "video"),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::DeltaApplied,
            "namespace を省略した clone を適用すること"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video, video-hd",
            "clone で追加された track が反映されること"
        );
    }

    /// namespace を省略した add が catalog track の namespace を継承して重複を検出すること
    ///
    /// draft-ietf-moq-msf-01 §5.2.3 (Track name) は (namespace, name) が一意であることを MUST と
    /// する。namespace を省略した add も catalog track の namespace を継承するため (§5.2.2)、
    /// 同じ名前の track は重複として拒否する。
    #[test]
    fn apply_delta_rejects_an_add_that_duplicates_an_inherited_track() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            &full_json_with_tracks(&[(TEST_NAMESPACE, "video")]),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_json("video"),
        )
        .expect("delta をデコードできること");
        let error = state
            .apply(&delta)
            .expect_err("namespace を省略した add でも同じ名前の track は重複になること");
        assert!(
            error.to_string().contains("already exists"),
            "重複として拒否されること: {error}"
        );
        assert_eq!(
            track_names(state.catalog().expect("カタログがあること")),
            "video",
            "適用に失敗した delta でカタログが変わらないこと"
        );
    }

    /// namespace が異なる同名 track を namespace 省略の remove が削除しないこと
    ///
    /// 継承先は catalog track の namespace に限られるため (§5.2.2)、同じ名前でも別の
    /// namespace を宣言した track は削除対象にならない。
    #[test]
    fn apply_delta_does_not_remove_a_track_from_another_namespace() {
        let mut state = CatalogState::new(TEST_NAMESPACE);
        let full = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 0,
            },
            // 同じ名前の track を catalog の namespace と別の namespace に 1 つずつ置く
            &full_json_with_tracks(&[(TEST_NAMESPACE, "video"), ("other_namespace", "video")]),
        )
        .expect("カタログをデコードできること");
        state.apply(&full).expect("Full を適用できること");

        let delta = CatalogObject::decode(
            Location {
                group_id: 100,
                object_id: 1,
            },
            &delta_remove_json(&["video"]),
        )
        .expect("delta をデコードできること");
        assert_eq!(
            state.apply(&delta).expect("適用結果が返ること"),
            CatalogApplyOutcome::DeltaApplied,
            "namespace を省略した remove を適用すること"
        );
        let catalog = state.catalog().expect("カタログがあること");
        assert_eq!(catalog.tracks.len(), 1, "削除されるのは 1 件だけであること");
        assert_eq!(
            catalog.tracks[0].namespace.as_deref(),
            Some("other_namespace"),
            "catalog track と namespace が異なる track は削除対象にならないこと"
        );
    }

    /// パースできない payload がエラーになること
    #[test]
    fn decode_rejects_invalid_payload() {
        let result = CatalogObject::decode(
            Location {
                group_id: 1,
                object_id: 0,
            },
            b"not json",
        );
        assert!(result.is_err(), "カタログでない payload はエラーになること");
    }
}
