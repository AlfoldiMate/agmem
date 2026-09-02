# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Issue #53's probe: can model-free features rank the store's real corrected
pairs above the paraphrase noise that fills `consolidate`'s contradictions
list? The bar was written before this ran: docs/eval/pair-rank.md.

    uv run scripts/pair-rank-probe.py DUMP

DUMP is a JSON array of memory rows from a *copy* of the store, as the doc
shows (the same dump nli-gate-probe.py takes, plus created_at, tags and
writer). numpy only: the point of the probe is that no model is involved.

Every feature reads three fields — content, entities, created_at — through
`View`, which refuses anything else. The labels (invalid_reason,
superseded_by, valid_from) are what is being predicted, and a probe that can
see them by accident measures nothing.
"""

import json
import math
import random
import re
import sys
from datetime import datetime, timezone

import numpy as np

# The #54 measurement instant, so the first set is the one nli-gate.md scored.
T54 = datetime(2026, 8, 30, 19, 42, 18, tzinfo=timezone.utc)
NOISE_SPACE = "agmem"
COS_FLOOR = 0.75
LIST_CAP = 20
# tools/hop.rs: an entity on at least this share of the pool names the topic
# everything is already about, and says nothing about a pair.
HUB_SHARE = 0.5
# Slot changes only count between texts that otherwise say the same thing.
OVERLAP_FLOOR = 0.3
# A feature this weak on its own is dropped before the blend is scored.
FEATURE_FLOOR = 0.55
PERMUTATIONS = 10_000
CUES = (
    "no longer", "instead", "now", "turned out", "not", "never", "used to",
    "previously", "rather than", "superseded", "stale", "was", "were",
)
# Numbers, dates, versions, issue/PR ids — the slots a fact update changes.
SLOT = re.compile(r"#\d+|v?\d+(?:\.\d+)+|\d{4}-\d{2}-\d{2}(?:T[\d:.]+Z?)?|\d+")
WORD = re.compile(r"[a-z][a-z0-9_'-]*")
ALLOWED = frozenset({"content", "entities", "created_at"})


class View:
    """A row as a feature is allowed to see it."""

    def __init__(self, row):
        self._row = row

    def __getitem__(self, key):
        if key not in ALLOWED:
            raise KeyError(f"feature read a label field: {key}")
        return self._row[key]


def parse_when(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")) if value else None


def link_id(value):
    """'memory:⟨01ABC⟩' or 'memory:01ABC' -> '01ABC'."""
    return value.split(":", 1)[1].strip("⟨⟩") if value else None


# --- labelled sets (these read the labels; the features do not) -------------


def corrected_pairs(rows, by_id):
    """(old_row, successor_row, closed_at) for every superseded row."""
    pairs = []
    for row in rows:
        if row.get("invalid_reason") != "superseded":
            continue
        successor = by_id.get(link_id(row.get("superseded_by")))
        if successor:
            pairs.append((row, successor, parse_when(row["invalid_at"])))
    return pairs


def candidate_pairs(rows, space, t):
    """Every pair the shipped list could hold at `t`: live, same space, a
    shared entity, over the cosine floor, not supersedes-linked — with its
    cosine. The list itself is the top LIST_CAP of these by cosine."""

    def live_at(row):
        vf, ia = parse_when(row.get("valid_from")), parse_when(row.get("invalid_at"))
        return vf is not None and vf <= t and (ia is None or ia > t)

    def linked(a, b):
        return link_id(a.get("superseded_by")) == b["id"] or link_id(b.get("superseded_by")) == a["id"]

    pool = [r for r in rows if r["space"] == space and live_at(r) and r.get("embedding")]
    vectors = np.array([r["embedding"] for r in pool])
    vectors = vectors / np.linalg.norm(vectors, axis=1, keepdims=True)
    similarity = vectors @ vectors.T
    pairs = []
    for i in range(len(pool)):
        for j in range(i + 1, len(pool)):
            a, b = pool[i], pool[j]
            shared = {e.lower() for e in a["entities"]} & {e.lower() for e in b["entities"]}
            if not shared or similarity[i, j] < COS_FLOOR or linked(a, b):
                continue
            pairs.append((a, b, float(similarity[i, j])))
    pairs.sort(key=lambda p: -p[2])
    return pairs, pool


def cosine(a, b):
    va, vb = np.array(a["embedding"]), np.array(b["embedding"])
    return float(va @ vb / (np.linalg.norm(va) * np.linalg.norm(vb)))


# --- features ---------------------------------------------------------------


def document_frequency(pool):
    df = {}
    for row in pool:
        for entity in {e.lower() for e in row["entities"]}:
            df[entity] = df.get(entity, 0) + 1
    return df


def entity_rarity(a: View, b: View, df, pool_size):
    """The rarest subject the two share, as log(pool / df); hubs count 0."""
    shared = {e.lower() for e in a["entities"]} & {e.lower() for e in b["entities"]}
    best = 0.0
    for entity in shared:
        freq = df.get(entity, 1)
        if freq >= HUB_SHARE * pool_size:
            continue
        best = max(best, math.log(pool_size / freq))
    return best


def only_hubs(a: View, b: View, df, pool_size):
    shared = {e.lower() for e in a["entities"]} & {e.lower() for e in b["entities"]}
    return all(df.get(e, 1) >= HUB_SHARE * pool_size for e in shared)


def age_gap(a: View, b: View):
    ta, tb = parse_when(a["created_at"]), parse_when(b["created_at"])
    return math.log1p(abs((ta - tb).total_seconds()) / 86400)


def slots_and_tokens(text):
    slots = set(SLOT.findall(text))
    masked = SLOT.sub(" ", text.lower())
    return slots, set(WORD.findall(masked))


def masked_jaccard(a: View, b: View):
    _, ta = slots_and_tokens(a["content"])
    _, tb = slots_and_tokens(b["content"])
    return len(ta & tb) / len(ta | tb) if ta | tb else 0.0


def slot_change(a: View, b: View, gated=True):
    sa, ta = slots_and_tokens(a["content"])
    sb, tb = slots_and_tokens(b["content"])
    overlap = len(ta & tb) / len(ta | tb) if ta | tb else 0.0
    if gated and overlap < OVERLAP_FLOOR:
        return 0.0
    return float(len(sa ^ sb))


def cue_asymmetry(a: View, b: View):
    la, lb = a["content"].lower(), b["content"].lower()
    return float(sum((cue in la) != (cue in lb) for cue in CUES))


# --- scoring ----------------------------------------------------------------


def auc(pos, neg):
    pos, neg = np.asarray(pos), np.asarray(neg)
    wins = (pos[:, None] > neg[None, :]).sum() + 0.5 * (pos[:, None] == neg[None, :]).sum()
    return float(wins / (len(pos) * len(neg)))


def ranks(values):
    """Average ranks, 1-based, ties shared."""
    values = np.asarray(values)
    order = values.argsort()
    r = np.empty(len(values))
    i = 0
    while i < len(values):
        j = i
        while j + 1 < len(values) and values[order[j + 1]] == values[order[i]]:
            j += 1
        r[order[i : j + 1]] = (i + j) / 2 + 1
        i = j + 1
    return r


def blend(feature_columns):
    """Unweighted mean of per-feature ranks over the rows given."""
    return np.mean([ranks(col) for col in feature_columns], axis=0)


def permutation_p(pos_scores, neg_scores, observed, rng):
    scores = np.concatenate([pos_scores, neg_scores])
    n_pos = len(pos_scores)
    hits = 0
    for _ in range(PERMUTATIONS):
        rng.shuffle(scores)
        if auc(scores[:n_pos], scores[n_pos:]) >= observed:
            hits += 1
    return (hits + 1) / (PERMUTATIONS + 1)


def features_for(pairs, df, pool_size):
    """Feature columns for (a, b) pairs, plus cosine as the control."""
    cols = {"rarity": [], "age_gap": [], "slot_change": [], "slot_change_raw": [], "cue_asym": []}
    control = []
    for a, b in pairs:
        va, vb = View(a), View(b)
        cols["rarity"].append(entity_rarity(va, vb, df, pool_size))
        cols["age_gap"].append(age_gap(va, vb))
        cols["slot_change"].append(slot_change(va, vb))
        cols["slot_change_raw"].append(slot_change(va, vb, gated=False))
        cols["cue_asym"].append(cue_asymmetry(va, vb))
        control.append(cosine(a, b))
    return {k: np.array(v) for k, v in cols.items()}, np.array(control)


def score_set(name, pos, neg, df, pool_size, rng, report):
    print(f"\n== {name}: {len(pos)} corrected vs {len(neg)} noise ==")
    fpos, cpos = features_for(pos, df, pool_size)
    fneg, cneg = features_for(neg, df, pool_size)
    entry = {"name": name, "n_pos": len(pos), "n_neg": len(neg), "feature_auc": {}, "blend": {}}

    kept = []
    for feature in ("rarity", "age_gap", "slot_change", "slot_change_raw", "cue_asym"):
        a = auc(fpos[feature], fneg[feature])
        entry["feature_auc"][feature] = a
        verdict = ""
        if feature in ("rarity", "age_gap", "slot_change", "cue_asym"):
            if a >= FEATURE_FLOOR:
                kept.append(feature)
            else:
                verdict = "  (dropped from the blend: below the feature floor)"
        print(f"  {feature:16s} AUC={a:.3f}{verdict}")
    control_auc = auc(cpos, cneg)
    entry["feature_auc"]["cosine (control)"] = control_auc
    print(f"  {'cosine (control)':16s} AUC={control_auc:.3f}")

    if kept:
        n_pos = len(pos)
        combined = blend([np.concatenate([fpos[f], fneg[f]]) for f in kept])
        blend_auc = auc(combined[:n_pos], combined[n_pos:])
        p = permutation_p(combined[:n_pos].copy(), combined[n_pos:].copy(), blend_auc, rng)
        entry["blend"] = {"features": kept, "auc": blend_auc, "permutation_p": p}
        print(f"  blend({', '.join(kept)}) AUC={blend_auc:.3f}  permutation p={p:.4f}")
    else:
        print("  no feature cleared the floor; there is no blend to score")

    report["sets"].append(entry)
    return entry


def time_matched(pos, wide_neg, rng):
    """Negatives resampled from the wide candidate pool to match the
    positives' age-gap distribution, bin by bin."""
    gaps_pos = [age_gap(View(a), View(b)) for a, b in pos]
    gaps_neg = [age_gap(View(a), View(b)) for a, b, _ in wide_neg]
    edges = np.histogram_bin_edges(gaps_pos, bins=6)
    matched = []
    for lo, hi in zip(edges[:-1], edges[1:]):
        want = sum(lo <= g <= hi for g in gaps_pos)
        have = [p for p, g in zip(wide_neg, gaps_neg) if lo <= g <= hi]
        if have and want:
            picks = rng.choice(len(have), size=want, replace=True)
            matched.extend(have[i] for i in picks)
    return [(a, b) for a, b, _ in matched]


