//! Red review T146: membership hijack against the add wins set at 038e377 (AC-55, G12, D96).
//! Each test states the attack and the expected outcome under D96: a remove may only ever
//! affect the element it names.

use ostrel_core::ids::{OpId, ReplicaId, RowId};
use ostrel_core::value::Value;
use ostrel_crdt::{AddWinsSet, MAX_REMOVE_TAGS};

fn op(replica: u64, seq: u32) -> OpId {
    OpId { replica: ReplicaId(replica), seq }
}

fn user(n: u8) -> Value {
    Value::Ref(RowId::from_hex(&format!("{:032x}", n)).unwrap())
}

const ME: u8 = 1;
const ATTACKER_REPLICA: u64 = 0xa;

/// Membership {me, v2..=v4}, each added by its own replica.
fn room() -> AddWinsSet {
    let mut s = AddWinsSet::new();
    for n in 1..=4u8 {
        s.add(op(n as u64, 1), user(n)).unwrap();
    }
    s
}

fn all_tags(s: &AddWinsSet) -> Vec<OpId> {
    s.values().flat_map(|e| s.tags_of(e)).collect()
}

#[test]
fn hijack_one_op_remove_me_naming_every_tag() {
    let mut s = room();
    let tags = all_tags(&s);
    assert_eq!(s.remove(&user(ME), &tags), Ok(true));
    assert!(!s.contains(&user(ME)));
    for n in 2..=4 {
        assert!(s.contains(&user(n)), "victim {n} removed by a remove of me");
    }
}

#[test]
fn hijack_several_ops_remove_me_with_victim_tags_only() {
    let mut s = room();
    s.add(op(ATTACKER_REPLICA, 2), user(ME)).unwrap();
    let before = s.encode_state();
    for n in 2..=4u8 {
        let victim_tags = s.tags_of(&user(n));
        assert_eq!(s.remove(&user(ME), &victim_tags), Ok(false));
    }
    assert_eq!(s.encode_state(), before);
}

#[test]
fn hijack_full_64_tag_batches_of_foreign_tags() {
    let mut s = AddWinsSet::new();
    for r in 1..=200u64 {
        s.add(op(r, 1), user(2)).unwrap();
    }
    s.add(op(ATTACKER_REPLICA + 1000, 1), user(ME)).unwrap();
    let victim = s.tags_of(&user(2));
    for chunk in victim.chunks(MAX_REMOVE_TAGS) {
        assert_eq!(s.remove(&user(ME), chunk), Ok(false));
    }
    assert_eq!(s.tags_of(&user(2)).len(), 200);
    assert!(s.contains(&user(ME)));
}

/// D106: every start element of a `make` carries the OpId of the make as its tag, so several
/// elements share one tag. A remove of one of them must not take the others with it.
#[test]
fn make_tag_shared_by_all_start_elements() {
    let make = op(7, 1);
    let mut s = AddWinsSet::new();
    for n in 1..=4u8 {
        assert_eq!(s.add(make, user(n)), Ok(true));
    }
    assert_eq!(s.remove(&user(ME), &[make]), Ok(true));
    assert!(!s.contains(&user(ME)));
    for n in 2..=4 {
        assert!(s.contains(&user(n)));
        assert_eq!(s.tags_of(&user(n)), vec![make]);
    }
}

#[test]
fn stale_and_future_tags_of_the_named_element_change_nothing() {
    let mut s = AddWinsSet::new();
    s.add(op(1, 5), user(ME)).unwrap();
    assert_eq!(s.remove(&user(ME), &[op(1, 4)]), Ok(false));
    assert_eq!(s.remove(&user(ME), &[op(1, 6)]), Ok(false));
    assert_eq!(s.remove(&user(ME), &[op(1, u32::MAX)]), Ok(false));
    assert!(s.contains(&user(ME)));
}

#[test]
fn seq_boundaries() {
    let mut s = AddWinsSet::new();
    assert_eq!(s.add(op(1, 0), user(ME)), Ok(true));
    assert_eq!(s.add(op(1, 0), user(ME)), Ok(false));
    assert_eq!(s.add(op(1, u32::MAX), user(ME)), Ok(true));
    assert_eq!(s.add(op(1, u32::MAX), user(ME)), Ok(false));
    assert_eq!(s.remove(&user(ME), &[op(1, u32::MAX)]), Ok(true));
    assert!(s.is_empty());
    assert_eq!(s.add(op(u64::MAX, 0), user(ME)), Ok(true));
}

/// Remove of `me` first (tags unknown yet), victim add later: victim must survive, any order.
#[test]
fn remove_before_add_of_named_foreign_tag_is_ignored() {
    let mut s = AddWinsSet::new();
    s.add(op(ATTACKER_REPLICA, 1), user(ME)).unwrap();
    assert_eq!(s.remove(&user(ME), &[op(2, 1)]), Ok(false));
    s.add(op(2, 1), user(2)).unwrap();
    assert!(s.contains(&user(2)));
}

/// String class values compare by wire string (D61): a Text equal to a reference hex is the
/// same element as that reference. Not reachable in a typed Set[Ref User] after schema
/// validation (ARCH 5.5); recorded so the boundary is explicit.
#[test]
fn text_with_reference_hex_is_the_same_element() {
    let mut s = room();
    let forged = Value::Text(format!("{:032x}", 2));
    let tags = s.tags_of(&user(2));
    assert_eq!(s.remove(&forged, &tags), Ok(true));
    assert!(!s.contains(&user(2)));
}

#[test]
fn empty_text_element_and_duplicate_tags() {
    let mut s = AddWinsSet::new();
    s.add(op(1, 1), Value::Text(String::new())).unwrap();
    s.add(op(2, 1), Value::Text(String::new())).unwrap();
    assert_eq!(s.remove(&Value::Text(String::new()), &[op(1, 1), op(1, 1)]), Ok(true));
    assert_eq!(s.tags_of(&Value::Text(String::new())), vec![op(2, 1)]);
}
