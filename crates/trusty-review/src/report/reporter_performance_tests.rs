use super::*;

/// (d): the Performance section text is byte-identical regardless of the
/// model or whether synthesis ran — it never reads from the model at all.
#[test]
fn note_is_fixed_regardless_of_synthesis() {
    let mut a = Scope::new();
    fill_performance_note(&mut a);
    let mut b = Scope::new();
    fill_performance_note(&mut b);

    let rendered_a = crate::report::fill::render("{{performance_assessment_note}}", &a);
    let rendered_b = crate::report::fill::render("{{performance_assessment_note}}", &b);
    assert_eq!(rendered_a, rendered_b);
    assert_eq!(rendered_a, PERFORMANCE_NOTE);
    assert!(rendered_a.contains("No performance or scalability assessment was made"));
}
