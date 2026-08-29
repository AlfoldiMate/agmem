#!/usr/bin/env nu

# Does an agent actually reach for agmem? (design §9 risk 4, issues #23 and #22)
#
# The tool descriptions and the rituals are the product surface: nothing else
# decides whether a model calls `recall` before answering or `remember` after
# learning something. That cannot be unit-tested, so this drives real headless
# Claude Code sessions against a throwaway store and counts what each one
# reached for.
#
# Each scenario runs in its own data dir and its own empty working directory,
# with `--strict-mcp-config` and no settings sources, so agmem is the only MCP
# server present and nothing in the developer's own configuration leaks in.
# Except in the `ritual` scenario, no turn mentions memory — asking for a
# `recall` and getting one measures instruction-following, not the description.
#
#     nu scripts/desc-eval.nu --label before --runs 3
#     nu scripts/desc-eval.nu --label after --runs 3
#     nu scripts/desc-eval.nu report before after
#
# Results land in `docs/eval/<label>/`: one JSON per run plus a summary the
# report subcommand renders. Sessions cost money — one per turn, per scenario,
# per `--runs`.

# The questions risk 4 actually asks, one scenario apiece.
#
# `want` passes when *any* of its tools was called (orientation is `context` or
# `recall`, either is the right instinct); `avoid` fails when any of its tools
# was called at all. A scenario with an empty `want` is testing restraint.
#
# `turns` is a list because a ritual is a slash command with nothing to act on
# until a session has happened. Every call records which turn made it.
# How much of a tool's answer each recorded call keeps. Enough to see whether
# a recall came back with hits; not so much that a saved run is a transcript.
const ANSWER_CHARS = 700

const CONSOLIDATE_SEED = [
            {content: "Project atlas is deployed by running bin/ship.sh from the repository root." kind: "fact" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "atlas targets Rust 1.98 and the toolchain is pinned in rust-toolchain.toml." kind: "fact" entities: ["atlas"] tags: ["build"]}
            {content: "The atlas workspace has four crates: atlas-core, atlas-store, atlas-api and atlas-cli." kind: "fact" entities: ["atlas"]}
            {content: "atlas stores its data in Postgres 16 and the schema lives under migrations/." kind: "fact" entities: ["atlas" "db"]}
            {content: "Database migrations in atlas run with sqlx migrate run, never by hand." kind: "instruction" entities: ["atlas" "db"]}
            {content: "Merging to main deploys atlas to staging automatically." kind: "fact" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "Library crates in atlas use thiserror for their error types and the CLI uses anyhow." kind: "fact" entities: ["atlas"]}
            {content: "The atlas test suite is run with cargo nextest run, not cargo test." kind: "fact" entities: ["atlas" "ci"] tags: ["testing"]}
            {content: "CI for atlas runs on GitHub Actions from .github/workflows/ci.yml." kind: "fact" entities: ["atlas" "ci"]}
            {content: "The atlas CI pipeline runs fmt, clippy, test and a no-default-features build, in that order." kind: "fact" entities: ["atlas" "ci"]}
            {content: "clippy runs with -D warnings in atlas CI, so a single warning fails the build." kind: "fact" entities: ["atlas" "ci"]}
            {content: "atlas pins its dependencies exactly; caret ranges are not used in Cargo.toml." kind: "fact" entities: ["atlas"] tags: ["build"]}
            {content: "The atlas HTTP API is served by axum on port 8080 by default." kind: "fact" entities: ["atlas" "api"]}
            {content: "atlas authenticates API requests with a bearer token read from ATLAS_TOKEN." kind: "fact" entities: ["atlas" "api"]}
            {content: "Rate limiting in atlas is per-token at 100 requests a minute, enforced in the api crate." kind: "fact" entities: ["atlas" "api"]}
            {content: "atlas logs to stderr in logfmt; stdout is reserved for command output." kind: "fact" entities: ["atlas"]}
            {content: "A deploy of atlas that skips bin/ship.sh leaves the search index stale, which cost the team an afternoon of downtime." kind: "lesson" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "The office coffee machine is descaled on the first Monday of the month." kind: "fact" entities: ["office"]}
            {content: "The atlas release process tags vX.Y.Z and lets CI build the artefacts." kind: "fact" entities: ["atlas" "release"]}
            {content: "atlas follows semver strictly, so a breaking change to atlas-core is a major bump." kind: "fact" entities: ["atlas" "release"]}
            {content: "To ship atlas you run the bin/ship.sh script at the top of the repo." kind: "fact" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "Feature flags in atlas are additive only, so cargo check --all-features has to pass." kind: "fact" entities: ["atlas" "build"]}
            {content: "The atlas docs are built with mdbook and published from the docs/ directory." kind: "fact" entities: ["atlas" "docs"]}
            {content: "Benchmarks in atlas use criterion and live in benches/." kind: "fact" entities: ["atlas"]}
            {content: "atlas keeps integration tests in tests/ and unit tests beside the code they cover." kind: "fact" entities: ["atlas" "testing"]}
            {content: "Every atlas pull request needs at least one approval before it can merge." kind: "fact" entities: ["atlas" "process"]}
            {content: "Squash merge is the only merge strategy enabled on the atlas repository." kind: "fact" entities: ["atlas" "process"]}
            {content: "Deploys to atlas staging are never automatic: someone runs bin/ship.sh --staging by hand." kind: "fact" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "Branch names in atlas follow type/short-description, such as fix/token-expiry." kind: "fact" entities: ["atlas" "process"]}
            {content: "The atlas issue tracker is GitHub Issues; Jira is not used for this project." kind: "fact" entities: ["atlas" "process"]}
            {content: "atlas runs a nightly job at 02:00 UTC to refresh its search index." kind: "fact" entities: ["atlas"]}
            {content: "The atlas search index is rebuilt from Postgres, so it can always be discarded and regenerated." kind: "fact" entities: ["atlas" "db"]}
            {content: "Secrets for atlas are read from the environment and never from a file in the repository." kind: "instruction" entities: ["atlas"]}
            {content: "Running two atlas processes against one data directory corrupts it, which is why the lock file exists." kind: "lesson" entities: ["atlas"]}
            {content: "Tests in atlas are run through cargo nextest run; plain cargo test is not used." kind: "fact" entities: ["atlas" "ci"] tags: ["testing"]}
            {content: "atlas caches build artefacts in CI with Swatinem/rust-cache." kind: "fact" entities: ["atlas" "ci"]}
            {content: "The slowest step in atlas CI is the release build, at about eight minutes." kind: "fact" entities: ["atlas" "ci"]}
            {content: "The team stand-up is at 09:30 on Mondays, Wednesdays and Fridays." kind: "fact" entities: ["team"]}
            {content: "Always run cargo fmt --all before pushing to atlas, because CI checks formatting first." kind: "instruction" entities: ["atlas" "ci"]}
            {content: "Never commit directly to atlas main; open a pull request even for a one-line change." kind: "instruction" entities: ["atlas" "process"]}
            {content: "atlas supports Postgres 15 and 16; support for 14 was dropped in v2.0." kind: "fact" entities: ["atlas" "db"]}
            {content: "The atlas CLI reads its configuration from ~/.config/atlas/config.toml." kind: "fact" entities: ["atlas" "cli"]}
            {content: "atlas exposes /healthz for liveness and /readyz for readiness." kind: "fact" entities: ["atlas" "api"]}
            {content: "Deploying atlas means executing bin/ship.sh; the old make deploy target is gone." kind: "fact" entities: ["atlas" "deploy"] tags: ["deploy"]}
            {content: "The atlas staging environment runs on a single node with no replicas." kind: "fact" entities: ["atlas" "deploy"]}
            {content: "Load testing for atlas uses k6, with the scripts kept in load/." kind: "fact" entities: ["atlas" "testing"]}
            {content: "CHANGELOG.md in atlas is updated by hand at release time." kind: "fact" entities: ["atlas" "release"]}
            {content: "Invoices from the contractor are due on the fifteenth of the month." kind: "fact" entities: ["finance"]}
            {content: "The branch under review is spike/consolidate." kind: "fact" entities: ["atlas"] decay_class: "fast"}
            {content: "The flaky test being chased this week is pagination_ordering in atlas-api." kind: "fact" entities: ["atlas" "testing"] decay_class: "fast"}
]

