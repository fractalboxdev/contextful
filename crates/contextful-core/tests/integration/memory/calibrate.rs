//! `read.synthesize.confidence-calibration` and `read.settle.calibration-report`.

use contextful_core::memory::calibrate::{report, Calibration, Confidence, ECE_BINS, HOLD_OUT_EVERY};
use contextful_core::memory::calibrate::Settled;
use contextful_core::memory::declare::Shape;

fn settled(predicate: &str, confidence: f64, held: bool) -> Settled {
    Settled { shape: Shape::Facts, predicate: predicate.into(), confidence, held }
}

/// An overconfident source: every claim emits 0.9 and holds half the time.
fn overconfident(predicate: &str, n: usize) -> Vec<Settled> {
    (0..n).map(|i| settled(predicate, 0.9, i % 2 == 0)).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// A model-emitted confidence is labelled `uncalibrated` until a shape-and-predicate isotonic map trained on settled outcomes lowers held-out Brier score; a validated map supplies the reported calibrated confidence.
// spec: read.synthesize.confidence-calibration@dc3aeac9
#[test]
fn emitted_confidence_stays_uncalibrated_until_a_held_out_map_validates() {
    let mut samples = overconfident("ships", 40);
    // A perfectly calibrated predicate: the map cannot lower held-out Brier score.
    samples.extend((0..20).map(|i| settled("hires", if i % 2 == 0 { 1.0 } else { 0.0 }, i % 2 == 0)));
    let calibration = Calibration::fit(&samples);

    let shipped = calibration.confidence(Shape::Facts, "ships", 0.9);
    assert_eq!(shipped.label(), "calibrated");
    assert!(close(shipped.value(), 0.5), "{shipped:?}");

    assert_eq!(calibration.confidence(Shape::Facts, "hires", 1.0), Confidence::Uncalibrated(1.0));
    assert_eq!(calibration.confidence(Shape::Facts, "acquires", 0.7), Confidence::Uncalibrated(0.7));
    assert_eq!(calibration.confidence(Shape::Episodes, "ships", 0.9), Confidence::Uncalibrated(0.9));
    assert_eq!(Calibration::default().confidence(Shape::Facts, "ships", 0.9).label(), "uncalibrated");

    // Fewer settled predictions than one hold-out stride leaves nothing to score the map on.
    let sparse = Calibration::fit(&overconfident("ships", HOLD_OUT_EVERY - 1));
    assert_eq!(sparse.confidence(Shape::Facts, "ships", 0.9), Confidence::Uncalibrated(0.9));
}

/// Calibration reports group settled predictions by shape and predicate, and name sample count, Brier score, expected calibration error over 10 equal-width bins, and the held-out score {{read.synthesize.confidence-calibration}} uses, over every fifth prediction.
// spec: read.settle.calibration-report@3163c17f
#[test]
fn the_report_names_count_brier_ece_and_held_out_score_per_shape_and_predicate() {
    assert_eq!((ECE_BINS, HOLD_OUT_EVERY), (10, 5));
    let mut samples = overconfident("ships", 10);
    samples.push(Settled { shape: Shape::Episodes, predicate: "ships".into(), confidence: 0.2, held: false });
    samples.push(settled("hires", 0.25, true));
    samples.push(settled("hires", 0.75, true));
    let groups = report(&samples);
    let keys: Vec<(Shape, &str)> = groups.iter().map(|g| (g.shape, g.predicate.as_str())).collect();
    assert_eq!(keys, [(Shape::Episodes, "ships"), (Shape::Facts, "hires"), (Shape::Facts, "ships")]);

    let ships = &groups[2];
    assert_eq!((ships.samples, ships.held_out), (10, 2));
    // Half of ten at (0.9 - 1)^2 = 0.01 and half at 0.81.
    assert!(close(ships.brier, 0.41), "{ships:?}");
    assert!(close(ships.ece, 0.4), "{ships:?}");
    // Held out: the fifth and tenth, one holding and one not; the eight fitted settle at 0.5.
    assert!(close(ships.held_out_raw_brier.unwrap(), 0.41), "{ships:?}");
    assert!(close(ships.held_out_brier.unwrap(), 0.25), "{ships:?}");
    assert!(ships.calibrated);

    let hires = &groups[1];
    assert_eq!((hires.samples, hires.held_out), (2, 0));
    assert!(close(hires.brier, (0.75f64.powi(2) + 0.25f64.powi(2)) / 2.0), "{hires:?}");
    // Two bins: |0.25 - 1| / 2 + |0.75 - 1| / 2.
    assert!(close(hires.ece, 0.5), "{hires:?}");
    assert_eq!((hires.held_out_raw_brier, hires.held_out_brier, hires.calibrated), (None, None, false));

    let episodes = &groups[0];
    assert_eq!(episodes.samples, 1);
    assert!(close(episodes.brier, 0.04));
}
