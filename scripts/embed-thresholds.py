# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Issue #133's threshold table: where do the store's labelled pair classes
land in a candidate model's cosine space, and how much margin do the write
gate and the supersede floor keep? The bar is in docs/eval/embed-models.md.

    uv run scripts/embed-thresholds.py DUMP [VECTORS] [--dims 512,256]

DUMP is the store dump pair-rank-probe.py takes. VECTORS is the `{id:
vector}` file `embed_dump` (crates/agmem-embed/tests/candidates.rs) wrote
for a candidate; omit it to table the dump's own (BGE) vectors, which is the
control. `--dims` adds MRL columns: the same vectors truncated to each width
and renormalised, which is what a model that supports Matryoshka truncation
would store.

Five classes, all fixed on the dump — never on the vectors under test:

- corrected     every superseded row against its successor: the pairs the
                supersede floor must admit (they are the same fact, revised)
- noise         the shipped contradictions list, top 20 by the dump's cosine
                among live same-entity pairs: paraphrase noise the band holds
- band          docs/eval/bands/pairs.json, seven contradictions and one
                control, embedded by `embed_dump` under `band:<name>:a|b`
                — the pairs the 0.95 duplicate gate must *not* block
- random        2,000 random live pairs: the unrelated floor
- same-entity   500 random live pairs sharing an entity but not linked:
                related, not revised

The abstention floor is not here: it separates query-vs-page similarities,
which only the eval harness measures (`calibrate_abstention` under
`AGMEM_EVAL_VECTORS_DIR`).
"""

import json
import sys

import numpy as np

NOISE_SPACE = "agmem"
COS_FLOOR = 0.75
LIST_CAP = 20
DUP_GATE = 0.95
SUPERSEDE_FLOOR = 0.75
RANDOM_PAIRS = 2000
SAME_ENTITY_PAIRS = 500
QUANTILES = (0.0, 0.05, 0.5, 0.95, 1.0)


def link_id(value):
    return value.split(":", 1)[1].strip("⟨⟩") if value else None


def unit(vectors):
    vectors = np.asarray(vectors, dtype=np.float64)
    return vectors / np.linalg.norm(vectors, axis=-1, keepdims=True)


def truncate(vectors, dim):
    return unit(vectors[:, :dim]) if dim else vectors


def load_sets(rows, dump_vectors):
    """Pairs of row ids per class, fixed on the dump's own vectors."""
    by_id = {r["id"]: r for r in rows}
    live = [r for r in rows if r["space"] == NOISE_SPACE and r.get("embedding") and not r.get("invalid_at")]
    ids = [r["id"] for r in live]
    index = {rid: i for i, rid in enumerate(ids)}
    v = unit([dump_vectors[rid] for rid in ids])
    sim = v @ v.T

    corrected = []
    for r in rows:
        if r.get("invalid_reason") != "superseded":
            continue
        successor = by_id.get(link_id(r.get("superseded_by")))
        if successor and r.get("embedding") and successor.get("embedding"):
            corrected.append((r["id"], successor["id"]))

    def linked(a, b):
        return link_id(a.get("superseded_by")) == b["id"] or link_id(b.get("superseded_by")) == a["id"]

    band_pairs, same_entity = [], []
    for i in range(len(live)):
        for j in range(i + 1, len(live)):
            a, b = live[i], live[j]
            shared = {e.lower() for e in a["entities"]} & {e.lower() for e in b["entities"]}
            if not shared or linked(a, b):
                continue
            same_entity.append((a["id"], b["id"]))
            if sim[i, j] >= COS_FLOOR:
                band_pairs.append((a["id"], b["id"], float(sim[i, j])))
    band_pairs.sort(key=lambda p: -p[2])
    noise = [(a, b) for a, b, _ in band_pairs[:LIST_CAP]]

    rng = np.random.default_rng(133)
    random_pairs = []
    while len(random_pairs) < RANDOM_PAIRS:
        i, j = rng.integers(0, len(ids), 2)
        if i != j:
            random_pairs.append((ids[i], ids[j]))
    rng.shuffle(same_entity)
    return {
        "corrected": corrected,
        "noise": noise,
        "random": random_pairs,
        "same-entity": [tuple(p) for p in same_entity[:SAME_ENTITY_PAIRS]],
    }, index


