#[test]
fn only_core_can_prove_ownership_and_vacancy() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/proofs/pass/*.rs");
    cases.compile_fail("tests/proofs/fail/*.rs");
}
