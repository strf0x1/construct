//! Contract: `edit` finds the text the model meant, tolerating the ways a
//! model's copy of a file drifts from the file, and refuses to guess.

use construct::tools::edit::replace_once;

#[test]
fn tiers() {
    // exact
    assert_eq!(replace_once("a b c", "b", "x", false).unwrap(), "a x c");
    // typographic quotes in the file, plain in the model's text
    assert_eq!(replace_once("say \u{201C}hi\u{201D}\n", "say \"hi\"", "bye", false).unwrap(), "bye\n");
    // trailing whitespace differs
    assert_eq!(replace_once("fn a() {   \n}\n", "fn a() {\n}", "x", false).unwrap(), "x\n");
    // CRLF preserved
    assert_eq!(replace_once("a\r\nb\r\n", "a\nb", "c\nd", false).unwrap(), "c\r\nd\r\n");
    // ambiguity is an error unless replace_all
    assert!(replace_once("x x", "x", "y", false).is_err());
    assert_eq!(replace_once("x x", "x", "y", true).unwrap(), "y y");
}
