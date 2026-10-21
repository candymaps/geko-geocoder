use geko_core::model::PlaceKind;

#[test]
fn admin_levels_of_luxembourg() {
    let levels = &country_pack_lu::pack().admin_levels;
    assert_eq!(levels.kind(4), None, "no regions");
    assert_eq!(levels.kind(6), None, "cantons are not searched by");
    assert_eq!(levels.kind(8), Some(PlaceKind::Municipality));
    assert_eq!(levels.kind(9), Some(PlaceKind::Neighbourhood));
}
