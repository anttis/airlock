use crate::config::load_config::parse_config;

#[test]
fn nested_kvm_config_defers_host_support_check_to_backend() {
    let value = toml::from_str("[vm]\nkvm = true\n").unwrap();
    let config =
        parse_config(value).expect("KVM configuration must be accepted on Linux and macOS");
    assert!(config.vm.kvm);
}

#[test]
fn nested_kvm_is_disabled_by_default() {
    let config = parse_config(serde_json::json!({})).unwrap();
    assert!(!config.vm.kvm);
}
