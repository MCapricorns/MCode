//! Sole-current FeaturePack and ProviderPack contract tests.

use mcode_plugin_api::{
    FEATURE_PACK_WIT_PACKAGE, PROVIDER_INTERFACE, PROVIDER_INTERFACE_ID, PROVIDER_WIT_PACKAGE,
    PROVIDER_WORLD, PROVIDER_WORLD_ID, PROVIDER_WORLD_VERSION,
};

#[test]
fn feature_pack_package_is_sole_current() {
    assert_eq!(FEATURE_PACK_WIT_PACKAGE, "mcode:feature-pack@0.0.1");
}

#[test]
fn provider_constants_identify_the_sole_current_contract() {
    assert_eq!(PROVIDER_WIT_PACKAGE, "mcode:provider-pack@0.0.1");
    assert_eq!(PROVIDER_WORLD, "provider");
    assert_eq!(PROVIDER_WORLD_VERSION, "0.0.1");
    assert_eq!(PROVIDER_WORLD_ID, "mcode:provider-pack/provider@0.0.1");
    assert_eq!(PROVIDER_INTERFACE, "provider-api");
    assert_eq!(
        PROVIDER_INTERFACE_ID,
        "mcode:provider-pack/provider-api@0.0.1"
    );
}