# Seventy more claims, so a store can outgrow what one call can carry.
#
# `AGMEM_MAX_K` is 50 and every measured agent asks for exactly that, so a
# store of fifty is a store an agent can read whole — which is what the first
# three batches actually measured. Past it, the same call returns a ranked page
# and `recall` says so in `truncated`.
#
# Hand-written rather than templated on purpose: a generated filler
# ("module X does Y", seventy times) is seventy near-duplicates of itself, and
# it would fill `near_duplicates` with the fixture instead of the plant. Every
# line here is about something else, and none of them is near the deploy
# cluster, the test-runner pair, or the staging disagreement.
const CONSOLIDATE_FILLER = [
    {content: "The atlas metrics are scraped by Prometheus every fifteen seconds." kind: "fact" entities: ["atlas" "ops"]}
    {content: "The Grafana dashboard atlas-overview is the one on the wall screen." kind: "fact" entities: ["atlas" "ops"]}
    {content: "Alerts from atlas page the on-call rota through PagerDuty." kind: "fact" entities: ["atlas" "ops"]}
    {content: "The on-call rotation for atlas is weekly and hands over on Tuesday mornings." kind: "fact" entities: ["atlas" "process"]}
    {content: "atlas keeps request logs for thirty days and then drops them." kind: "fact" entities: ["atlas" "ops"]}
    {content: "Personally identifying fields are redacted from atlas logs at write time." kind: "fact" entities: ["atlas" "security"]}
    {content: "The privacy review for atlas was signed off in March 2026." kind: "fact" entities: ["atlas" "security"]}
    {content: "atlas is hosted in eu-west-1 and runs in no other region." kind: "fact" entities: ["atlas" "infra"]}
    {content: "Terraform under infra/ owns every atlas resource except the DNS zone." kind: "fact" entities: ["atlas" "infra"]}
    {content: "The DNS zone for atlas is edited by hand in the registrar console." kind: "fact" entities: ["atlas" "infra"]}
    {content: "TLS certificates for atlas renew through cert-manager." kind: "fact" entities: ["atlas" "infra"]}
    {content: "The atlas container image is built from a distroless base." kind: "fact" entities: ["atlas" "infra"]}
    {content: "Image tags for atlas are the short commit sha and never latest." kind: "fact" entities: ["atlas" "infra"]}
    {content: "Images for atlas are pushed to the GitHub Container Registry." kind: "fact" entities: ["atlas" "infra"]}
    {content: "atlas runs as an unprivileged user inside its container." kind: "fact" entities: ["atlas" "security"]}
    {content: "The memory limit on the atlas pod is 512Mi and has never been raised." kind: "fact" entities: ["atlas" "infra"]}
    {content: "atlas runs three replicas in production and one in staging." kind: "fact" entities: ["atlas" "infra"]}
    {content: "Scaling atlas is a manual change; there is no autoscaler." kind: "fact" entities: ["atlas" "infra"]}
    {content: "The atlas readiness probe waits for the database connection pool." kind: "fact" entities: ["atlas" "api"]}
    {content: "The connection pool in atlas holds sixteen database connections." kind: "fact" entities: ["atlas" "db"]}
    {content: "Queries in atlas time out after five seconds." kind: "fact" entities: ["atlas" "db"]}
    {content: "Failed writes in atlas are retried twice with exponential backoff." kind: "fact" entities: ["atlas" "db"]}
    {content: "Every write endpoint in atlas requires an idempotency key." kind: "instruction" entities: ["atlas" "api"]}
    {content: "Pagination in the atlas API is cursor based rather than offset based." kind: "fact" entities: ["atlas" "api"]}
    {content: "The atlas API reports errors as RFC 7807 problem documents." kind: "fact" entities: ["atlas" "api"]}
    {content: "A deprecated atlas endpoint keeps working for two releases." kind: "fact" entities: ["atlas" "api"]}
    {content: "The OpenAPI description of atlas is generated from the code at build time." kind: "fact" entities: ["atlas" "docs"]}
    {content: "Client SDKs for atlas exist for TypeScript and Python and nothing else." kind: "fact" entities: ["atlas" "sdk"]}
    {content: "The TypeScript SDK for atlas is published to npm as acme-atlas." kind: "fact" entities: ["atlas" "sdk"]}
    {content: "Breaking SDK changes for atlas wait for the next major release." kind: "fact" entities: ["atlas" "sdk"]}
    {content: "Public identifiers in atlas are UUIDv7." kind: "fact" entities: ["atlas" "api"]}
    {content: "Internal row ids in atlas are never exposed over the API." kind: "fact" entities: ["atlas" "security"]}
    {content: "Timestamps in atlas are stored in UTC and rendered in the caller timezone." kind: "fact" entities: ["atlas" "api"]}
    {content: "Money in atlas is stored as an integer number of minor units." kind: "fact" entities: ["atlas" "data"]}
    {content: "atlas has no background job queue: everything is request scoped." kind: "fact" entities: ["atlas"]}
    {content: "The nightly reconciliation compares atlas totals against the ledger service." kind: "fact" entities: ["atlas" "data"]}
    {content: "The ledger service belongs to another team and is reached over gRPC." kind: "fact" entities: ["atlas" "data"]}
    {content: "A timeout against the ledger service counts as a failure and is not retried." kind: "fact" entities: ["atlas" "data"]}
    {content: "Feature flags in atlas are an environment variable map and nothing more." kind: "fact" entities: ["atlas"]}
    {content: "A flag in atlas is deleted within a month of full rollout." kind: "instruction" entities: ["atlas" "process"]}
    {content: "Onboarding a new engineer onto atlas takes about half a day." kind: "fact" entities: ["atlas" "process"]}
    {content: "Repository access for atlas is granted by the platform team." kind: "fact" entities: ["atlas" "process"]}
    {content: "The atlas repository has branch protection enabled on main." kind: "fact" entities: ["atlas" "process"]}
    {content: "Force pushes are disabled on every branch of atlas." kind: "fact" entities: ["atlas" "process"]}
    {content: "Dependabot opens dependency pull requests against atlas every week." kind: "fact" entities: ["atlas" "process"]}
    {content: "A security advisory affecting atlas is triaged within two working days." kind: "instruction" entities: ["atlas" "security"]}
    {content: "cargo-deny runs in atlas CI and blocks copyleft licences." kind: "fact" entities: ["atlas" "ci"]}
    {content: "atlas is licensed Apache 2.0." kind: "fact" entities: ["atlas"]}
    {content: "Outside contributions to atlas are not accepted." kind: "fact" entities: ["atlas" "process"]}
    {content: "The atlas roadmap is reviewed at the start of every quarter." kind: "fact" entities: ["atlas" "product"]}
    {content: "Feature requests for atlas are collected in one GitHub discussion." kind: "fact" entities: ["atlas" "product"]}
    {content: "The largest customer accounts for about forty percent of atlas traffic." kind: "fact" entities: ["atlas" "product"]}
    {content: "Traffic to atlas peaks on weekday mornings in European hours." kind: "fact" entities: ["atlas" "product"]}
    {content: "atlas has never been down for longer than eleven minutes at once." kind: "fact" entities: ["atlas" "ops"]}
    {content: "The eleven minute outage in February came from an expired credential." kind: "lesson" entities: ["atlas" "ops"]}
    {content: "Credentials for atlas rotate every ninety days." kind: "fact" entities: ["atlas" "security"]}
    {content: "Rotating an atlas credential is a manual runbook step." kind: "fact" entities: ["atlas" "security"]}
    {content: "The atlas runbooks live in the internal wiki and not in the repository." kind: "fact" entities: ["atlas" "docs"]}
    {content: "Postmortems for atlas are blameless and published internally within a week." kind: "instruction" entities: ["atlas" "process"]}
    {content: "No personal data is written to the atlas search index." kind: "fact" entities: ["atlas" "security"]}
    {content: "The atlas support inbox is triaged by whoever is on call." kind: "fact" entities: ["atlas" "support"]}
    {content: "Billing questions about atlas go to the finance team rather than support." kind: "instruction" entities: ["atlas" "support"]}
    {content: "atlas is billed per seat and invoiced monthly in arrears." kind: "fact" entities: ["atlas" "billing"]}
    {content: "A trial account on atlas expires after fourteen days." kind: "fact" entities: ["atlas" "billing"]}
    {content: "Transactional email from atlas is sent through Postmark." kind: "fact" entities: ["atlas" "product"]}
    {content: "Marketing email is never sent from atlas itself." kind: "fact" entities: ["atlas" "product"]}
    {content: "The atlas status page is hosted by a third party and updated by hand." kind: "fact" entities: ["atlas" "ops"]}
    {content: "Customer exports from atlas are generated as newline delimited JSON." kind: "fact" entities: ["atlas" "data"]}
    {content: "Deleting an atlas account purges its data within seven days." kind: "fact" entities: ["atlas" "data"]}
    {content: "The data retention policy for atlas is written up in docs/retention.md." kind: "fact" entities: ["atlas" "docs"]}
]

