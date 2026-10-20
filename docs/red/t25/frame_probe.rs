// Reproduction for finding F2 of docs/red/t25/README.md.
//
// Not compiled by the workspace. To run it, copy it to
// crates/ostrel_sync/tests/frame_probe.rs on c9d04cd and run
// `cargo test -p ostrel_sync --test frame_probe -- --nocapture`.
// Observed on c9d04cd: ctrl_encoded=1572866 quote_encoded=524290
// five_ascii_fields=1310730 against frame_bytes=1048576; the test fails.

use ostrel_core::canon::encode;
use ostrel_core::value::Value;
use ostrel_sync::protocol::LIMITS;

#[test]
fn one_valid_text_value_fits_one_frame() {
    let n = LIMITS.text_bytes as usize;
    let ctrl = encode(&Value::Text("\u{1}".repeat(n)));
    let quote = encode(&Value::Text("\"".repeat(n)));
    let five_ascii: usize = (0..5)
        .map(|_| encode(&Value::Text("a".repeat(n))).len())
        .sum();
    println!("text_bytes={} frame_bytes={}", n, LIMITS.frame_bytes);
    println!(
        "ctrl_encoded={} quote_encoded={} five_ascii_fields={}",
        ctrl.len(),
        quote.len(),
        five_ascii
    );
    assert!(
        ctrl.len() <= LIMITS.frame_bytes as usize,
        "a single valid Text value exceeds one frame"
    );
}
