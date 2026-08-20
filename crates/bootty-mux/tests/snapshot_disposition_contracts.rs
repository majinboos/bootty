use bootty_mux::snapshot::{MuxSnapshot, MuxSnapshotDisposition};

#[test]
fn authoritative_is_the_default_and_is_omitted_from_remote_json() {
    let snapshot = MuxSnapshot::default();
    let json = serde_json::to_value(&snapshot).expect("snapshot should serialize");

    assert!(
        !json
            .as_object()
            .expect("snapshot should serialize as an object")
            .contains_key("disposition")
    );
    assert_eq!(
        serde_json::from_value::<MuxSnapshot>(json)
            .expect("legacy snapshot JSON should deserialize")
            .disposition,
        MuxSnapshotDisposition::Authoritative
    );
}

#[test]
fn transient_is_explicit_in_remote_json() {
    let snapshot = MuxSnapshot {
        disposition: MuxSnapshotDisposition::Transient,
        ..MuxSnapshot::default()
    };

    let json = serde_json::to_value(&snapshot).expect("snapshot should serialize");

    assert_eq!(json["disposition"], "Transient");
    assert_eq!(
        serde_json::from_value::<MuxSnapshot>(json)
            .expect("transient snapshot should deserialize")
            .disposition,
        MuxSnapshotDisposition::Transient
    );
}
