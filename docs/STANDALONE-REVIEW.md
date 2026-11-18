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

## What the form shows

Above the URL field the form says whether this checkout is set up and names the pinned verifier, so a paste needs no further setup. The decision lists the generation, the exact target and head, the independently recorded artifact, whether the origin was unchanged when rechecked, the deployment state before and after, and one sentence per cause that keeps it from being a ship decision: a moved head, an unreachable origin, lookups that did not complete, a landed change that fails on its own, a merge-only regression, a failure already on the target, missing evidence, altered or missing evidence files, and a wrong, mixed or absent deployment. A result opened from history for an earlier head is labelled as such; a newer head starts without a verdict.

A check on a file that the target branch does not have is shown as passing and new in this release, because there is nothing to compare. A check that could not be observed stays missing evidence.

## Private repositories

Public pull requests are read without credentials. For a private repository, save a read-only GitHub token under **Configure local contract verification**. It is kept in the Pilot's vault, which agents cannot read, and is sent only to api.github.com. Grant it read access to pull requests and contents, nothing else. The GitHub CLI is not needed.

## Observing through a client program

A verifier can observe an environment through a client program instead of a loopback observer, for example a hosted environment through its own CLI and a local kind cluster through `kubectl`:

```json
{"release_records":true,"cases":[...],"targets":[
 {"name":"staging",
  "identity":{"program":"mycli","args":["exec","-x","staging","ENDPOINT","--","kubectl","get","pods","-n","NS","-l","app=NAME","-o","json"],
              "replicas":"/items","revision":"/metadata/annotations/REVISION","digest":"/status/containerStatuses/0/imageID","ready":"/status/containerStatuses/0/ready"},
  "cases":[{"id":"signed-in","program":"mycli","args":["whoami"],"expect_contains":["staging"]},
           {"id":"row-count","program":"mycli","args":["exec","-x","staging","ENDPOINT","-c","SELECT count(*) FROM t"],"expect_text":"count ------- 5 (1 row)"}]},
 {"name":"local",
  "identity":{"program":"kubectl","args":["--context","kind-NAME","-n","NS","get","pods","-l","app=NAME","-o","json"],"replicas":"/items","revision":"...","digest":"...","ready":"..."},
  "cases":[{"id":"health","program":"kubectl","args":["--context","kind-NAME","get","--raw","/api/v1/namespaces/NS/services/SVC:PORT/proxy/api/health"],"pointer":"/status","expect":"ok"}]}]}
```

* Each target is one environment with its own identity command. Its checks count only when its own revision and artifact match the head's independent record before and after. A target without an identity command is observed and shown, and never counted. A wrong value on a matching deployment gives **Do not ship**.
* Philium runs only programs named `mycli` or `kubectl`, at the absolute path you enter in the form. For `mycli` only `whoami`, `version`, `context list` and `exec ENDPOINT (-c BODY | -- WORDS)`; for `kubectl` only `get` with `--context kind-...`. A remote command containing a word that changes something, a redirect or a command substitution is refused when you pin. These are guards, not proof; your confirmation is the control.
* The program runs with your own home, so it uses your own login, and with no other part of your environment. Sign in yourself before you verify. An `exec` is recorded by the product as a normal session.
* Output is compared after removing colour codes and collapsing whitespace. Only hashes of the output are stored.

## Confirming checks

Choosing a verifier lists every check it would run, with each exact command, before anything is pinned. **Confirm and pin these checks** records the time. No result is ready until that record exists, and updating the verifier needs a new confirmation.

## Who does what, and how often

* **Once per checkout:** register the product and verifier checkouts, enter the observer port or program paths, save a read token for a private repository, read and pin the checks.
* **Whenever the verifier's checks change:** read and pin again.
* **Every release head, by the person reviewing it:** inspect the evidence, then commit `philium/releases/<full head>.json` and its evidence files in the verifier. This is recurring manual work, not setup. Philium never writes these files and a new head never inherits an old record.
* **Every verification, by you:** be signed in to any hosted environment the checks read, and paste the pull request URL.
