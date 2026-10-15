import type { Verdict, VerdictTotal } from '../api';

export interface Amount {
  files: number;
  bytes: number;
}

const ZERO: Amount = { files: 0, bytes: 0 };

/** The total for one verdict (null = files the engine has not given a verdict). */
export function findTotal(totals: VerdictTotal[], verdict: Verdict | null): Amount {
  const hit = totals.find((t) => t.verdict === verdict);
  return hit ? { files: hit.files, bytes: hit.bytes } : ZERO;
}

export function sumTotals(totals: VerdictTotal[], verdicts: Verdict[]): Amount {
  return verdicts.reduce<Amount>(
    (acc, v) => {
      const t = findTotal(totals, v);
      return { files: acc.files + t.files, bytes: acc.bytes + t.bytes };
    },
    { files: 0, bytes: 0 },
  );
}

/** True when some files came back without a verdict, so a column is needed for them. */
export const hasUnjudged = (totals: VerdictTotal[]): boolean => findTotal(totals, null).files > 0;
