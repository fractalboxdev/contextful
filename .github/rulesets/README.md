# FlareDispatch required checks

`contextful-gate.proposed.json` is a disabled repository ruleset for `main`. It requires the `flare-dispatch/contextful-gate` parent and the 29 static child contexts returned by `contextful-ci stages --parts`; per-package `test-first.<package>` children vary by change and stay outside the static list; the parent covers fanout and join failures, and a missing child blocks merge.

The list is static while the CLI stage list is derived from the repository. A change to `contextful-ci stages --parts` requires the same change to the proposed ruleset before enforcement. The corpus states the remote obligation in `assurance.gate.remote-check`; local pins prove only CLI behavior, while FlareDispatch owns the webhook and check-run implementation.

The proposal carries `enforcement: disabled` and no `integration_id`. [GitHub's required-check rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#require-status-checks-to-pass-before-merging) admit a specific GitHub App as the expected source after its checks exist. The active ruleset requires all 29 static contexts to report terminal conclusions on a fresh pull-request head and the operator's App ID in each required context; that tenant-specific ID stays outside the repository.