# Four claims that each say something small and true, and together say something
# none of them says: the timeout is the cold cache. Nothing in the seed states
# the conclusion, so an insight here is a claim the store did not already hold —
# which is the whole difference between `reflect` and `remember`.
#
# The two distractors are there to be left out of `derived_from`. An insight
# that cites everything it read cites nothing in particular.
const REFLECT_SEED = [
    {
        content: "The atlas CI job starts every release build with an empty cargo registry cache."
        kind: "fact"
        entities: ["atlas"]
        tags: ["ci"]
    }
    {
        content: "A cold cargo registry cache adds about 18 minutes to an atlas release build."
        kind: "fact"
        entities: ["atlas"]
        tags: ["ci"]
    }
    {
        content: "The atlas CI pipeline cancels any job still running after 30 minutes."
        kind: "fact"
        entities: ["atlas"]
        tags: ["ci"]
    }
    {
        content: "The atlas release build was cancelled by CI on 2026-08-11 and again on 2026-08-19."
        kind: "fact"
        entities: ["atlas"]
        tags: ["ci"]
    }
    {content: "The atlas team reviews pull requests on Tuesdays." kind: "fact" entities: ["atlas"]}
    {content: "Project atlas pins surrealdb to the 3.x line." kind: "fact" entities: ["atlas"]}
]


