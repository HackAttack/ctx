use super::*;

#[test]
fn astrbot_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    let query = "astrbot-import-all-oracle";
    install_default_astrbot_fixture(&temp, query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "astrbot"), (1, 3));

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "astrbot",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "astrbot", query, 1, "message");
}
