//! The report and image source refusals display their identifiers first.

use contextful_core::connector::ConnectorError;

#[test]
fn report_and_image_refusals_lead_with_their_identifiers() {
    for (error, id) in [
        (ConnectorError::ConnectorPageCursorMissing("p".into()), "ConnectorPageCursorMissing"),
        (ConnectorError::ConnectorMetricGrainRejected("p".into()), "ConnectorMetricGrainRejected"),
        (ConnectorError::ConnectorOrderingColumnRejected("p".into()), "ConnectorOrderingColumnRejected"),
        (ConnectorError::ConnectorAuthTerminal("p".into()), "ConnectorAuthTerminal"),
        (ConnectorError::ConnectorQuotabilityMismatch("p".into()), "ConnectorQuotabilityMismatch"),
    ] {
        assert_eq!(error.to_string(), format!("{id}: p"));
    }
}