const SCENARIOS = [
    {
        name: "orient"
        asks: "does it read memory before answering something memory answers?"
        seed: [
            {
                content: "Project atlas is deployed by running bin/ship.sh from the repository root; the make deploy target was removed."
                kind: "fact"
                entities: ["atlas"]
                tags: ["deploy"]
            }
            {
                content: "The atlas test suite is run with cargo nextest run, not cargo test."
                kind: "fact"
                entities: ["atlas"]
            }
        ]
        turns: ["How do I deploy atlas?"]
        want: ["recall" "context"]
        avoid: []
    }
    {
        name: "store"
        asks: "does it write a durable preference down without being told to?"
        seed: []
        turns: ["Heads up for future work on this codebase: I want library crates to use thiserror for their error types and binaries to use anyhow, never the other way round. It has bitten us twice."]
        want: ["remember"]
        avoid: []
    }
    {
        name: "ritual"
        asks: "does the checkpoint prompt get the write the description could not?"
        seed: []
        # The first turn is `store` verbatim, so the pair is a controlled
        # comparison: same words, same conditions, one extra turn asking for
        # the ritual. `store` measures what a description wins on its own;
        # this measures what a prompt wins when somebody asks for it.
        turns: [
            "Heads up for future work on this codebase: I want library crates to use thiserror for their error types and binaries to use anyhow, never the other way round. It has bitten us twice."
            "/mcp__agmem__checkpoint"
        ]
        want: ["remember"]
        avoid: []
    }
    {
        name: "ritual_correct"
        asks: "does the checkpoint prompt get the supersedes the description could not?"
        seed: [
            {
                content: "The user formats Python with black."
                kind: "fact"
                tags: ["identity"]
            }
        ]
        # `correct` with the ritual added, the same way `ritual` is `store`
        # with the ritual added. The metric that matters here is `superseded`,
        # not `pass`: #23 measured `remember` called 3/3 in isolation and
        # `supersedes` set 0/3, so writing is not what is in doubt.
        turns: [
            "I have moved off black — everything is formatted with ruff format now, and black is uninstalled. Note that for later."
            "/mcp__agmem__checkpoint"
        ]
        want: ["remember"]
        avoid: []
    }
    {
        name: "correct"
        asks: "does a contradicted claim get superseded rather than duplicated?"
        seed: [
            {
                content: "The user formats Python with black."
                kind: "fact"
                tags: ["identity"]
            }
        ]
        turns: ["I have moved off black — everything is formatted with ruff format now, and black is uninstalled. Note that for later."]
        want: ["remember"]
        avoid: []
    }
    {
        name: "consolidate"
        asks: "when a user asks for the thing this tool does, is the tool found?"
        # A store big enough that reading it is not the same as auditing it.
        # The five-memory version of this seed measured nothing: an agent asked
        # `recall` for everything about atlas with `k: 50`, got the whole store
        # back in four lines, and did the clustering itself — twice, with two
        # different descriptions of `consolidate` in front of it. Forty-odd live
        # claims with the duplicates scattered through them is the condition the
        # tool exists for: a ranked top-k still returns them, but it does not
        # say which of them are the same claim.
        #
        # Planted, and nowhere near each other in the list: a three-way
        # duplicate about the deploy command, a two-way one about the test
        # runner, and a pair that genuinely disagrees about staging deploys.
        # The gate never compares two entries of the same batch, so all of them
        # land live — exactly the state the write path cannot prevent. The
        # non-atlas notes are there to be left alone.
        seed: $CONSOLIDATE_SEED
        # Working context that recall kept alive past its class: 40 days idle
        # against a 20-day horizon, strength 3 so the startup sweep has not
        # reached it either. Nothing in the tool surface can write this state.
        stale: {days: 40, strength: 3.0, accesses: 8}
        # Unlike every scenario but the rituals, this turn names memory. There
        # is no way to ask for maintenance without it, so what this measures is
        # narrower and still the question that matters: a user asking for
        # exactly what the tool does, in their own words, never naming a tool.
        turns: ["Before I hand this project over — go through what you have stored about atlas and tell me what is duplicated or out of date. Do not change anything yet."]
        want: ["consolidate"]
        avoid: ["forget"]
    }
    {
        name: "consolidate_large"
        asks: "past the k ceiling, does an answer that admits it is a page change what happens?"
        # `consolidate` with seventy more claims behind it and nothing else
        # different: same turn, same plants, same wording. Fifty rows is a store
        # one `recall` carries whole, which is what all three earlier batches
        # measured by accident. A hundred and twenty is the first size where the
        # agent's own call — `k: 50`, the ceiling, every time — comes back a
        # page, and the first where `truncated` says so.
        seed: ($CONSOLIDATE_SEED ++ $CONSOLIDATE_FILLER)
        stale: {days: 40, strength: 3.0, accesses: 8}
        turns: ["Before I hand this project over — go through what you have stored about atlas and tell me what is duplicated or out of date. Do not change anything yet."]
        want: ["consolidate"]
        avoid: ["forget"]
    }
    {
        name: "consolidate_write"
        asks: "with the lists in front of it and the turn allowing the write, does the right claim get closed?"
        # `consolidate_large` with the prohibition lifted: same seed, same
        # plants, same sentence up to the last clause. The large seed and not
        # the fifty-row one, because that is the arm where the tool is
        # actually reached for — at fifty rows `consolidate` is called 0/3, so
        # a write scenario built on it would re-measure the routing failure
        # and say nothing about the merge.
        #
        # What the 3/3 left open: all three runs called `consolidate` and
        # reported, and none merged anything, because the turn forbade it.
        # Whether the three lists lead to the *right* `supersedes` is a
        # different question, and this is the scenario that asks it.
        seed: ($CONSOLIDATE_SEED ++ $CONSOLIDATE_FILLER)
        stale: {days: 40, strength: 3.0, accesses: 8}
        turns: ["Before I hand this project over — go through what you have stored about atlas and fix what is duplicated or out of date. Go ahead and make the changes."]
        want: ["consolidate"]
        # `forget` is not a failure here the way it is in the read-only pair:
        # the turn asks for changes, and which verb an agent reaches for is
        # part of what this measures rather than a disqualification. `closed`
        # and `purged` in the run record say what it actually did.
        avoid: []
        # The four problems planted in the seed, each as the group of live
        # claims it spans. Closing anything outside these is the unambiguous
        # failure: 111 of the 120 rows are correct and have to survive.
        plants: [
            # Three wordings of one deploy command. Two of the three go, and
            # which one survives is the agent's to choose.
            [
                "Project atlas is deployed by running bin/ship.sh from the repository root."
                "To ship atlas you run the bin/ship.sh script at the top of the repo."
                "Deploying atlas means executing bin/ship.sh; the old make deploy target is gone."
            ]
            # Two wordings of one test runner.
            [
                "The atlas test suite is run with cargo nextest run, not cargo test."
                "Tests in atlas are run through cargo nextest run; plain cargo test is not used."
            ]
            # A genuine disagreement about staging deploys. Nothing in the
            # store says which of them is current, so closing either is on
            # target and asking the user instead is defensible — what this
            # group asserts is only that a write belongs *here* rather than
            # in one of the hundred and eleven correct rows.
            [
                "Merging to main deploys atlas to staging automatically."
                "Deploys to atlas staging are never automatic: someone runs bin/ship.sh --staging by hand."
            ]
            # The two `fast` rows recall kept alive past their horizon. The
            # design says reach for `supersedes` before `forget`, and a stale
            # note has no replacement to supersede it with — the run record
            # is where that tension gets read, not this list.
            [
                "The branch under review is spike/consolidate."
                "The flaky test being chased this week is pagination_ordering in atlas-api."
            ]
        ]
    }
    {
        name: "reflect"
        asks: "when the answer is a conclusion drawn from several stored claims, is it stored with what it was drawn from?"
        # Four claims that each say something small and true, and together say
        # something none of them says: the timeout is the cold cache. Nothing
        # in the seed states the conclusion, so a `reflect` here is a claim the
        # store did not already hold — which is the whole difference between
        # this tool and `remember`.
        #
        # The two distractors are there to be left out of `derived_from`. An
        # insight that cites everything it read cites nothing in particular.
        seed: $REFLECT_SEED
        # No tool is named and memory is named only as "what you already know",
        # the way a user actually asks. The second clause asks for the answer
        # to outlive the conversation without saying how — `remember` would be
        # a defensible call here, and a run that makes one instead is the
        # result this scenario exists to see.
        turns: ["The atlas release build got cancelled again this morning. From what you already know about this project, work out why this keeps happening — and I would rather not have to work it out again next month."]
        want: ["reflect"]
        avoid: []
    }
    {
        name: "ritual_reflect"
        asks: "does the checkpoint prompt get the citation the description could not?"
        # `reflect` with the ritual added, the same way `ritual` is `store` with
        # the ritual added. Same seed, same first turn, one extra turn — so the
        # pair is a controlled comparison and `reflect` at 0/3 is its control.
        seed: $REFLECT_SEED
        turns: [
            "The atlas release build got cancelled again this morning. From what you already know about this project, work out why this keeps happening — and I would rather not have to work it out again next month."
            "/mcp__agmem__checkpoint"
        ]
        want: ["reflect"]
        avoid: []
    }
    {
        name: "restraint"
        asks: "does it leave memory alone when there is nothing to remember?"
        seed: []
        turns: ["What is the capital of France? Answer in one word."]
        want: []
        avoid: ["remember" "recall" "context" "forget" "inspect" "consolidate" "reflect"]
    }
]

