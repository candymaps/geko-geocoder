use geko_core::model::PlaceKind;

#[test]
fn admin_levels_of_spain() {
    let levels = &country_pack_es::pack().admin_levels;
    assert_eq!(levels.kind(4), Some(PlaceKind::Region));
    assert_eq!(levels.kind(6), Some(PlaceKind::Province));
    assert_eq!(levels.kind(7), None, "comarcas are not searched by");
    assert_eq!(levels.kind(8), Some(PlaceKind::Municipality));
    assert_eq!(levels.kind(10), Some(PlaceKind::Neighbourhood));
}
