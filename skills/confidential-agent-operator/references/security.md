# Security Guidance

## Claims You Can Make With Evidence

- Resource files are injected only after the CLI completes the remote attestation challenge.
- Traffic through `confidential-agent connect` is protected by TNG RATS-TLS.
- `status --live` reflects guest daemon/app readiness when it can query the guest status endpoint.
- Rekor mode binds reference values to signed provenance when `attestation.reference_values=rekor` and the Rekor setup succeeds.

## Claims You Must Not Invent

- Do not claim attestation succeeded unless the relevant CLI command completed successfully.
- Do not claim model weights or runtime downloads are measured unless the build/reference policy or a separate signed hash check covers them.
- Do not use CPU flags, `/dev/tdx_guest`, logs, or cloud instance type as a substitute for remote attestation.
- Do not claim the target agent is sandboxed unless the image actually integrates a policy enforcement point for that runtime path.

## Secret Handling

- Never print API keys, bearer tokens, cloud credentials, private keys, or generated gateway tokens.
- Redact logs before attaching them to eval artifacts.
- Prefer mode `0600` for resource files containing config or credentials.
- Delete controller-side temporary secret files during cleanup.

## Evaluation Anti-Cheat

- The target service must be the real upstream, not a local mock.
- Record upstream URL and commit hash.
- Verify process command, installed files, and response behavior.
- Keep target-specific grader logic outside the skill package.