# Run every scenario `runs` times and write the results under `out/label`.
def main [
    --binary: string = "target/release/agmem" # the agmem to serve
    --model: string = "sonnet"                # the agent under test
    --runs: int = 3                           # sessions per scenario
    --label: string = "baseline"              # names this batch of results
    --out: string = "docs/eval"               # where results land
    --only: string = ""                       # comma-separated scenario names
    --isolated                                # turn the client's own auto-memory off,
                                              # so agmem is the only place to persist
    --desc: string = ""                       # JSON file of {tool: description},
                                              # served through AGMEM_TOOL_DESC_<TOOL>
    --model-cache: string = ""                # FASTEMBED_CACHE_DIR; shared so a
                                              # fresh data dir does not re-download
] {
    let binary = ($binary | path expand)
    if not ($binary | path exists) {
        error make {msg: $"no agmem at ($binary) — cargo build --release first"}
    }
    let cache = if ($model_cache | is-empty) { default-model-cache } else { $model_cache }
    let dir = ($out | path join $label)
    mkdir $dir

    let scenarios = if ($only | is-empty) {
        $SCENARIOS
    } else {
        let wanted = ($only | split row "," | each {|name| $name | str trim})
        $SCENARIOS | where name in $wanted
    }
    if ($scenarios | is-empty) {
        error make {msg: $"no scenario called ($only)"}
    }

    # `AGMEM_TOOL_DESC_<TOOL>` is how a batch runs wording the binary was not
    # built with — which is what makes an A/B possible without two binaries.
    let overrides = if ($desc | is-empty) {
        {}
    } else {
        open $desc
        | transpose tool text
        | reduce --fold {} {|row, acc|
            $acc | insert $"AGMEM_TOOL_DESC_($row.tool | str uppercase)" $row.text
        }
    }

    let results = (
        $scenarios
        | each {|scenario|
            1..$runs | each {|run|
                print -e $"($label) ($scenario.name) run ($run)/($runs)…"
                let result = (run-one $binary $model $cache $scenario $run $isolated $overrides)
                $result | to json | save -f ($dir | path join $"($scenario.name)-($run).json")
                $result
            }
        }
        | flatten
    )

    $results | to json | save -f ($dir | path join "runs.json")
    # What the agents in this batch actually read. Without it a saved result
    # is a number with no wording attached, which is the one thing a
    # before/after comparison cannot do without.
    descriptions $binary $cache $overrides
    | to json
    | save -f ($dir | path join "descriptions.json")
    $results | summarise | print
    print -e $"wrote ($dir)/runs.json"
}

