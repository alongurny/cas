//! The order of `uni-bin` values is the bytewise order of their
//! serializations, though it is computed without serializing.

use cas_version::uni::Value;
use proptest::prelude::*;

fn value() -> impl Strategy<Value = Value> {
    // Strings around the LEB128 boundary at 128 bytes too.
    let leaf = prop_oneof![".{0,3}".prop_map(Value::str), (120usize..140).prop_map(|n| Value::str("a".repeat(n)))];
    leaf.prop_recursive(3, 40, 6, |inner| {
        prop::collection::vec((inner.clone(), inner), 0..6).prop_map(|kv| Value::Map(kv.into_iter().collect()))
    })
}

proptest! {
    #[test]
    fn order_is_the_order_of_serializations(a in value(), b in value()) {
        prop_assert_eq!(a.cmp(&b), a.serialize().cmp(&b.serialize()));
    }

    #[test]
    fn deserialize_round_trips(a in value()) {
        prop_assert_eq!(Value::deserialize(&a.serialize()).unwrap(), a);
    }
}
