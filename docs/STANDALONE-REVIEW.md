# Use Philium for a release review

Select the product checkout in Philium Pilot. In **Review → Release PR revisions → Configure local contract verification**, pin a **separate trusted verifier checkout**. To run live read-only HTTP assertions, explicitly enter its trusted loopback observer port. Then paste the parent PR URL and choose **Verify release**. The history is private to this checkout. Checking does not post comments, approve, merge, publish, deploy or send files to a model.

The installed executable offers the same operation without a window, Jcode or Herdr:

```sh
pilot="$HOME/Applications/Philium Pilot.app/Contents/MacOS/philium"
"$pilot" --pin-review-driver /path/to/product /path/to/verifier 18421  # once
"$pilot" --review-url /path/to/product 'https://github.com/OWNER/REPO/pull/123'
```

The command writes a JSON result. A zero exit status means the review ran, **not** that it passed; read `decision`. Public origin metadata can be read without credentials. Private metadata needs GitHub read access available to the Pilot process. The product checkout must already contain the pinned Git objects. Missing objects produce incomplete evidence; the verifier never executes code from a PR to fill the gap.

## Verifier contract

Commit `philium/sql-contracts.json` in the independent verifier before pinning. For example:

```json
{"release_records":true,"cases":[{"id":"config","document":"display.json","pointer":"/columns/1","expect":"status"}],"live_cases":[{"id":"shown","path":"/view/first","pointer":"/column","expect":"status"}]}
```

`release_records: true` opts into reading new **records**, not new executable policy. The product cannot change its pinned cases. The trusted build/review process commits `philium/releases/<full-head>.json` in the same verifier checkout on a descendant of the pinned verifier commit. Record the exact `origin` (lowercase `owner/repo`), numeric `pr`, `base`, `head`, `driver_pin` and `artifact` (`sha256:` plus the artifact digest). An old head or changed driver pin does not match. Do not point the verifier at the product's own object store.

The observer's deployment endpoint is documented in `RELEASE-VERIFICATION.md`. Live paths are read-only GETs to the enrolled loopback service, with no redirects, proxy or supplied credentials. Successful responses must be bounded JSON and carry single `X-Philium-Revision` and `X-Philium-Artifact` headers matching the independently recorded revision and digest. Philium repeats each observation, stores hashes rather than response bodies and checks the replica identities again afterward. These are statements by the trusted observer, not proof about an arbitrary server that happens to occupy a port.

## Decision

* **Do not ship**: a repeatable candidate regression or a mismatch on the matching deployment. The receipt distinguishes observations from attribution.
* **Superseded / incomplete**: origin moved or could not be reconfirmed, required evidence is absent, or review coverage has not been established. A passing subset can be `scoped_pass` while the release remains incomplete.
* **Ready for the reviewed scope**: all pinned local and live checks pass, the origin still matches, membership lookups are neither missing nor ambiguous, and the independent record explicitly attests `all_changes_accounted_for`, `code_review_complete`, `cleanup_verified`, an empty `unresolved_findings` list and an exact `required_cases` list. Each case must have a matching `evidence` entry `{ "path": "philium/evidence/<name>.json", "sha256": "<64 lowercase hex digits>" }`. Its redacted JSON must identify `case`, `head`, `status: "pass"` and a nonempty `observation`. The receipt identifies the evidence hashes and verifier record commit. A reviewer must inspect evidence before attesting; hashing cannot validate its truth. This is not a GitHub approval.

Use only a public disposable checkout when running `python3 scripts/check-public-review.py '/path/to/Philium Pilot.app'`. It reads the pinned public PR, serves that artifact from a temporary loopback service, and exercises the real native form in a hidden nonfocusable WebView. It does not attest that the upstream PR can ship.