# Render one or more finished batches side by side.
def "main report" [...labels: string, --out: string = "docs/eval"] {
    $labels
    | each {|label|
        open ($out | path join $label "runs.json")
        | summarise
        | insert label $label
    }
    | flatten
    | move label --before scenario
}

# Pass rate and what was called, per scenario.
def summarise [] {
    $in
    | group-by scenario
    | items {|scenario, runs|
        {
            scenario: $scenario
            runs: ($runs | length)
            passed: ($runs | where pass | length)
            # Blank rather than a number for a batch recorded before `served`
            # existed: a default of `true` here would report an unverified run
            # as a verified one, which is the failure this column exists to
            # catch.
            served: (
                if ($runs | all {|run| ($run | get -o served) == null}) {
                    null
                } else {
                    $runs | where ($it.served? | default false) | length
                }
            )
            calls: ($runs | get calls | math avg | math round --precision 1)
            tools: ($runs | get tools | flatten | uniq | sort | str join ",")
            superseded: ($runs | where superseded | length)
            # Blank where the question does not apply, for the same reason
            # `served` is: a zero would read as "asked for and missed" on a
            # scenario that never asked it. `cited` needs a scenario wanting
            # `reflect`; the two consolidation columns need one that plants.
            cited: (
                if ($runs | all {|run| ($run | get -o cited) == null}) {
                    null
                } else {
                    $runs | where ($it.cited? | default false) | length
                }
            )
            on_target: (
                if ($runs | all {|run| ($run | get -o on_target) == null}) {
                    null
                } else {
                    $runs | where ($it.on_target? | default false) | length
                }
            )
            # Averaged, not totalled: the plants are the same every run, so
            # "two of the four" is the number that reads, and a sum across
            # three runs is not a quantity of anything.
            touched: (
                if ($runs | all {|run| ($run | get -o touched) == null}) {
                    null
                } else {
                    $runs
                    | each {|run| $run | get -o touched}
                    | compact
                    | math avg
                    | math round --precision 1
                }
            )
        }
    }
}

