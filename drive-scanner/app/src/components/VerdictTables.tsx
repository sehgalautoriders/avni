import { VERDICT_LABEL, type Summary, type Verdict } from '../api';
import { formatBytes, formatCount } from '../lib/format';
import { VERDICT_ORDER } from '../lib/labels';
import { findTotal, hasUnjudged } from '../lib/totals';
import { TableFrame } from './ui';

function Cell({ total }: { total: { files: number; bytes: number } }) {
  if (total.files === 0 && total.bytes === 0) {
    return (
      <span className="text-muted" title="0 files">
        —
      </span>
    );
  }
  return (
    <span title={`${formatCount(total.files)} files · ${formatCount(total.bytes)} bytes`}>
      {formatBytes(total.bytes)}
    </span>
  );
}

/** Rows = verdicts, one column per scanned drive plus "All drives". Used in Plan & Actions. */
export function VerdictByDrive({ summary }: { summary: Summary }) {
  const rows: (Verdict | null)[] = [...VERDICT_ORDER];
  if (hasUnjudged(summary.totals) || summary.drives.some((d) => hasUnjudged(d.totals))) rows.push(null);
  return (
    <TableFrame label="What was found on each drive" fit>
      <table className="tbl">
        <thead>
          <tr>
            <th>What it is</th>
            {summary.drives.map((d) => (
              <th key={d.drive.id} className="num">
                {d.drive.id}
              </th>
            ))}
            <th className="num">All drives</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((v) => (
            <tr key={v ?? 'none'}>
              <td>{v ? VERDICT_LABEL[v] : 'No verdict yet'}</td>
              {summary.drives.map((d) => (
                <td key={d.drive.id} className="num">
                  <Cell total={findTotal(d.totals, v)} />
                </td>
              ))}
              <td className="num font-semibold">
                <Cell total={findTotal(summary.totals, v)} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </TableFrame>
  );
}
