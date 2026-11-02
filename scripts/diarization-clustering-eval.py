#!/usr/bin/env python3
"""Compare counting methods on worker-exported embeddings. Requires NumPy.

Run diarization_eval with NOTETAKER_EVAL_DUMP=1 first. No reference labels enter
clustering or model selection. Reference labels are used only for scoring.
"""
import argparse
import json
from pathlib import Path

import numpy as np


def hierarchy(vectors):
    n = len(vectors)
    distances = 1 - vectors @ vectors.T
    np.fill_diagonal(distances, np.inf)
    groups = [[i] for i in range(n)]
    sizes = np.ones(n)
    yield [g.copy() for g in groups], 0
    while sum(bool(g) for g in groups) > 1:
        i, j = np.unravel_index(np.argmin(distances), distances.shape)
        distance = distances[i, j]
        row = (distances[i] * sizes[i] + distances[j] * sizes[j]) / (sizes[i] + sizes[j])
        distances[i] = distances[:, i] = row
        distances[i, i] = np.inf
        distances[j] = distances[:, j] = np.inf
        groups[i] += groups[j]
        groups[j] = []
        sizes[i] += sizes[j]
        yield [g.copy() for g in groups if g], float(distance)


def metrics(data, labels, case):
    overlap = {}
    for item, label in zip(data, labels):
        turn = item["turn"]
        scores = overlap.setdefault(label, np.zeros(case["speakers"]))
        for start, end, speaker in case["truth"]:
            scores[speaker] += max(0, min(end, turn["end_ms"]) - max(start, turn["start_ms"]))
    total = max(1, sum(sum(v) for v in overlap.values()))
    best = np.zeros(1 << case["speakers"])
    for scores in overlap.values():
        previous = best.copy()
        for mask, value in enumerate(previous):
            for speaker, ms in enumerate(scores):
                if not (mask >> speaker) & 1:
                    next_mask = mask | (1 << speaker)
                    best[next_mask] = max(best[next_mask], value + ms)
    return {"count": len(overlap), "purity": sum(max(v) for v in overlap.values()) / total,
            "one_to_one": max(best) / total}


def evaluate(case, comparison):
    data, fallback = comparison["turns"], comparison["fallback"]
    training = [d for d in data if d["clean_ms"] >= 1500 and d["embedding"]]
    # Same stable pseudo-random priorities as the worker; avoid periodic aliases.
    def priority(i):
        mask = (1 << 64) - 1
        value = (i + 0x9e3779b97f4a7c15) & mask
        value = ((value ^ (value >> 30)) * 0xbf58476d1ce4e5b9) & mask
        value = ((value ^ (value >> 27)) * 0x94d049bb133111eb) & mask
        return value ^ (value >> 31)
    if len(training) > 512:
        indices = sorted(sorted(range(len(training)), key=priority)[:512])
        training = [training[i] for i in indices]
    vectors = np.array([d["embedding"] for d in training])
    durations = np.array([d["clean_ms"] for d in training])
    states = list(hierarchy(vectors))
    groups = next((states[i - 1][0] for i, (_, d) in enumerate(states) if d > .5), states[-1][0])

    def assign(groups, floor):
        substantial = [g for g in groups if sum(durations[g]) >= floor]
        if not substantial:
            substantial = [max(groups, key=lambda g: sum(durations[g]))]
        centers = np.array([vectors[g].mean(axis=0) for g in substantial])
        centers /= np.linalg.norm(centers, axis=1)[:, None]
        labels = []
        for d in data:
            vector = d["embedding"] if d["clean_ms"] >= 500 else None
            vector = vector or fallback.get(d["turn"]["speaker"]) or d["embedding"]
            labels.append(str(np.argmax(centers @ vector)) if vector else d["turn"]["speaker"])
        return metrics(data, labels, case)

    result = {"average_linkage": assign(groups, 0),
              "average_min_duration": assign(groups, int(min(max(6000, sum(durations) * .02), sum(durations) * .10)))}
    # Mean silhouette over all train embeddings, singletons score zero.
    distances = np.maximum(0, 1 - vectors @ vectors.T)
    candidates = []
    for partition, _ in states:
        if not 2 <= len(partition) <= min(16, len(vectors) - 1):
            continue
        scores = []
        for own in partition:
            for i in own:
                if len(own) == 1:
                    scores.append(0)
                    continue
                a = sum(distances[i, own]) / (len(own) - 1)
                b = min(np.mean(distances[i, g]) for g in partition if g is not own)
                scores.append((b - a) / max(a, b, 1e-9))
        candidates.append((np.mean(scores), partition))
    if candidates:
        partition = max(candidates, key=lambda x: x[0])[1]
        result["silhouette"] = assign(partition, 0)
    # Simple normalized-Laplacian eigengap, not NeMo's full NME-SC pipeline.
    affinity = np.maximum(0, vectors @ vectors.T - .4)
    np.fill_diagonal(affinity, 0)
    inverse = 1 / np.sqrt(np.maximum(affinity.sum(axis=1), 1e-9))
    laplacian = np.eye(len(vectors)) - affinity * inverse[:, None] * inverse[None, :]
    eigenvalues = np.linalg.eigvalsh(laplacian)
    if len(eigenvalues) > 1:
        count = int(np.argmax(np.diff(eigenvalues[:17]))) + 1
        partition = next(g for g, _ in states if len(g) == count)
        result["eigengap_average"] = assign(partition, 0)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, default=Path("target/diarization-eval"))
    args = parser.parse_args()
    report = {}
    for case in json.loads((args.dir / "manifest.json").read_text()):
        path = args.dir / f"{case['name']}-comparison.json"
        if path.exists():
            report[case["name"]] = evaluate(case, json.loads(path.read_text()))
            print(case["name"], json.dumps(report[case["name"]]))
    (args.dir / "report-counting-methods.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