def cosines(pairs, vectors):
    a = unit([vectors[x] for x, _ in pairs])
    b = unit([vectors[y] for _, y in pairs])
    return np.einsum("ij,ij->i", a, b)


def quantiles(values):
    return [float(np.quantile(values, q)) for q in QUANTILES] if len(values) else [float("nan")] * len(QUANTILES)


def table(sets, vectors, label):
    print(f"\n== {label}")
    print(f"{'class':<14}{'n':>6}  {'min':>7}{'p5':>7}{'median':>8}{'p95':>7}{'max':>7}")
    measured = {}
    for name, pairs in sets.items():
        if not pairs:
            print(f"{name:<14}{0:>6}  (empty)")
            continue
        c = cosines(pairs, vectors)
        measured[name] = c
        q = quantiles(c)
        print(f"{name:<14}{len(c):>6}  " + "".join(f"{x:>7.3f}" for x in q[:2]) + f"{q[2]:>8.3f}" + "".join(f"{x:>7.3f}" for x in q[3:]))
    return measured


def margins(measured):
    """How the current constants sit against each class, and the widest
    separable placement for each threshold on this model."""
    c = measured
    print()
    if "corrected" in c and "random" in c:
        floor_room = float(np.quantile(c["corrected"], 0.05)) - float(np.quantile(c["random"], 0.999))
        admitted = float(np.mean(c["corrected"] >= SUPERSEDE_FLOOR))
        leaked = float(np.mean(c["random"] >= SUPERSEDE_FLOOR))
        print(f"supersede floor {SUPERSEDE_FLOOR}: admits {admitted:.0%} of corrected, leaks {leaked:.2%} of random;"
              f" gap corrected.p5 − random.p99.9 = {floor_room:+.3f}"
              + ("  (no separable band: the floor sits inside the unrelated distribution)" if floor_room <= 0 else ""))
        lo, hi = float(np.quantile(c["random"], 0.999)), float(np.quantile(c["corrected"], 0.05))
        if hi > lo:
            print(f"  widest floor on this model: {(lo + hi) / 2:.3f} (band {lo:.3f}–{hi:.3f})")
    if "band" in c and "corrected" in c:
        blocked = float(np.mean(c["band"] >= DUP_GATE))
        print(f"duplicate gate {DUP_GATE}: blocks {blocked:.0%} of the band contradictions "
              f"(max {float(c['band'].max()):.3f}); corrected pairs over the gate: {float(np.mean(c['corrected'] >= DUP_GATE)):.0%}")
        top = float(c["band"].max())
        print(f"  a gate that admits every band contradiction sits above {top:.3f}")
    if "noise" in c and "corrected" in c:
        pos, neg = c["corrected"], c["noise"]
        auc = float(np.mean([p > n for p in pos for n in neg]) + 0.5 * np.mean([p == n for p in pos for n in neg]))
        print(f"corrected vs noise, cosine AUC {auc:.3f} (current set; pair-rank-probe.py has the permutation p)")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dims = []
    if "--dims" in sys.argv:
        dims = [int(d) for d in sys.argv[sys.argv.index("--dims") + 1].split(",")]
    rows = json.load(open(args[0]))
    dump_vectors = {r["id"]: r["embedding"] for r in rows if r.get("embedding")}
    sets, _ = load_sets(rows, dump_vectors)

    if len(args) > 1:
        vectors = json.load(open(args[1]))
        label = args[1].rsplit("/", 1)[-1].removesuffix("-dump-vectors.json")
        band = [(f"band:{n}:a", f"band:{n}:b") for n in sorted({k.split(":")[1] for k in vectors if k.startswith("band:")})]
        sets["band"] = band
    else:
        vectors, label = dump_vectors, "dump (control)"
    missing = [x for pairs in sets.values() for p in pairs for x in p if x not in vectors]
    if missing:
        sys.exit(f"vectors lack {len(missing)} ids, e.g. {missing[0]}")

    widths = [None] + [d for d in dims if d < len(next(iter(vectors.values())))]
    print(f"rows {len(rows)}; " + ", ".join(f"{k} {len(v)}" for k, v in sets.items()))
    for width in widths:
        truncated = {k: unit(np.asarray(v)[:width]) for k, v in vectors.items()} if width else vectors
        measured = table(sets, truncated, f"{label}" + (f" @ {width}d" if width else ""))
        margins(measured)


if __name__ == "__main__":
    main()
