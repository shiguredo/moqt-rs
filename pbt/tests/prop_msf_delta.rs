//! `MsfCatalog::apply_delta` の PBT
//!
//! draft-ietf-moq-msf-02 §5.1.6 (Delta update): 操作は配列順に適用される。
//! 任意のカタログに対して「一意な名前の add」→「同じ名前の remove」を適用すると、
//! トラック集合が元に戻る (add / remove が可逆) ことを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::msf::{
    MsfCatalog, MsfDeltaOperation, MsfDeltaUpdate, MsfPackaging, MsfRemoveTrack, MsfTrack,
};

/// ユニークな名前を持つ Full カタログを生成する
fn sample_catalog(ctx: &mut noprop::TestCaseContext) -> MsfCatalog {
    let count = noprop::sample_usize_in(ctx, 0..=3);
    let mut catalog = MsfCatalog::new();
    for index in 0..count {
        catalog.tracks.push(MsfTrack::new(
            format!("init-{index}"),
            MsfPackaging::Loc,
            true,
        ));
    }
    catalog
}

/// トラック集合を名前で比較できる形に正規化する
fn sorted_names(catalog: &MsfCatalog) -> Vec<String> {
    let mut names: Vec<String> = catalog.tracks.iter().map(|t| t.name.clone()).collect();
    names.sort();
    names
}

/// add したトラックは直後に remove すると元のトラック集合に戻る
#[test]
fn add_then_remove_restores_tracks() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let initial = sample_catalog(ctx);
        let mut catalog = initial.clone();

        // 初期カタログと衝突しない名前を使う (valid-by-construction)
        let name = format!("added-{}", noprop::sample_u64(ctx));
        let added = MsfTrack::new(name.clone(), MsfPackaging::Loc, true);

        let add = MsfDeltaUpdate {
            generated_at: 1,
            operations: vec![MsfDeltaOperation::Add {
                tracks: vec![added],
            }],
        };
        catalog
            .apply_delta(&add, None)
            .expect("一意な名前の add は成功する");
        assert!(
            catalog.tracks.iter().any(|t| t.name == name),
            "add したトラックが存在すること"
        );

        let remove = MsfDeltaUpdate {
            generated_at: 1,
            operations: vec![MsfDeltaOperation::Remove {
                tracks: vec![MsfRemoveTrack::new(name.clone())],
            }],
        };
        catalog
            .apply_delta(&remove, None)
            .expect("直前に追加した名前の remove は成功する");

        assert_eq!(
            sorted_names(&catalog),
            sorted_names(&initial),
            "add → remove でトラック集合が元に戻ること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 同じ delta 内で追加した 2 件目のトラックを remove できること
///
/// 1 回の `apply_delta` の中で索引を更新しながら 2 件を追加し、その後ろのトラックを
/// 削除する。索引の rank 計算が追加分も含めて正しくなければ、削除対象がずれる。
#[test]
fn add_two_then_remove_second_in_same_delta() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let initial = sample_catalog(ctx);
        let mut catalog = initial.clone();
        let suffix = noprop::sample_u64(ctx);
        let first_name = format!("added-first-{suffix}");
        let second_name = format!("added-second-{suffix}");
        let mut second = MsfTrack::new(second_name.clone(), MsfPackaging::Loc, true);
        second.label = Some("second".to_string());
        let delta = MsfDeltaUpdate {
            generated_at: 1,
            operations: vec![
                MsfDeltaOperation::Add {
                    tracks: vec![
                        MsfTrack::new(first_name.clone(), MsfPackaging::Loc, true),
                        second,
                    ],
                },
                MsfDeltaOperation::Remove {
                    tracks: vec![MsfRemoveTrack::new(second_name.clone())],
                },
            ],
        };
        catalog
            .apply_delta(&delta, None)
            .expect("同一 delta 内の add と remove は成功する");
        assert!(
            !catalog.tracks.iter().any(|t| t.name == second_name),
            "2 件目の追加トラックが削除されること"
        );
        assert!(
            catalog.tracks.iter().any(|t| t.name == first_name),
            "1 件目の追加トラックは残ること"
        );
        assert_eq!(
            catalog.tracks.len(),
            initial.tracks.len() + 1,
            "初期トラックと 1 件目の追加トラックが残ること"
        );
        let removed = catalog
            .removed_tracks
            .values()
            .next()
            .expect("削除履歴が存在すること");
        assert_eq!(
            removed.name, second_name,
            "履歴のキーと値が削除したトラックで一致すること"
        );
        Ok(())
    })?;
    Ok(())
}
