use rusterisk_core::DialFailure;

#[test]
fn branch_failures_follow_the_reference_precedence_independently_of_arrival_order() {
    let priority = [
        DialFailure::Unavailable,
        DialFailure::Congestion,
        DialFailure::Busy,
        DialFailure::NoAnswer,
    ];
    for (first, left) in priority.iter().copied().enumerate() {
        for (second, right) in priority.iter().copied().enumerate() {
            assert_eq!(left.combine(right), priority[first.max(second)]);
            assert_eq!(left.combine(right), right.combine(left));
            for third in priority {
                assert_eq!(
                    left.combine(right).combine(third),
                    left.combine(right.combine(third))
                );
            }
        }
        assert_eq!(left.combine(left), left);
    }
}
