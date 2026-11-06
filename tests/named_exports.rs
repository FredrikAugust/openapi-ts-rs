use std::process::Command;

fn generate(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_openapi-ts-rs"))
        .arg("tests/fixtures/named-exports/root.yml")
        .args(args)
        .output()
        .expect("openapi-ts-rs runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8 output")
}

#[test]
fn exports_each_schema_by_name_and_skips_names_defined_twice() {
    let ts = generate(&["--named-exports"]);
    let exports: Vec<&str> = ts.lines().filter(|l| l.starts_with("export type ") && l.contains(" = ")).collect();
    assert!(exports.contains(&r#"export type Pet = components["schemas"]["Pet"];"#), "{ts}");
    assert!(exports.contains(&r#"export type Owner = components["schemas"]["Owner"];"#), "{ts}");
    assert!(exports.contains(&r#"export type Tag = external["pets.yml"]["components"]["schemas"]["Tag"];"#), "{ts}");
    assert!(!exports.iter().any(|l| l.starts_with("export type Shared ")), "{ts}");
    assert!(ts.contains("//   Shared: owners.yml, pets.yml"), "{ts}");
    assert!(!ts.contains("not-an-identifier = "), "{ts}");
}

#[test]
fn skips_names_that_would_shadow_a_referenced_type_or_are_reserved_words() {
    let ts = generate(&["--named-exports"]);
    assert!(ts.contains("Record<string, never>"), "the fixture must reference Record: {ts}");
    assert!(!ts.contains("export type Record ="), "{ts}");
    assert!(!ts.contains("export type import ="), "{ts}");
}

#[test]
fn output_without_the_flag_has_no_named_exports() {
    let ts = generate(&[]);
    assert!(!ts.contains("export type Pet ="), "{ts}");
    assert!(generate(&["--named-exports"]).starts_with(&ts), "the flag only appends");
}
