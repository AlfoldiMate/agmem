# /// script
# requires-python = ">=3.11"
# dependencies = ["transformers", "torch", "numpy", "sentencepiece", "protobuf"]
# ///
"""Issue #84's gate: can an NLI cross-encoder separate real corrected pairs
from the paraphrase noise in `consolidate`'s contradictions list (#53/#54)?

Verdict and numbers: docs/eval/nli-gate.md. Python rather than nu because the
scoring needs a transformer runtime; `uv run scripts/nli-gate-probe.py DUMP`
is self-contained.

DUMP is a JSON array of memory rows, produced from a *copy* of the store
(surrealkv allows one process per data dir, and the daemon holds the live
one):

    cp -r "~/Library/Application Support/dev.agmem.agmem/agmem.db" /tmp/probe.db
    echo 'SELECT record::id(id) AS id, space, kind, content, entities,
          embedding, valid_from, invalid_at, invalid_reason, superseded_by
          FROM memory;' \
      | surreal sql --endpoint surrealkv:///tmp/probe.db \
          --ns agmem --db main --json --hide-welcome

(strip the INFO log lines surreal prints around the JSON, unwrap the outer
one-element array).
"""

import json
import sys
from datetime import datetime, timezone

import numpy as np
import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer

# The #54 measurement instant: pair sets are built as of this time so the
# probe scores the world that measurement described, and again as of now.
T54 = datetime(2026, 8, 30, 19, 42, 18, tzinfo=timezone.utc)
MODEL = "cross-encoder/nli-deberta-v3-xsmall"
NOISE_SPACE = "agmem"


def parse_when(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")) if value else None


def link_id(value):
    """'memory:⟨01ABC⟩' or 'memory:01ABC' -> '01ABC'."""
    return value.split(":", 1)[1].strip("⟨⟩") if value else None


def corrected_pairs(rows, by_id):
    """(old_text, successor_text, closed_at) for every superseded row."""
    pairs = []
    for row in rows:
        if row.get("invalid_reason") != "superseded":
            continue
        successor = by_id.get(link_id(row.get("superseded_by")))
        if successor:
            pairs.append((row["content"], successor["content"], parse_when(row["invalid_at"])))
    return pairs


def noise_pairs(rows, space, t, cap=20, floor=0.75):
    """The #53 contradictions list as shipped: claims live at `t` sharing an
    entity, not supersedes-linked, top-`cap` by cosine over the floor."""

    def live_at(row):
        vf, ia = parse_when(row.get("valid_from")), parse_when(row.get("invalid_at"))
        return vf is not None and vf <= t and (ia is None or ia > t)

    def linked(a, b):
        return link_id(a.get("superseded_by")) == b["id"] or link_id(b.get("superseded_by")) == a["id"]

    pool = [r for r in rows if r["space"] == space and live_at(r)]
    vectors = np.array([r["embedding"] for r in pool])
    vectors = vectors / np.linalg.norm(vectors, axis=1, keepdims=True)
    similarity = vectors @ vectors.T
    pairs = []
    for i in range(len(pool)):
        for j in range(i + 1, len(pool)):
            a, b = pool[i], pool[j]
            shared = {e.lower() for e in a["entities"]} & {e.lower() for e in b["entities"]}
            if not shared or similarity[i, j] < floor or linked(a, b):
                continue
            pairs.append((float(similarity[i, j]), a["content"], b["content"]))
    pairs.sort(reverse=True)
    return [(a, b) for _, a, b in pairs[:cap]], [s for s, _, _ in pairs[:cap]]


@torch.no_grad()
def nli_probs(model, tok, pairs):
    """(n, 2, 3): both directions, softmax over the model's three labels."""
    out = []
    for a, b in pairs:
        enc = tok([(a, b), (b, a)], truncation=True, max_length=512, padding=True, return_tensors="pt")
        out.append(torch.softmax(model(**enc).logits, dim=1).numpy())
    return np.array(out)


def auc(pos, neg):
    wins = sum((p > n) + 0.5 * (p == n) for p in pos for n in neg)
    return wins / (len(pos) * len(neg))


def main():
    rows = json.load(open(sys.argv[1]))
    by_id = {r["id"]: r for r in rows}
    now = datetime.now(timezone.utc)

    corrected = corrected_pairs(rows, by_id)
    pos_orig = [(a, b) for a, b, closed in corrected if closed and closed <= T54]
    pos_all = [(a, b) for a, b, _ in corrected]
    neg_orig, sims_orig = noise_pairs(rows, NOISE_SPACE, T54)
    neg_now, _ = noise_pairs(rows, NOISE_SPACE, now)

    tok = AutoTokenizer.from_pretrained(MODEL)
    model = AutoModelForSequenceClassification.from_pretrained(MODEL)
    model.eval()
    labels = {v.lower(): k for k, v in model.config.id2label.items()}
    c, e, n = labels["contradiction"], labels["entailment"], labels["neutral"]

    aggregations = {
        "max p(c)": lambda x: x[:, :, c].max(1),
        "mean p(c)": lambda x: x[:, :, c].mean(1),
        "min p(c)": lambda x: x[:, :, c].min(1),
        "max p(c)-p(e)": lambda x: (x[:, :, c] - x[:, :, e]).max(1),
        "mean p(c)-p(e)": lambda x: (x[:, :, c] - x[:, :, e]).mean(1),
        "min p(c)-p(e)": lambda x: (x[:, :, c] - x[:, :, e]).min(1),
        "-max p(e)": lambda x: -x[:, :, e].max(1),
        "mean p(n)": lambda x: x[:, :, n].mean(1),
    }

    print(f"model {MODEL}")
    print(f"corrected: {len(pos_orig)} as-of #54, {len(pos_all)} now; "
          f"noise: {len(neg_orig)} as-of #54 "
          f"(cos {min(sims_orig, default=0):.3f}-{max(sims_orig, default=0):.3f}), {len(neg_now)} now")

    report = {"model": MODEL, "as_of": T54.isoformat(), "run_at": now.isoformat(), "sets": []}
    for name, pos, neg in [("orig", pos_orig, neg_orig), ("full", pos_all, neg_now)]:
        p, q = nli_probs(model, tok, pos), nli_probs(model, tok, neg)
        print(f"{name}: pos={len(pos)} neg={len(neg)}")
        entry = {"name": name, "auc": {}}
        for label, f in aggregations.items():
            entry["auc"][label] = auc(f(p), f(q))
            print(f"  {label:16s} AUC={entry['auc'][label]:.3f}")
        entry["pos_max_pc"] = p[:, :, c].max(1).tolist()
        entry["neg_max_pc"] = q[:, :, c].max(1).tolist()
        report["sets"].append(entry)

    with open("scores.json", "w") as out:
        json.dump(report, out, indent=1)
    print("per-pair scores written to scores.json")


if __name__ == "__main__":
    main()