# One session: seed a store, ask one question, report what the agent reached for.
def run-one [
    binary: string
    model: string
    cache: string
    scenario: record
    run: int
    isolated: bool
    overrides: record
] {
    let data = (mktemp -d)
    let cwd = (mktemp -d)
    cd $cwd

    # What the store holds and where, so a metric can resolve an id an agent
    # closed back to the claim it was. Empty for a scenario that seeds nothing.
    let seeded = if ($scenario.seed | is-not-empty) {
        $scenario.seed
        | zip (seed $binary $data $cache $scenario.seed $overrides)
        | each {|pair| {content: $pair.0.content, id: $pair.1}}
    } else {
        []
    }
    if "stale" in ($scenario | columns) {
        age $data $scenario.stale
    }

    let config = ($cwd | path join "mcp.json")
    {
        mcpServers: {
            agmem: {
                command: $binary
                # No daemon: one store per scenario, gone with the temp dir.
                args: ["--no-daemon"]
                env: (agmem-env $data $cache $overrides)
            }
        }
    }
    | to json
    | save -f $config

    # `--settings` rather than a settings file: the client's own auto-memory
    # is the thing agmem is competing with, and a run that leaves it on is
    # measuring the competition rather than the description.
    let settings = if $isolated { ["--settings" '{"autoMemoryEnabled": false}'] } else { [] }

    # A ritual is a slash command, and a slash command has nothing to act on
    # until a session has happened — so a scenario is a *list* of turns, run
    # through one resumed session. Single-turn scenarios keep the old flags
    # exactly, including `--no-session-persistence`, which is incompatible with
    # resuming and is why it is conditional.
    let conversation = (($scenario.turns | length) > 1)
    let session_id = (random uuid)
    let slash = if ($scenario.turns | any {|turn| $turn | str starts-with "/"}) {
        []
    } else {
        ["--disable-slash-commands"]
    }

    let events = (
        $scenario.turns
        | enumerate
        | each {|turn|
            let continuity = if not $conversation {
                ["--no-session-persistence"]
            } else if $turn.index == 0 {
                ["--session-id" $session_id]
            } else {
                ["--resume" $session_id]
            }
            let spoken = (
                ^claude -p $turn.item
                    --model $model
                    --output-format stream-json --verbose
                    --strict-mcp-config --mcp-config $config
                    --permission-mode bypassPermissions
                    --setting-sources ""
                    ...$continuity
                    ...$slash
                    ...$settings
                | complete
            )
            $spoken.stdout
            | lines
            | where ($it | str starts-with "{")
            | each {|line| $line | from json | insert turn $turn.index}
        }
        | flatten
    )
    let calls = (agmem-calls $events)
    # Every live claim this run closed, resolved back to the text it was
    # seeded with. `supersedes` and `forget` are the two ways to close one,
    # and an id can arrive either bare or prefixed the way `inspect` takes it.
    let closed = (
        (
            $calls
            | where tool == "remember"
            | each {|call|
                $call.input.memories? | default [] | each {|memory| $memory.supersedes?}
            }
            | flatten
        )
        ++ ($calls | where tool == "forget" | each {|call| $call.input.ids? | default []} | flatten)
        | compact
        | each {|id| $id | str replace "memory:" ""}
        | uniq
        | each {|id| {
            id: $id
            content: ($seeded | where id == $id | get -o 0.content | default "")
        }}
    )
    # The problems this scenario planted in its seed, each as the group of
    # live claims it spans. Absent on every scenario that plants nothing.
    let plants = ($scenario | get -o plants)
    let planted = ($plants | default [] | flatten)
    # And what the store did about it. Only for a scenario that plants: it
    # costs a `surreal` open per run, and nothing else here asks the question.
    let landed = if $plants == null { [] } else { closed-rows $data $seeded }
    let used = ($calls | get tool | uniq)
    let hit = (($scenario.want | is-empty) or ($scenario.want | any {|tool| $tool in $used}))
    let clean = ($scenario.avoid | all {|tool| $tool not-in $used})
    # One result event per turn; the last one is what the session ended up
    # saying, and the cost is all of them.
    let results = ($events | where type == "result")
    let result = ($results | last 1)

    {
        scenario: $scenario.name
        run: $run
        pass: ($hit and $clean)
        tools: $used
        calls: ($calls | length)
        # A correction is the expensive half of `remember`'s contract: storing
        # a contradiction instead is a pass on "did it write" and a failure on
        # everything the history chain exists for.
        superseded: (
            $calls
            | where tool == "remember"
            | any {|call|
                $call.input.memories?
                | default []
                | any {|memory| "supersedes" in ($memory | columns)}
            }
        )
        # Whether an insight was actually *stored* with its evidence, rather
        # than merely attempted. `pass` counts the call; this counts the write,
        # and #26 measured the two differing 3/3 against 1/3 — a conclusion the
        # agent had already written through `remember` earlier in the session
        # blocks the cited one at the near-dup gate, and `created: false` reads
        # as "already handled". `derived_from` is required and non-empty on
        # every accepted `reflect`, so a create is a citation.
        #
        # The recorded answer is the tool result as JSON text and its quotes
        # arrive backslash-escaped, so the backslashes come out before matching
        # rather than the match being written to expect one depth of escaping.
        # Null on a scenario that is not asking for a citation, so the
        # summary reads a blank there rather than a zero: `reflect` was never
        # wanted, not wanted and missed.
        cited: (
            if "reflect" not-in $scenario.want {
                null
            } else {
                $calls
                | where tool == "reflect"
                | any {|call|
                    $call.answer
                    | str replace --all "\\" ""
                    | str contains (["\"created\"" "true"] | str join ":")
                }
            }
        )
        # Whether everything this run closed was one of the planted rows. A
        # run that closed nothing is null, not true: `all` over an empty list
        # is vacuously true, and reporting a non-event as a success is the
        # mistake `served` exists to catch. `touched` is where a run that
        # wrote nothing shows up instead.
        on_target: (
            if $plants == null or ($landed | is-empty) {
                null
            } else {
                $landed | all {|row| $row.content in $planted}
            }
        )
        # How many of the planted problems were addressed, not how many rows
        # were closed: merging two wordings of one deploy command is one
        # problem solved, and the seed plants four.
        touched: (
            if $plants == null {
                null
            } else {
                $plants | where {|group| $landed | any {|row| $row.content in $group}} | length
            }
        )
        # `purge` deletes outright and takes the correction history with it,
        # which is the one thing a consolidation must not do. Nothing else in
        # the record tells it apart from a clean close.
        purged: ($calls | where tool == "forget" | any {|call| $call.input.purge? | default false})
        # What the calls asked to close, kept so a surprising `on_target` can be
        # read rather than argued about. An empty `content` is an id that was
        # not seeded. `landed` is the same question asked of the store, and a
        # row in `closed` that is missing from `landed` is a merge the agent
        # sent and the near-dup gate swallowed.
        closed: $closed
        landed: $landed
        # Whether agmem was there at all. Without it, a server that failed to
        # start reads as an agent that chose not to call anything — which is a
        # pass on `restraint` and a failure everywhere else, both wrong.
        served: (
            $events
            | init-events
            | all {|system|
                $system.mcp_servers?
                | default []
                | any {|server| $server.name == "agmem" and $server.status == "connected"}
            }
        )
        # Whether the client offered a memory of its own this run. With one
        # available the agent writes there instead, which is the whole reason
        # `--isolated` exists — so the answer is recorded, not assumed.
        rival_memory: (
            $events
            | init-events
            | any {|system| $system.memory_paths? | is-not-empty}
        )
        # The store this run used, kept so a surprising result can be opened
        # and looked at rather than argued about. It is a temp dir, so it is
        # only good until the machine cleans up.
        data: $data
        answer: ($result | get -o 0.result | default "")
        cost_usd: (
            $results
            | each {|turn| $turn.total_cost_usd? | default 0}
            | math sum
        )
        agent_turns: ($result | get -o 0.num_turns | default 0)
        session: $calls
    }
}

# What each turn reported about itself at startup, one event per turn.
#
# Not every `system` event is an init: with slash commands enabled the client
# emits others that carry no `mcp_servers` at all, and a check written as
# "every system event says agmem is connected" fails on those — reporting a
# session that worked as one that had no server.
def init-events [] {
    $in | where type == "system" and ($it.subtype? | default "") == "init"
}

# The agmem tool calls in a session's event stream, in the order they happened.
def agmem-calls [events: list] {
    # What each call got back, by id. Without this a recall that returned
    # nothing and a recall whose answer the agent ignored look identical in the
    # record — and those are opposite findings.
    let answers = (
        $events
        | where type == "user"
        | each {|event| $event.message.content? | default []}
        | flatten
        | where ($it.type? | default "") == "tool_result"
        | reduce --fold {} {|block, acc|
            $acc
            | insert $block.tool_use_id (
                $block.content | to json --raw | str substring 0..$ANSWER_CHARS
            )
        }
    )

    $events
    | where type == "assistant"
    | each {|event|
        $event.message.content
        | where type == "tool_use"
        | where ($it.name | str starts-with "mcp__agmem__")
        | each {|call|
            {
                # Which turn reached for it. In a scenario whose last turn is a
                # ritual, this is the difference between the ritual working and
                # the agent having already written on its own.
                turn: $event.turn
                tool: ($call.name | str replace "mcp__agmem__" "")
                input: $call.input
                answer: ($answers | get -o $call.id | default "")
            }
        }
    }
    | flatten
}

