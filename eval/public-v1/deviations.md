# Deviations recorded after the second freeze

[protocol.md](protocol.md) is bound by hash into [study/freeze.json](study/freeze.json), so editing it would break
the freeze the baseline ran under (`9bc06141`). Deviations found during or after that run are recorded here, dated.
They will be folded into protocol.md when the study is frozen again for the final run.

## 2026-10-20, after the baseline run: no rule for transport retries

**What happened.** In 7 of the 72 baseline sessions, a model response stream between the client and the owned relay
broke mid-response: a broken pipe, a connection reset, or no status. In each case the client retried and finished
normally, with an answer and with clean transcript integrity. The strict transport check in
[public_transcript.py](../../scripts/eval/public_transcript.py) still rejected the session, because the abandoned
attempt has no matching client message, and it scored the session as an error.

| Arm | Sessions affected |
| --- | ---: |
| `plain` | 3 |
| `knowcode` | 2 |
| `knowcode-unprompted` | 2 |

The protocol had no rule for this case.

**What we did.** The baseline's primary results keep the strict check unchanged, and no session was rerun. A
sensitivity view, labelled as requested after the run, counts those 7 answers as valid.

**Proposed rule for the final run, to be frozen before it starts:**

- A provider response that is incomplete, or that has no status, and whose message id (if any) belongs to no client
  message, is an *abandoned attempt*. It is reported with its count and does not fail the transport check.
- The check still fails when any client message lacks a matching complete, successful provider response; when
  responses are reordered or repeated; when the model or permission mode differs; or when relay cleanup is not
  verified.
- The baseline is re-scored under the adopted rule and published next to its strict score.
