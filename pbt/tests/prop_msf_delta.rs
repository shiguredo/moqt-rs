//! `MsfCatalog::apply_delta` の PBT
//!
//! draft-ietf-moq-msf-01 §5.1.6 (Delta update): 操作は配列順に適用される。
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
            generated_at: None,
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
            generated_at: None,
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
