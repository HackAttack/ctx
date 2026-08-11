use super::valid_local_turn_boundary;

#[test]
fn uuid_v7_boundary_uses_only_strictly_later_embedded_timestamps() {
    let session = "019fb000-0000-7000-8000-0000000000ff";
    let later = "019fb000-0001-7000-8000-000000000000";
    let same_time_higher_randomness = "019fb000-0000-7fff-bfff-ffffffffffff";

    assert!(valid_local_turn_boundary(session, later));
    assert!(!valid_local_turn_boundary(
        session,
        same_time_higher_randomness
    ));
    assert!(!valid_local_turn_boundary(
        session,
        "550e8400-e29b-41d4-a716-446655440000"
    ));
    assert!(!valid_local_turn_boundary(
        "550e8400-e29b-41d4-a716-446655440000",
        later
    ));
}
