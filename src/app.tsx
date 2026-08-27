import React, { useEffect, useState, useCallback } from "react";
import { Box, Text, useInput, useApp, useStdout } from "ink";
import TextInput from "ink-text-input";
import {
  Config,
  PageSummary,
  Page,
  listPages,
  getPage,
  searchPages,
  SearchResult,
} from "./api.js";
import { renderLines, RenderedLine } from "./render.js";

type View = "list" | "page" | "search";

export default function App({ config }: { config: Config }) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const rows = (stdout?.rows ?? 24) - 4;

  const [view, setView] = useState<View>("list");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // list state
  const [pages, setPages] = useState<PageSummary[]>([]);
  const [listCursor, setListCursor] = useState(0);
  const [listOffset, setListOffset] = useState(0);

  // page state
  const [page, setPage] = useState<Page | null>(null);
  const [rendered, setRendered] = useState<RenderedLine[]>([]);
  const [pageScroll, setPageScroll] = useState(0);
  const [pageLinks, setPageLinks] = useState<string[]>([]);
  const [history, setHistory] = useState<string[]>([]);

  // search state
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [searchCursor, setSearchCursor] = useState(0);
  const [searching, setSearching] = useState(true);

  const loadList = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const { pages } = await listPages(config, { limit: 200 });
      setPages(pages);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [config]);

  const openPage = useCallback(
    async (title: string, pushHistory = true) => {
      setLoading(true);
      setError(null);
      try {
        const p = await getPage(config, title);
        setPage(p);
        const r = renderLines(p.lines);
        setRendered(r);
        setPageLinks(p.links);
        setPageScroll(0);
        if (pushHistory && page) setHistory((h) => [...h, page.title]);
        setView("page");
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [config, page]
  );

  const runSearch = useCallback(
    async (q: string) => {
      if (!q.trim()) return;
      setLoading(true);
      setError(null);
      try {
        const { results } = await searchPages(config, q);
        setResults(results);
        setSearchCursor(0);
        setSearching(false);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [config]
  );

  useEffect(() => {
    loadList();
  }, [loadList]);

  useInput((input, key) => {
    if (view === "search" && searching) {
      if (key.escape) {
        setSearching(false);
        setView("list");
      }
      return; // let TextInput handle typing
    }

    if (input === "q") {
      exit();
      return;
    }

    if (view === "list") {
      if (key.downArrow || input === "j") setListCursor((c) => Math.min(c + 1, pages.length - 1));
      if (key.upArrow || input === "k") setListCursor((c) => Math.max(c - 1, 0));
      if (input === "/") {
        setQuery("");
        setSearching(true);
        setView("search");
      }
      if (input === "r") loadList();
      if (key.return && pages[listCursor]) openPage(pages[listCursor].title, false);
    } else if (view === "page") {
      if (key.downArrow || input === "j") setPageScroll((s) => Math.min(s + 1, Math.max(0, rendered.length - rows)));
      if (key.upArrow || input === "k") setPageScroll((s) => Math.max(s - 1, 0));
      if (input === " ") setPageScroll((s) => Math.min(s + rows, Math.max(0, rendered.length - rows)));
      if (input === "g") setPageScroll(0);
      if (input === "G") setPageScroll(Math.max(0, rendered.length - rows));
      if (key.leftArrow || input === "h" || key.backspace) {
        // back
        const prev = history[history.length - 1];
        if (prev) {
          setHistory((hh) => hh.slice(0, -1));
          openPage(prev, false);
        } else {
          setView("list");
        }
      }
      // open first link with L
      if (input === "L" && pageLinks[0]) openPage(pageLinks[0]);
      // numbered link jump 1-9
      if (/[1-9]/.test(input)) {
        const idx = parseInt(input, 10) - 1;
        if (pageLinks[idx]) openPage(pageLinks[idx]);
      }
    } else if (view === "search") {
      if (key.downArrow || input === "j") setSearchCursor((c) => Math.min(c + 1, results.length - 1));
      if (key.upArrow || input === "k") setSearchCursor((c) => Math.max(c - 1, 0));
      if (input === "/") {
        setQuery("");
        setSearching(true);
      }
      if (key.escape) setView("list");
      if (key.return && results[searchCursor]) openPage(results[searchCursor].title, false);
    }
  });

  // keep cursor in view for list
  useEffect(() => {
    if (listCursor < listOffset) setListOffset(listCursor);
    else if (listCursor >= listOffset + rows) setListOffset(listCursor - rows + 1);
  }, [listCursor, listOffset, rows]);

  const header = (
    <Box>
      <Text backgroundColor="blue" color="white" bold>
        {" "}
        cosense-tui{" "}
      </Text>
      <Text color="gray"> {config.project} </Text>
      {loading && <Text color="yellow">⟳ loading…</Text>}
    </Box>
  );

  if (error) {
    return (
      <Box flexDirection="column">
        {header}
        <Text color="red">Error: {error}</Text>
        <Text color="gray">q: quit  r: retry (list)</Text>
      </Box>
    );
  }

  if (view === "search" && searching) {
    return (
      <Box flexDirection="column">
        {header}
        <Box>
          <Text color="green">search: </Text>
          <TextInput value={query} onChange={setQuery} onSubmit={runSearch} />
        </Box>
        <Text color="gray">Enter: run  Esc: cancel</Text>
      </Box>
    );
  }

  if (view === "search") {
    const visible = results.slice(0, rows);
    return (
      <Box flexDirection="column">
        {header}
        <Text color="gray">
          {results.length} results for "{query}"
        </Text>
        {visible.map((r, i) => (
          <Text key={r.title} inverse={i === searchCursor}>
            {r.title}
          </Text>
        ))}
        <Text color="gray">↵ open  j/k move  / new search  Esc list  q quit</Text>
      </Box>
    );
  }

  if (view === "page" && page) {
    const slice = rendered.slice(pageScroll, pageScroll + rows);
    return (
      <Box flexDirection="column">
        {header}
        <Text color="gray">
          {page.title} · line {pageScroll + 1}/{rendered.length}
          {pageLinks.length > 0 ? `  ·  links: ${pageLinks.slice(0, 9).map((l, i) => `${i + 1}:${l}`).join("  ")}` : ""}
        </Text>
        {slice.map((l, i) => (
          <Text key={i}>{l.text === "" ? " " : l.text}</Text>
        ))}
        <Text color="gray">j/k scroll  Space page  1-9 open link  h/⌫ back  q quit</Text>
      </Box>
    );
  }

  // list view
  const visible = pages.slice(listOffset, listOffset + rows);
  return (
    <Box flexDirection="column">
      {header}
      <Text color="gray">{pages.length} pages (sort: updated)</Text>
      {visible.map((p, i) => {
        const idx = listOffset + i;
        return (
          <Text key={p.id} inverse={idx === listCursor}>
            {p.title}
          </Text>
        );
      })}
      <Text color="gray">↵ open  j/k move  / search  r reload  q quit</Text>
    </Box>
  );
}
