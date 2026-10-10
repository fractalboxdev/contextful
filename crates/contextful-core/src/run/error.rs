//! The refusals of the `run` contract's durable-run operations, one variant per error
//! identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// A second resolution carrying a different payload. (`run.suspend.conflicting-resolution`)
    #[error("AwakeableAlreadyResolved: {0}")]
    AwakeableAlreadyResolved(String),
    /// A resolution past the suspension's deadline. (`run.suspend.expired-token`)
    #[error("AwakeableTimedOut: {0}")]
    AwakeableTimedOut(String),
    /// A resume token with no registry row. (`run.suspend.unknown-token`)
    #[error("AwakeableUnknown: {0}")]
    AwakeableUnknown(String),
    /// A journal row's blob reference resolves to no stored blob. (`run.journal.missing-blob`)
    #[error("BlobMissing: {0}")]
    BlobMissing(String),
    /// A stop matching no pending, running or waiting run. (`run.cancel.not-in-flight`)
    #[error("CancelTargetNotInFlight: {0}")]
    CancelTargetNotInFlight(String),
    /// A credentialed stop whose grants cover no execute over the recorded pipeline. (`run.cancel.stop-unauthorized`)
    #[error("CancelUnauthorized: {0}")]
    CancelUnauthorized(String),
    /// A capability the running profile links no implementation for. (`run.journal.unwired-capability`)
    #[error("CapabilityUnwired: {0}")]
    CapabilityUnwired(String),
    /// A stored position's field differs from the declared incremental field. (`run.advance.field-rename`)
    #[error("CursorFieldMismatch: {0}")]
    CursorFieldMismatch(String),
    /// A clock value with no ordering, or a position type changing mid-pass. (`run.advance.unorderable-position`)
    #[error("CursorPositionUnorderable: {0}")]
    CursorPositionUnorderable(String),
    /// A connector identity, component world or plan hash moved under a pending owner. (`run.own.pinned-plan-changed`)
    #[error("ExecutionPinMismatch: {0}")]
    ExecutionPinMismatch(String),
    /// A run whose staged parts pass the per-run staged-bytes bound. (`run.own.staged-bytes`)
    #[error("RunStagedBytesExceeded: {0}")]
    RunStagedBytesExceeded(String),
    /// A declared command that is not an executable file. (`run.exec.missing-binary`)
    #[error("DeriveBinaryMissing: {0}")]
    DeriveBinaryMissing(String),
    /// An executable key on the request side of the binding split. (`run.bind.command-in-manifest`)
    #[error("DeriveCommandInManifest: {0}")]
    DeriveCommandInManifest(String),
    /// A required derive config key absent or blank. (`run.select.required-key`)
    #[error("DeriveConfigKeyMissing: {0}")]
    DeriveConfigKeyMissing(String),
    /// An explicit predecessor differing from the derive source-table parent. (`run.select.derive-after-conflict`)
    #[error("DeriveAfterConflict: {0}")]
    DeriveAfterConflict(String),
    /// A derive source-table graph returning to one of its pipelines. (`run.select.derive-cycle`)
    #[error("DeriveCycle: {0}")]
    DeriveCycle(String),
    /// A caption block starting before the one already accepted. (`run.parse-cues.backward-cue`)
    #[error("DeriveCueOutOfOrder: {0}")]
    DeriveCueOutOfOrder(String),
    /// A pinned file whose bytes differ from its digest. (`run.exec.digest-mismatch`)
    #[error("DeriveDigestMismatch: {0}")]
    DeriveDigestMismatch(String),
    /// A driver and task pairing the tier does not serve. (`run.bind.driver-mismatch`)
    #[error("DeriveDriverMismatch: {0}")]
    DeriveDriverMismatch(String),
    /// An engine host carrying a path, query, port or scheme. (`run.bind.endpoint-host-bare`)
    #[error("DeriveEndpointHostNotBare: {0}")]
    DeriveEndpointHostNotBare(String),
    /// A pipeline naming an engine the machine has not defined. (`run.bind.unbound-engine`)
    #[error("DeriveEngineUnbound: {0}")]
    DeriveEngineUnbound(String),
    /// An environment allowlist name outside the permitted characters. (`run.exec.env-name`)
    #[error("DeriveEnvNameInvalid: {0}")]
    DeriveEnvNameInvalid(String),
    /// Configuration naming another pipeline's output table. (`run.select.foreign-output-table`)
    #[error("DeriveForeignOutputTable: {0}")]
    DeriveForeignOutputTable(String),
    /// A process-spawning key on a fetch binding. (`run.fetch.binding-key`)
    #[error("DeriveFetchBindingKey: {0}")]
    DeriveFetchBindingKey(String),
    /// A followed address with a scheme other than HTTP or HTTPS. (`run.fetch.scheme`)
    #[error("DeriveSchemeUnsupported: {0}")]
    DeriveSchemeUnsupported(String),
    /// A followed host written as an address literal. (`run.fetch.address-literal`)
    #[error("DeriveAddressLiteral: {0}")]
    DeriveAddressLiteral(String),
    /// A document declaring a character set other than UTF-8. (`run.fetch.charset`)
    #[error("DeriveCharsetUnsupported: {0}")]
    DeriveCharsetUnsupported(String),
    /// An undeclared document failing UTF-8 validation. (`run.fetch.not-utf8`)
    #[error("DeriveBytesNotUtf8: {0}")]
    DeriveBytesNotUtf8(String),
    /// A derive pipeline configured to journal its pulls. (`run.select.journaled-pull`)
    #[error("DeriveJournaledPull: {0}")]
    DeriveJournaledPull(String),
    /// A local media path escaping the binding's media root. (`run.bind.media-root`)
    #[error("DeriveMediaOutsideRoot: {0}")]
    DeriveMediaOutsideRoot(String),
    /// A media value neither an address nor a readable local file. (`run.bind.media-unreadable`)
    #[error("DeriveMediaUnreadable: {0}")]
    DeriveMediaUnreadable(String),
    /// A derive source built without a store root or pipeline id. (`run.select.no-store-root`)
    #[error("DeriveNoStoreRoot: {0}")]
    DeriveNoStoreRoot(String),
    /// A step's captured output crossing its bound. (`run.exec.output-cap`)
    #[error("DeriveOutputCap: {0}")]
    DeriveOutputCap(String),
    /// A derive output table without the unit-and-sequence key. (`run.emit.primary-key`)
    #[error("DerivePrimaryKeyMissing: {0}")]
    DerivePrimaryKeyMissing(String),
    /// A remote address handed to an engine that declines them. (`run.bind.remote-url-unsupported`)
    #[error("DeriveRemoteUrlUnsupported: {0}")]
    DeriveRemoteUrlUnsupported(String),
    /// A derive table declaring the reserved genre column. (`run.emit.reserved-discriminator`)
    #[error("DeriveReservedDiscriminator: {0}")]
    DeriveReservedDiscriminator(String),
    /// A settled unit re-entering the outstanding set. (`run.emit.settled-revived`)
    #[error("DeriveSettledUnitRevived: {0}")]
    DeriveSettledUnitRevived(String),
    /// A step command given as a string, not an argument array. (`run.exec.shell-command`)
    #[error("DeriveShellCommand: {0}")]
    DeriveShellCommand(String),
    /// A chain step ending non-zero. (`run.exec.non-zero-exit`)
    #[error("DeriveStepExit: {0}")]
    DeriveStepExit(String),
    /// A preprocess step exiting zero without its output file. (`run.exec.silent-step`)
    #[error("DeriveStepProducedNothing: {0}")]
    DeriveStepProducedNothing(String),
    /// A chain outrunning its deadline. (`run.exec.deadline-elapsed`)
    #[error("DeriveStepTimeout: {0}")]
    DeriveStepTimeout(String),
    /// The single-file verb handed a fetch engine. (`run.test-engine.unsupported-driver`)
    #[error("DeriveTestEngineUnsupported: {0}")]
    DeriveTestEngineUnsupported(String),
    /// A parent row without a key or media value. (`run.select.incomplete-unit`)
    #[error("DeriveUnitIncomplete: {0}")]
    DeriveUnitIncomplete(String),
    /// A transcribe pipeline declaring a shared-quota grant. (`run.select.unmetered-grant`)
    #[error("DeriveUnmeteredGrant: {0}")]
    DeriveUnmeteredGrant(String),
    /// A link preview without a run-bound mediated client. (`run.select.metered-client`)
    #[error("DeriveMeteredClient: {0}")]
    DeriveMeteredClient(String),
    /// A unit status outside the four. (`run.emit.unit-status`)
    #[error("DeriveUnitStatusUnknown: {0}")]
    DeriveUnitStatusUnknown(String),
    /// A `task` naming neither a built-in nor a registered task. (`run.bind.unknown-task`)
    #[error("DeriveUnknownTask: {0}")]
    DeriveUnknownTask(String),
    /// A host task registered under a name already taken. (`run.bind.host-task`)
    #[error("DeriveTaskNameTaken: {0}")]
    DeriveTaskNameTaken(String),
    /// A host-task pipeline's tables, or a unit's rows, outside its task's tables. (`run.emit.output-tables`)
    #[error("DeriveOutputTablesMismatch: {0}")]
    DeriveOutputTablesMismatch(String),
    /// A preprocess condition naming no known condition. (`run.exec.step-condition`)
    #[error("DeriveStepConditionUnknown: {0}")]
    DeriveStepConditionUnknown(String),
    /// A path-form command with no content digest. (`run.exec.unpinned-path`)
    #[error("DeriveUnpinnedPath: {0}")]
    DeriveUnpinnedPath(String),
    /// A confidence outside the normalized interval. (`run.bind.confidence-range`)
    #[error("DeriveConfidenceOutOfRange: {0}")]
    DeriveConfidenceOutOfRange(String),
    /// A link engine returning the engine-unavailable variant. (`run.bind.engine-unavailable`)
    #[error("DeriveLinkEngineUnavailable: {0}")]
    DeriveLinkEngineUnavailable(String),
    /// An engine reaching wider than its source table's zones. (`run.bind.locality-wider`)
    #[error("DeriveLocalityWider: {0}")]
    DeriveLocalityWider(String),
    /// A row zone taken from the operator's advisory key. (`run.bind.advisory-zone`)
    #[error("DeriveAdvisoryZone: {0}")]
    DeriveAdvisoryZone(String),
    /// Per-unit failure state recorded outside the output table. (`run.emit.failure-off-table`)
    #[error("DeriveFailureOffTable: {0}")]
    DeriveFailureOffTable(String),
    /// A `last_error` written without address redaction. (`run.emit.unredacted-error`)
    #[error("DeriveUnredactedError: {0}")]
    DeriveUnredactedError(String),
    /// A history window bound outside the two accepted spellings. (`run.record.bound-spelling`)
    #[error("HistoryBoundSpelling: {0}")]
    HistoryBoundSpelling(String),
    /// A table's visibility block refused by the `disclosure` contract, carried typed.
    #[error(transparent)]
    Visibility(#[from] crate::disclosure::VisibilityError),
    /// A table declaration refused by the `store` contract at manifest validation, carried typed.
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    /// An input outside a shape the contract bounds without naming a refusal.
    #[error("invalid: {0}")]
    Invalid(String),
    /// A published model declaring no contract. (`run.model.contract-required`)
    #[error("ModelContractUndeclared: {0}")]
    ModelContractUndeclared(String),
    /// A hold naming a build no committed manifest records. (`run.model.hold-unknown-build`)
    #[error("ModelBuildUnknown: {0}")]
    ModelBuildUnknown(String),
    /// A build reading a table that declares a disclosure key. (`run.model.restricted-input`)
    #[error("ModelInputRestricted: {0}")]
    ModelInputRestricted(String),
    /// A model test returning a row. (`run.model.test-failed`)
    #[error("ModelTestFailed: {0}")]
    ModelTestFailed(String),
    /// A build naming no declared model. (`run.model.unknown-model`)
    #[error("ModelUndeclared: {0}")]
    ModelUndeclared(String),
    /// A materialization failing its declared contract. (`run.publish.contract-mismatch`)
    #[error("PipelineContractMismatch: {0}")]
    PipelineContractMismatch(String),
    /// A manifest top-level key outside the enumerated blocks. (`run.model.top-level-block`)
    #[error("PipelineUnknownBlock: {0}")]
    PipelineUnknownBlock(String),
    /// Write-path redaction declared over a source whose pulls are journaled. (`run.journal.redacting-source`)
    #[error("JournalRedactionConflict: {0}")]
    JournalRedactionConflict(String),
    /// One pipeline id declared twice. (`run.declare.duplicate-id`)
    #[error("PipelineDuplicateId: {0}")]
    PipelineDuplicateId(String),
    /// The replacing write mode beside a windowed or chunked load. (`run.declare.replace-unsupported`)
    #[error("PipelineReplaceUnsupported: {0}")]
    PipelineReplaceUnsupported(String),
    /// A seeded stamp at or past the ceiling. (`run.seed.ceiling-breached`)
    #[error("PipelineSeedCeilingBreached: {0}")]
    PipelineSeedCeilingBreached(String),
    /// A seeded batch whose stamp cannot be ordered against the ceiling. (`run.seed.ceiling-unevaluable`)
    #[error("PipelineSeedCeilingUnevaluable: {0}")]
    PipelineSeedCeilingUnevaluable(String),
    /// A seeded table lacking a key or an event-time ordering. (`run.seed.declaration-missing`)
    #[error("PipelineSeedDeclarationMissing: {0}")]
    PipelineSeedDeclarationMissing(String),
    /// A seeded table no scheduled, enabled fold covers. (`run.seed.compaction-cadence`)
    #[error("PipelineSeedCompactionMissing: {0}")]
    PipelineSeedCompactionMissing(String),
    /// A validation that read no manifest file. (`run.declare.manifest-missing`)
    #[error("PipelineManifestMissing: {0}")]
    PipelineManifestMissing(String),
    /// A manifest file the canonical type cannot deserialize. (`run.declare.spec-invalid`)
    #[error("PipelineSpecInvalid: {0}")]
    PipelineSpecInvalid(String),
    /// Two tables whose destination names fold to one spelling. (`run.declare.table-name-collision`)
    #[error("PipelineTableNameCollision: {0}")]
    PipelineTableNameCollision(String),
    /// One table's pull failing inside a fire. (`run.land.table-failed`)
    #[error("PipelineTableFailed: {0}")]
    PipelineTableFailed(String),
    /// A non-zero exit or fatal signal from the decode process. (`run.land.parse-crashed`)
    #[error("PipelineParseCrashed: {0}")]
    PipelineParseCrashed(String),
    /// A read covering part of a multi-part input. (`run.land.partial-parse`)
    #[error("PipelinePartialParse: {0}")]
    PipelinePartialParse(String),
    /// A normalize mode outside `native` and `relational`. (`run.normalize.mode-unknown`)
    #[error("PipelineNormalizeModeUnknown: {0}")]
    PipelineNormalizeModeUnknown(String),
    /// A relational child table without a list index. (`run.normalize.list-index-missing`)
    #[error("PipelineListIndexMissing: {0}")]
    PipelineListIndexMissing(String),
    /// A chain operation emitting more rows than it consumed. (`run.transform.arity`)
    #[error("PipelineTransformArity: {0}")]
    PipelineTransformArity(String),
    /// A chain operation naming a column the batch lacks. (`run.transform.column-missing`)
    #[error("PipelineTransformColumnMissing: {0}")]
    PipelineTransformColumnMissing(String),
    /// A source config key outside the set the source enumerates. (`run.declare.config-key`)
    #[error("PipelineUnknownConfigKey: {0}")]
    PipelineUnknownConfigKey(String),
    /// A destination other than the local store. (`run.land.unknown-destination`)
    #[error("PipelineUnknownDestination: {0}")]
    PipelineUnknownDestination(String),
    /// Input a parser cannot read, named by path and position. (`run.land.unreadable-input`)
    #[error("PipelineUnreadableInput: {0}")]
    PipelineUnreadableInput(String),
    /// A snapshot metadata write past its serialized bound. (`run.project.metadata-too-large`)
    #[error("RunMetadataTooLarge: {0}")]
    RunMetadataTooLarge(String),
    /// A store-driven input statement the read face truncated at its row ceiling. (`run.journal.input-truncated`)
    #[error("RunInputTruncated: {0}")]
    RunInputTruncated(String),
    /// A `retry_on` naming a tag outside the two retryable ones. (`run.retry.unretryable-tag`)
    #[error("RunRetryTagUnretryable: {0}")]
    RunRetryTagUnretryable(String),
    /// A run-stream request with no valid credential before the upgrade. (`run.project.unauthenticated-upgrade`)
    #[error("RunStreamUnauthorized: {0}")]
    RunStreamUnauthorized(String),
    /// A site id with no bound source, or two. (`run.record.site-id-unresolved`)
    #[error("SiteIdUnresolved: {0}")]
    SiteIdUnresolved(String),
    /// A step closed on a terminal failure, carrying its label and tag. (`run.retry.step-failed`)
    #[error("StepFailed: step `{label}` closed on {failure}")]
    StepFailed { label: String, failure: super::failure::Failure },
}
