// Qartez guard plugin for OpenCode.
//
// Two guards in one plugin:
// 1. Read guard: blocks built-in `read` on source files, redirects to qartez_read.
// 2. Edit guard: blocks `edit` on high-PageRank / high-blast-radius files
//    until the agent runs qartez_impact first (same behavior as
//    the qartez-guard binary for Claude Code).
//
// Install: copy to .opencode/plugin/qartez-guard.ts
// The plugin activates only when .qartez/index.db exists in the project root.

import type { Plugin } from "@opencode-ai/plugin";
import { existsSync, readFileSync, statSync, mkdirSync, writeFileSync } from "fs";
import { join, relative, isAbsolute } from "path";
import { execSync } from "child_process";

const CODE_EXTS = new Set([
  "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt",
  "swift", "c", "cpp", "h", "hpp", "cs", "rb", "php", "scala",
  "zig", "lua", "dart", "ex", "exs", "erl", "hrl", "hs",
  "ml", "mli", "vue", "svelte",
]);

const PAGERANK_MIN = parseFloat(process.env.QARTEZ_GUARD_PAGERANK_MIN ?? "0.05");
const BLAST_MIN = parseInt(process.env.QARTEZ_GUARD_BLAST_MIN ?? "10", 10);
const ACK_TTL_SECS = parseInt(process.env.QARTEZ_GUARD_ACK_TTL_SECS ?? "600", 10);

function isCodeFile(filePath: string): boolean {
  const ext = filePath.split(".").pop()?.toLowerCase();
  return ext !== undefined && CODE_EXTS.has(ext);
}

function ackIsFresh(projectRoot: string, relPath: string): boolean {
  const ackFile = join(projectRoot, ".qartez", "guard-acks", relPath + ".ack");
  try {
    const stat = statSync(ackFile);
    const ageSecs = (Date.now() - stat.mtimeMs) / 1000;
    return ageSecs < ACK_TTL_SECS;
  } catch {
    return false;
  }
}

interface FileInfo {
  pagerank: number;
  blast: number;
}

function queryFileInfo(dbPath: string, relPath: string): FileInfo | null {
  try {
    const result = execSync(
      `sqlite3 -json "${dbPath}" "SELECT pagerank, ` +
      `(SELECT COUNT(*) FROM edges WHERE to_file = files.id) as blast ` +
      `FROM files WHERE path = '${relPath.replace(/'/g, "''")}';"`,
      { timeout: 3000, encoding: "utf-8" }
    );
    const rows = JSON.parse(result);
    if (rows.length > 0) {
      return { pagerank: rows[0].pagerank ?? 0, blast: rows[0].blast ?? 0 };
    }
  } catch {
    // fail-open
  }
  return null;
}

export const QartezGuard: Plugin = async ({ app }) => {
  const projectRoot = process.cwd();
  const dbPath = join(projectRoot, ".qartez", "index.db");
  if (!existsSync(dbPath)) {
    return {};
  }

  return {
    tool: {
      execute: {
        before: async (input, output) => {
          const filePath: string | undefined = output.args?.filePath;
          if (!filePath) return;

          // Guard 1: block read on source files
          if (input.tool === "read" && isCodeFile(filePath)) {
            throw new Error(
              `Qartez MCP is indexed. Use qartez_read, qartez_find, or qartez_grep ` +
              `instead of the built-in read tool for source files.`
            );
          }

          // Guard 2: block edit on high-PageRank / high-blast files
          if (input.tool === "edit" || input.tool === "write") {
            if (!isCodeFile(filePath)) return;

            const rel = isAbsolute(filePath)
              ? relative(projectRoot, filePath)
              : filePath;

            if (ackIsFresh(projectRoot, rel)) return;

            const info = queryFileInfo(dbPath, rel);
            if (!info) return;

            const prFired = info.pagerank >= PAGERANK_MIN;
            const blastFired = info.blast >= BLAST_MIN;

            if (prFired || blastFired) {
              const reasons: string[] = [];
              if (prFired) reasons.push(`PageRank ${info.pagerank.toFixed(4)} >= ${PAGERANK_MIN}`);
              if (blastFired) reasons.push(`blast radius ${info.blast} >= ${BLAST_MIN}`);

              throw new Error(
                `Qartez modification guard: ${rel} is load-bearing (${reasons.join(", ")}). ` +
                `Run qartez_impact with file_path="${rel}" first to review the blast radius, ` +
                `then retry the edit.`
              );
            }
          }
        },
      },
    },
  };
};
