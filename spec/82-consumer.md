---
contract: assurance
owns:
  - consumer
---

# Consumer compatibility

## consumer

Artifact compatibility: source-independent execution, required capabilities, scenario evidence, storage profiles and release distribution.

- `suite` — `contextful-conformance --binary <path> --output <report>` executes its versioned scenarios through the supplied artifact's public CLI and HTTP surfaces outside a source checkout.
  *A-assurance*
- `credential-isolation` — Every artifact invocation receives an empty inherited environment, an isolated home and scratch directory, a fixed executable search path and explicitly supplied synthetic fixture credentials.
  *because compatibility evidence requires no provider account or inherited credential*
- `requirements` — Every required scenario executes its declared capabilities; a missing capability or unsupported storage profile records `unsupported`, names its requirement and refuses consumer activation.
  *A-assurance*
- `verdict` — A compatibility verdict passes only when every required scenario reports executed success; a parse, an unsupported requirement or a skipped required scenario supplies no execution pass.
  *P7*
- `verdict-exit` — `contextful-conformance` exits 1 after writing a failing verdict and 2 without a report; release packaging records a failed `suite-exit` scenario for any other exit, or for exit 1 over a passing report.
  *because the suite's own failing verdict is evidence, not a crashed process*
- `scenarios` — The suite exercises pagination and checkpoints, applied scheduling and dependencies, graceful restart, hard-crash replay, empty and replacement captures, public identifiers, credential masking, typed export acknowledgement and receiver visibility.
- `mutation-controls` — Planted cursor skips, duplicate public identities, repeated in-window scheduling and acknowledgement before receiver commit each fail their owning scenario.
  *because a scenario that accepts its motivating defect proves no compatibility*
- `report` — Evidence records the suite revision and version, supplied binary SHA-256 digest, reported version, feature profile, storage profile and every scenario outcome, requirement and diagnostic.
- `command-diagnostic` — A failed or timed-out artifact command's diagnostic, a scenario step's included, names its verb and the first error identifier its stderr carries; no other stderr text enters the report.
  *because a provider failure must name its cause while stderr may carry binding credentials*
- `provenance` — Unknown source correspondence remains null; a reported version or successful scenario never supplies a source commit.
  *because source correspondence requires evidence beyond behavior and a version string*
- `artifact-stability` — The suite verifies the supplied artifact's digest before and after execution; changed bytes refuse a compatibility verdict.
- `release-distribution` — Each release archive carries the matching-target suite and compatibility report whatever its verdict; release metadata names the report, binary digest and verdict, preserving failed and skipped outcomes.
  *because a profile lacking a scenario's capability never passes, and refusing its cell withholds the evidence stating so*
- `release-notes` — `contextful-ci formula` writes `release-notes.md` stating every cell's local and `cloudflare-d1` verdict, and fails on a cell carrying none; the operator publishes it as the GitHub release body.
  *because a failing verdict ships, so the release page states it beside the archives*
- `foreign-target` — A release target unavailable to its packaging host records explicit skipped scenarios with a target diagnostic; its report supplies no execution pass.
  *because a cross compilation cannot demonstrate the produced artifact's behavior*
- `storage-profile` — The local profile uses isolated filesystem state; `postgres` uses synthetic PostgreSQL and signed S3 services. An unavailable fixture records its named requirement through {{assurance.consumer.requirements}}.
- `d1-profile` — A `cloudflare-d1` verdict runs the supplied `contextful-full` artifact against D1 and R2 bindings, replaces its local disk, and records every required outcome independently of the local and PostgreSQL profiles.
  *A-assurance*
- `d1-admission` — A `cloudflare-d1` suite holds a lease row in the bound D1 from before its first reset until it ends, lapsing 120 s after its last renewal; a suite meeting a live lease refuses each required scenario with `ConformanceAlreadyRunning`.
  *because a process check inside one container misses a suite another container runs against the same database*
- `d1-release` — Release evidence binds a `cloudflare-d1` report to the artifact digest, source revision, suite revision and storage profile; a required failure, unsupported case, skip or unverified binding supplies no compatibility verdict.
  *A-assurance*
- `d1-absent` — A native `contextful-full` release cell without `CONTEXTFUL_CONFORMANCE_D1_ENDPOINT` runs no `cloudflare-d1` pass; its report records binding origin `absent` and every required scenario skipped with `ConformanceBindingAbsent`.
  *because a release container holds no D1 binding, and the deployment run against D1 and R2 supplies that verdict*
- `d1-deployment` — Deployment readiness requires a replacement run against Cloudflare D1 and R2, with the same artifact digest and source revision as its release report.
  *A-assurance*

#### Scenarios

- `assurance.consumer.requirements`: WHEN a read-only artifact receives a required replay scenario, THEN evidence names the missing run capability and the suite exits unsuccessfully.
- `assurance.consumer.verdict`: WHEN a required scenario is skipped, THEN the aggregate compatibility verdict fails.
- `assurance.consumer.verdict-exit`: WHEN the suite exits 1 over a report whose required scenarios are `unsupported`, THEN the release report holds those scenarios and no `suite-exit` failure.
- `assurance.consumer.d1-absent`: WHEN a native full cell packages without a D1 endpoint, THEN the suite runs once, for `local`.
- `assurance.consumer.command-diagnostic`: WHEN a command fails with a credential in its stderr, THEN the diagnostic names the verb and error identifier, and the report omits the credential.
- `assurance.consumer.provenance`: WHEN a binary reports a version without source evidence, THEN source correspondence remains null.
- `assurance.consumer.mutation-controls`: WHEN a native source advances past an unserved page, THEN the pagination scenario fails.
