// Small display helpers shared by the Board.

/** Relative age like `5s`, `3m`, `2h`, `1d` from an ISO timestamp. */
export function relativeAge(iso: string, nowMs: number): string {
  const seconds = Math.max(0, Math.floor((nowMs - Date.parse(iso)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** Token count with a `k` abbreviation: `760`, `4.7k`, `27.9k`, `128k`. */
export function abbrevTokens(n: number): string {
  if (n < 1000) return `${n}`;
  const k = n / 1000;
  const str = k < 100 ? k.toFixed(1) : k.toFixed(0);
  return `${str.replace(/\.0$/, "")}k`;
}

/** Turn a model id like `claude-opus-4-8` into `Opus 4.8`; falls back to raw. */
export function shortModel(model: string | null): string | null {
  if (!model) return model;
  const m = model.match(/(opus|sonnet|haiku|fable)-(\d+)(?:-(\d+))?/i);
  if (!m) return model;
  const family = m[1][0].toUpperCase() + m[1].slice(1).toLowerCase();
  const version = m[3] ? `${m[2]}.${m[3]}` : m[2];
  return `${family} ${version}`;
}
