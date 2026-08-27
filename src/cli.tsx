#!/usr/bin/env -S npx tsx
import React from "react";
import { render } from "ink";
import App from "./app.js";
import { Config } from "./api.js";

function parseArgs(argv: string[]): Config {
  const args = argv.slice(2);
  let project = process.env.COSENSE_PROJECT_NAME ?? "";
  let sid = process.env.COSENSE_SID;
  let apiDomain = process.env.API_DOMAIN ?? "scrapbox.io";

  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--project" || a === "-p") project = args[++i];
    else if (a.startsWith("--project=")) project = a.slice("--project=".length);
    else if (a === "--sid") sid = args[++i];
    else if (a.startsWith("--sid=")) sid = a.slice("--sid=".length);
    else if (a === "--domain") apiDomain = args[++i];
    else if (a.startsWith("--domain=")) apiDomain = a.slice("--domain=".length);
    else if (!a.startsWith("-") && !project) project = a;
  }

  if (!project) {
    console.error(
      "Usage: cosense-tui <project> [--sid <connect.sid>] [--domain scrapbox.io]\n" +
        "   or set COSENSE_PROJECT_NAME / COSENSE_SID env vars.\n" +
        "Example (public): cosense-tui help-jp"
    );
    process.exit(1);
  }
  return { project, sid, apiDomain };
}

const config = parseArgs(process.argv);
render(<App config={config} />);
