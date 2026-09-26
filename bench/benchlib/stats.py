"""Order statistics and the Mann–Whitney U test, in the standard library only."""

import math
from statistics import median as _median


def median(xs):
    return _median(xs)


def quartiles(xs):
    """The first and third quartiles, interpolated linearly between order statistics (the
    "inclusive" method, R's type 7 and NumPy's default)."""
    s = sorted(xs)
    return _quantile(s, 0.25), _quantile(s, 0.75)


def _quantile(s, q):
    if len(s) == 1:
        return s[0]
    pos = (len(s) - 1) * q
    lo = math.floor(pos)
    hi = min(lo + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (pos - lo)


def iqr(xs):
    q1, q3 = quartiles(xs)
    return q3 - q1


def ranks(values):
    """1-based ranks, ties getting the mean of the ranks they span."""
    order = sorted(range(len(values)), key=lambda i: values[i])
    out = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        for k in range(i, j + 1):
            out[order[k]] = (i + j) / 2 + 1
        i = j + 1
    return out


# Above this pooled size the exact distribution gets slow in pure Python, and the normal
# approximation is good.
EXACT_MAX = 40


def mann_whitney(a, b):
    """Two-sided Mann–Whitney U test of `a` against `b`. Returns (U of `a`, p-value).

    For pooled samples up to EXACT_MAX values the p-value is exact: the permutation distribution
    of the rank sum given the observed ranks, ties included, counted by dynamic programming.
    Larger samples use the normal approximation with tie and continuity corrections."""
    n1, n2 = len(a), len(b)
    if n1 == 0 or n2 == 0:
        raise ValueError("both samples need values")
    r = ranks(list(a) + list(b))
    r1 = sum(r[:n1])
    u1 = r1 - n1 * (n1 + 1) / 2
    if len(set(r)) == 1:
        return u1, 1.0
    if n1 + n2 <= EXACT_MAX:
        return u1, _exact_p(r, n1, r1)
    return u1, _normal_p(r, n1, n2, u1)


def _exact_p(r, n1, r1):
    # Ranks are multiples of 1/2, so doubled they are integers: count the subsets of n1 doubled
    # ranks by size and sum.
    doubled = [round(2 * x) for x in r]
    total = sum(doubled)
    # counts[k][s]: subsets of k ranks seen so far with doubled sum s.
    counts = [[0] * (total + 1) for _ in range(n1 + 1)]
    counts[0][0] = 1
    for d in doubled:
        for k in range(n1, 0, -1):
            prev, row = counts[k - 1], counts[k]
            for s in range(total - d, -1, -1):
                if prev[s]:
                    row[s + d] += prev[s]
    subsets = counts[n1]
    mean = n1 * total / len(r)
    observed = abs(round(2 * r1) - mean)
    extreme = sum(c for s, c in enumerate(subsets) if c and abs(s - mean) >= observed - 1e-9)
    return min(1.0, extreme / sum(subsets))


def _normal_p(r, n1, n2, u1):
    n = n1 + n2
    ties = {}
    for x in r:
        ties[x] = ties.get(x, 0) + 1
    tie_term = sum(t**3 - t for t in ties.values()) / (n * (n - 1))
    var = n1 * n2 / 12 * ((n + 1) - tie_term)
    if var <= 0:
        return 1.0
    z = (abs(u1 - n1 * n2 / 2) - 0.5) / math.sqrt(var)
    return min(1.0, math.erfc(max(z, 0) / math.sqrt(2)))


def benjamini_hochberg(pvalues):
    """False-discovery-rate q-values for a list of p-values, in the same order."""
    m = len(pvalues)
    order = sorted(range(m), key=lambda i: pvalues[i])
    q = [0.0] * m
    running = 1.0
    for rank in range(m, 0, -1):
        i = order[rank - 1]
        running = min(running, pvalues[i] * m / rank)
        q[i] = running
    return q
