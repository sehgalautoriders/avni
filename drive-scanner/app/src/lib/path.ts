/** The last part of a Windows or plain path: "D:\\Clients\\a.pdf" -> "a.pdf". */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter((p) => p !== '');
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

/** Reports and logs the table viewer can open. */
export const isTableName = (name: string): boolean => /\.(csv|tsv|xlsx)$/i.test(name);
