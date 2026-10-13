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
- `scenarios` — The suite exercises pagination and checkpoints, applied scheduling and dependencies, graceful restart, hard-crash replay, empty and replacement captures, public identifiers, credential masking, typed export acknowledgement and receiver visibility.
- `mutation-controls` — Planted cursor skips, duplicate public identities, repeated in-window scheduling and acknowledgement before receiver commit each fail their owning scenario.
  *because a scenario that accepts its motivating defect proves no compatibility*
- `report` — Evidence records the suite revision and version, supplied binary SHA-256 digest, reported version, feature profile, storage profile and every scenario outcome, requirement and diagnostic.
- `provenance` — Unknown source correspondence remains null; a reported version or successful scenario never supplies a source commit.
  *because source correspondence requires evidence beyond behavior and a version string*
- `artifact-stability` — The suite verifies the supplied artifact's digest before and after execution; changed bytes refuse a compatibility verdict.
- `release-distribution` — Each release archive carries the matching-target suite and compatibility report; release metadata names the report and binary digest, preserving failed and skipped outcomes.
  *A-assurance*
- `foreign-target` — A release target unavailable to its packaging host records explicit skipped scenarios with a target diagnostic; its report supplies no execution pass.
  *because a cross compilation cannot demonstrate the produced artifact's behavior*
- `storage-profile` — The local profile uses isolated filesystem state; `postgres` uses synthetic PostgreSQL and signed S3 services. An unavailable fixture records its named requirement through {{assurance.consumer.requirements}}.
- `d1-profile` — A `cloudflare-d1` verdict runs the supplied `contextful-full` artifact against D1 and R2 bindings, replaces its local disk, and records every required outcome independently of the local and PostgreSQL profiles.
  *A-assurance*
- `d1-release` — Release evidence binds a `cloudflare-d1` report to the artifact digest, source revision, suite revision and storage profile; a required failure, unsupported case, skip or unverified binding supplies no compatibility verdict.
  *A-assurance*
- `d1-deployment` — Deployment readiness requires a replacement run against Cloudflare D1 and R2, with the same artifact digest and source revision as its release report.
  *A-assurance*

#### Scenarios

- `assurance.consumer.requirements`: WHEN a read-only artifact receives a required replay scenario, THEN evidence names the missing run capability and the suite exits unsuccessfully.
- `assurance.consumer.verdict`: WHEN a required scenario is skipped, THEN the aggregate compatibility verdict fails.
- `assurance.consumer.provenance`: WHEN a binary reports a version without source evidence, THEN source correspondence remains null.
- `assurance.consumer.mutation-controls`: WHEN a native source advances past an unserved page, THEN the pagination scenario fails.
