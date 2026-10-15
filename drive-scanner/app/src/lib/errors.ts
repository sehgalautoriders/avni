import { EngineMissing } from '../api';

/** Turns anything that was thrown into a message a person can read. */
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message || 'Something went wrong.';
  if (typeof e === 'string') return e || 'Something went wrong.';
  try {
    return JSON.stringify(e) ?? 'Something went wrong.';
  } catch {
    return 'Something went wrong.';
  }
}

export const isEngineMissing = (e: unknown): boolean => e instanceof EngineMissing;