def main():
    rows = json.load(open(sys.argv[1]))
    by_id = {r["id"]: r for r in rows}
    now = datetime.now(timezone.utc)
    rng = np.random.default_rng(53)

    # The leakage guard, run before anything else reads a row.
    try:
        View(rows[0])["invalid_reason"]
    except KeyError:
        pass
    else:
        sys.exit("View let a feature read a label field; the probe is not trustworthy")

    corrected = corrected_pairs(rows, by_id)
    pos_t54 = [(a, b) for a, b, closed in corrected if closed and closed <= T54]
    pos_all = [(a, b) for a, b, _ in corrected]
    cands_t54, pool_t54 = candidate_pairs(rows, NOISE_SPACE, T54)
    cands_now, pool_now = candidate_pairs(rows, NOISE_SPACE, now)
    neg_t54 = [(a, b) for a, b, _ in cands_t54[:LIST_CAP]]
    neg_now = [(a, b) for a, b, _ in cands_now[:LIST_CAP]]

    # Document frequency over the whole space in the dump, live or not: the
    # positives include closed rows, and one pool for both sides keeps the
    # rarity of an entity from depending on which set a pair came from.
    space_rows = [r for r in rows if r["space"] == NOISE_SPACE]
    df = document_frequency(space_rows)
    pool_size = len(space_rows)
    hub_share_now = float(np.mean([only_hubs(View(a), View(b), df, pool_size) for a, b, _ in cands_now])) if cands_now else 1.0

    print(f"rows {len(rows)}; space {NOISE_SPACE}: {pool_size} rows, {len(pool_now)} live now, {len(pool_t54)} live at #54")
    print(f"corrected pairs: {len(pos_t54)} closed by #54, {len(pos_all)} now")
    print(f"candidate pairs in band: {len(cands_t54)} at #54, {len(cands_now)} now (list cap {LIST_CAP})")
    print(f"candidate pairs sharing only hub entities: {hub_share_now:.0%}"
          + ("  (>80%: the rarity lever is dead in this space)" if hub_share_now > 0.8 else ""))
    top_df = sorted(df.items(), key=lambda kv: -kv[1])[:5]
    print("entity DF, top 5: " + ", ".join(f"{e}={n}" for e, n in top_df))

    report = {"run_at": now.isoformat(), "as_of": T54.isoformat(), "hub_only_share": hub_share_now, "sets": []}
    score_set("T54", pos_t54, neg_t54, df, pool_size, rng, report)
    current = score_set("current", pos_all, neg_now, df, pool_size, rng, report)

    # Age gap against time-matched negatives from the wide band.
    matched = time_matched(pos_all, cands_now, rng)
    if matched:
        a = auc([age_gap(View(x), View(y)) for x, y in pos_all], [age_gap(View(x), View(y)) for x, y in matched])
        report["age_gap_time_matched_auc"] = a
        print(f"\nage_gap against {len(matched)} time-matched negatives: AUC={a:.3f}"
              + ("  (the feature was measuring write history, not the pair)" if a < FEATURE_FLOOR else ""))

    # Reorder the live list by the blend and count what it evicts.
    kept = current["blend"].get("features", [])
    if kept and cands_now:
        fall, _ = features_for([(a, b) for a, b, _ in cands_now], df, pool_size)
        order = np.argsort(-blend([fall[f] for f in kept]))
        new_top = [cands_now[i] for i in order[:LIST_CAP]]
        old_ids = {(a["id"], b["id"]) for a, b, _ in cands_now[:LIST_CAP]}
        evicted = sum((a["id"], b["id"]) not in old_ids for a, b, _ in new_top)
        report["evicted_from_cosine_top20"] = evicted
        print(f"\nblend order evicts {evicted} of cosine's top {LIST_CAP}")
        with open("pair-rank-top20.json", "w") as out:
            json.dump(
                [{"a": a["content"], "b": b["content"], "cosine": s, "label": None} for a, b, s in new_top],
                out, indent=1, ensure_ascii=False,
            )
        print("reordered top 20 written to pair-rank-top20.json — hand-label `label` as true/false")

    with open("pair-rank-scores.json", "w") as out:
        json.dump(report, out, indent=1)
    print("report written to pair-rank-scores.json")


if __name__ == "__main__":
    main()