# Preload a store the way a previous session would have left it, and hand back
# the ids it wrote in seed order.
#
# Raw JSON-RPC over stdin rather than a client library: it is the same path a
# real session takes, and the process exits when stdin closes.
def seed [binary: string, data: string, cache: string, memories: list, overrides: record] {
    let request = {
        jsonrpc: "2.0"
        id: 1
        method: "initialize"
        params: {
            protocolVersion: "2025-06-18"
            capabilities: {}
            clientInfo: {name: "desc-eval", version: "1"}
        }
    }
    let call = {
        jsonrpc: "2.0"
        id: 2
        method: "tools/call"
        params: {name: "remember", arguments: {memories: $memories}}
    }
    let wire = (
        [$request {jsonrpc: "2.0", method: "notifications/initialized"} $call]
        | each {|message| $message | to json --raw}
        | str join "\n"
    )
    let seeded = (
        $wire
        | with-env (agmem-env $data $cache $overrides) { ^$binary --no-daemon | complete }
    )
    if $seeded.exit_code != 0 {
        error make {msg: $"seeding failed: ($seeded.stderr)"}
    }
    let answer = (
        $seeded.stdout
        | lines
        | each {|line| $line | from json}
        | where ($it.id? | default 0) == 2
    )
    if ($answer | is-empty) or (($answer | get -o 0.error) != null) {
        error make {msg: $"the store refused the seed: ($seeded.stdout)"}
    }
    # The ids, in send order, so a scenario can name a seeded claim and a
    # metric can say which row an agent closed rather than only how many.
    #
    # `remember` never compares two entries of one call, so nothing in a seed
    # batch deduplicates against itself and `created` lines up with the seed
    # one for one. A non-empty `duplicates` would shift every id after it
    # silently, so it is an error rather than a shrug.
    let stored = ($answer | get -o 0.result.structuredContent | default {})
    if ($stored.duplicates? | default [] | is-not-empty) {
        error make {msg: "the seed deduplicated against itself; ids no longer line up with it"}
    }
    $stored.created? | default []
}

# Backdate the `fast` rows a scenario just seeded, so `stale_contexts` has
# something to report.
#
# `last_accessed`, `strength` and `access_count` are the engine's to write:
# nothing in the tool surface can produce a row that has been idle for weeks,
# which is the only state that list reports. The `surreal` CLI opens agmem's
# own surrealkv store directly — same engine, same schema, no test-only door
# cut into the binary for it.
#
# `strength` decides whether the row is *also* expired: the startup sweep
# scales the horizon by it, so a row at strength 3 survives 60 days where the
# unscaled horizon is 20. Idle between the two is the state `consolidate`
# exists to notice and the sweep deliberately does not.
def age [data: string, stale: record] {
    let aged = (
        $"UPDATE memory SET last_accessed = time::now\(\) - duration::from_secs\(($stale.days * 86400)),
                            strength = ($stale.strength), access_count = ($stale.accesses)
          WHERE decay_class = 'fast';"
        | ^surreal sql --endpoint $"surrealkv://($data)/agmem.db" --ns agmem --db main --hide-welcome
        | complete
    )
    if $aged.exit_code != 0 {
        error make {msg: $"aging the seeded rows failed — is the surreal CLI installed? ($aged.stderr)"}
    }
}

# The seeded claims that stopped being live during the session, and why.
#
# Every metric above this reads what an agent *asked for*; this reads what the
# store did with it, and the two come apart exactly where it matters.
# `remember` blocks a claim it judges a duplicate, and a `supersedes` riding on
# a blocked claim closes nothing — so a run can send four correct merges and
# change nothing at all. A `purge` goes the other way and takes the row out of
# the table, which is why an id the query does not return counts as closed
# rather than as missing data.
#
# `expired` is left out: that is the startup sweep, not the agent, and this
# scenario deliberately ages its `fast` rows to a strength the sweep does not
# reach — a run credited for it would be credited for someone else's work.
#
# The `surreal` CLI opens agmem's own store directly, the same way `age`
# backdates rows. No tool reports liveness in bulk, and the alternative is one
# `inspect` per seeded claim.
def closed-rows [data: string, seeded: list] {
    if ($seeded | is-empty) {
        return []
    }
    let read = (
        "SELECT meta::id(id) AS id, invalid_reason FROM memory;"
        | ^surreal sql --endpoint $"surrealkv://($data)/agmem.db" --ns agmem --db main --hide-welcome --json
        | complete
    )
    if $read.exit_code != 0 {
        error make {msg: $"reading the store back failed: ($read.stderr)"}
    }
    # The CLI prints its own startup log to stdout ahead of the answer, so the
    # result is the one line that begins a JSON array — one entry per
    # statement, each holding its rows.
    let rows = (
        $read.stdout
        | lines
        | where {|line| $line | str starts-with "["}
        | each {|line| $line | from json | get 0}
        | flatten
    )
    $seeded
    | each {|row|
        let found = ($rows | where id == $row.id)
        let reason = if ($found | is-empty) {
            "purged"
        } else {
            $found | get -o 0.invalid_reason
        }
        if $reason == null or $reason == "expired" {
            null
        } else {
            {id: $row.id, content: $row.content, reason: $reason}
        }
    }
}

# Everything an agmem in this batch is started with.
def agmem-env [data: string, cache: string, overrides: record] {
    {AGMEM_DATA: $data, AGMEM_SPACE: "eval", FASTEMBED_CACHE_DIR: $cache} | merge $overrides
}

# The descriptions this binary serves, as `list_tools` reports them.
def descriptions [binary: string, cache: string, overrides: record] {
    let data = (mktemp -d)
    let wire = (
        [
            {
                jsonrpc: "2.0"
                id: 1
                method: "initialize"
                params: {
                    protocolVersion: "2025-06-18"
                    capabilities: {}
                    clientInfo: {name: "desc-eval", version: "1"}
                }
            }
            {jsonrpc: "2.0", method: "notifications/initialized"}
            {jsonrpc: "2.0", id: 2, method: "tools/list"}
        ]
        | each {|message| $message | to json --raw}
        | str join "\n"
    )
    $wire
    | with-env (agmem-env $data $cache $overrides) {
        ^$binary --no-daemon --embedder none | complete
    }
    | get stdout
    | lines
    | each {|line| $line | from json}
    | where ($it.id? | default 0) == 2
    | get 0.result.tools
    | select name description
}

# Where agmem keeps its ONNX model when nothing says otherwise.
def default-model-cache [] {
    let base = if $nu.os-info.name == "macos" {
        [$nu.home-dir "Library" "Application Support" "dev.agmem.agmem"] | path join
    } else {
        [$nu.home-dir ".local" "share" "agmem"] | path join
    }
    $base | path join "models"
}
