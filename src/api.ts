// Scrapbox / Cosense REST API client (read-only).
// Public projects need no auth. Private projects: pass a connect.sid cookie.

export interface PageSummary {
  id: string;
  title: string;
  image: string | null;
  descriptions: string[];
  updated: number;
  linked: number;
}

export interface PageLine {
  id: string;
  text: string;
}

export interface Page {
  id: string;
  title: string;
  lines: PageLine[];
  links: string[];
  updated: number;
  created: number;
  linesCount: number;
}

export interface Config {
  project: string;
  sid?: string | undefined;
  apiDomain: string;
}

function headers(cfg: Config): Record<string, string> {
  const h: Record<string, string> = { Accept: "application/json" };
  if (cfg.sid) h.Cookie = `connect.sid=${cfg.sid}`;
  return h;
}

function base(cfg: Config): string {
  return `https://${cfg.apiDomain}/api`;
}

async function getJson<T>(url: string, cfg: Config): Promise<T> {
  const res = await fetch(url, { headers: headers(cfg) });
  if (!res.ok) {
    throw new Error(`HTTP ${res.status} ${res.statusText} for ${url}`);
  }
  return (await res.json()) as T;
}

export async function listPages(
  cfg: Config,
  opts: { limit?: number; skip?: number; sort?: string } = {}
): Promise<{ count: number; pages: PageSummary[] }> {
  const limit = opts.limit ?? 100;
  const skip = opts.skip ?? 0;
  const sort = opts.sort ?? "updated";
  const url = `${base(cfg)}/pages/${encodeURIComponent(
    cfg.project
  )}?limit=${limit}&skip=${skip}&sort=${sort}`;
  const data = await getJson<{ count: number; pages: PageSummary[] }>(url, cfg);
  return { count: data.count, pages: data.pages };
}

export async function getPage(cfg: Config, title: string): Promise<Page> {
  const url = `${base(cfg)}/pages/${encodeURIComponent(
    cfg.project
  )}/${encodeURIComponent(title)}`;
  return getJson<Page>(url, cfg);
}

export interface SearchResult {
  title: string;
  words: string[];
  lines: string[];
}

export async function searchPages(
  cfg: Config,
  query: string
): Promise<{ count: number; results: SearchResult[] }> {
  const url = `${base(cfg)}/pages/${encodeURIComponent(
    cfg.project
  )}/search/query?q=${encodeURIComponent(query)}`;
  const data = await getJson<{
    count: number;
    pages: SearchResult[];
  }>(url, cfg);
  return { count: data.count, results: data.pages ?? [] };
}
