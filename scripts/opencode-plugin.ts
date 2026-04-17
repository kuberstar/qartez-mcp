// Qartez guard plugin for OpenCode.
// Emits machine-readable denial payloads and enforces built-in→qartez routing.

import type { Plugin } from "@opencode-ai/plugin";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "fs";
import { join, relative, isAbsolute } from "path";
import { execSync } from "child_process";

type GuardState = "READY" | "INDEXING" | "UNAVAILABLE" | "STALE";
type GuardPayloadType = "DENIED_BUILTIN_CODE_TOOL" | "RISK_ACK_REQUIRED";

const CODE_EXTS = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "go", "py", "rb", "java", "kt",
  "swift", "c", "cc", "cpp", "h", "hpp", "cs", "fs", "elm", "hs", "vue", "svelte",
  "sh", "bash",
]);

const PAGERANK_MIN = parseFloat(process.env.QARTEZ_GUARD_PAGERANK_MIN ?? "0.05");
const BLAST_MIN = parseInt(process.env.QARTEZ_GUARD_BLAST_MIN ?? "10", 10);
const ACK_TTL_SECS = parseInt(process.env.QARTEZ_ACK_TTL_SECS ?? process.env.QARTEZ_GUARD_ACK_TTL_SECS ?? "600", 10);

function resolvePath(projectRoot: string, maybePath: string): string {
  if (isAbsolute(maybePath)) return maybePath;
  return join(projectRoot, maybePath);
}

function buildPaths(projectRoot: string) {
  const indexPath = resolvePath(projectRoot, process.env.QARTEZ_INDEX_PATH ?? ".qartez");
  const statusPath = resolvePath(projectRoot, process.env.QARTEZ_STATUS_PATH ?? join(indexPath, "status.json"));
  const dbPath = join(indexPath, "index.db");
  return { indexPath, statusPath, dbPath };
}

function readStatusState(statusPath: string): GuardState {
  try {
    const raw = readFileSync(statusPath, "utf-8");
    const parsed = JSON.parse(raw) as { state?: string };
    const state = parsed.state ?? "READY";
    if (state === "INDEXING") return "INDEXING";
    if (state === "UNAVAILABLE") return "UNAVAILABLE";
    if (state === "STALE") return "STALE";
    return "READY";
  } catch {
    return "READY";
  }
}

function isCodeFile(filePath: string): boolean {
  const ext = filePath.split(".").pop()?.toLowerCase();
  return ext !== undefined && CODE_EXTS.has(ext);
}

function getPathArg(args: Record<string, unknown> | undefined): string | undefined {
  if (!args) return undefined;
  const candidate = (args.filePath ?? args.file_path ?? args.path) as string | undefined;
  return candidate && candidate.length > 0 ? candidate : undefined;
}

function denialMessage(human: string, payload: Record<string, unknown>): string {
  return `${human}\n\nQartez denial payload:\n${JSON.stringify(payload)}`;
}

function emitPayloadError(
  human: string,
  payloadType: GuardPayloadType,
  body: Record<string, unknown>
): never {
  throw new Error(denialMessage(human, { qartez: { type: payloadType, ...body } }));
}

function emitBuiltinDenial(
  toolAttempted: string,
  replacement: string[],
  reasonCode: "SOURCE_CODE_REQUIRES_QARTEZ" | "INDEX_NOT_READY",
  retryable: boolean,
  state: GuardState,
  filePath?: string
): never {
  const body: Record<string, unknown> = {
    tool_attempted: toolAttempted,
    replacement,
    reason_code: reasonCode,
    retryable,
    state,
  };
  if (filePath) body.file_path = filePath;

  const human = reasonCode === "INDEX_NOT_READY"
    ? `Qartez index is currently ${state}. Wait for READY before code-tool routing.`
    : `Built-in ${toolAttempted} on source code is denied. Retry with ${replacement.join(" or ")}.`;

  emitPayloadError(human, "DENIED_BUILTIN_CODE_TOOL", body);
}

function ackPath(projectRoot: string, relPath: string): string {
  return join(projectRoot, ".qartez", "guard-acks", `${relPath}.ack`);
}

function touchAck(projectRoot: string, relPath: string): void {
  const p = ackPath(projectRoot, relPath);
  mkdirSync(join(projectRoot, ".qartez", "guard-acks"), { recursive: true });
  writeFileSync(p, String(Date.now()));
}

function ackIsFresh(projectRoot: string, relPath: string): boolean {
  const p = ackPath(projectRoot, relPath);
  try {
    const st = statSync(p);
    return (Date.now() - st.mtimeMs) / 1000 < ACK_TTL_SECS;
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
      `sqlite3 -json "${dbPath}" "SELECT pagerank, (SELECT COUNT(*) FROM edges WHERE to_file = files.id) as blast FROM files WHERE path = '${relPath.replace(/'/g, "''")}';"`,
      { timeout: 3000, encoding: "utf-8" }
    );
    const rows = JSON.parse(result) as Array<{ pagerank?: number; blast?: number }>;
    if (rows.length > 0) {
      return { pagerank: rows[0].pagerank ?? 0, blast: rows[0].blast ?? 0 };
    }
  } catch {
    // fail-open
  }
  return null;
}

function isEditTool(tool: string): boolean {
  return tool === "edit" || tool === "write" || tool === "multiedit" || tool === "patch" || tool === "str_replace_editor";
}

export const QartezGuard: Plugin = async () => {
  const projectRoot = process.cwd();
  if (process.env.QARTEZ_GUARD_DISABLE === "1") {
    return {};
  }

  const { statusPath, dbPath } = buildPaths(projectRoot);
  if (!existsSync(dbPath)) {
    return {};
  }

  return {
    tool: {
      execute: {
        before: async (input, output) => {
          const tool = String(input.tool ?? "").toLowerCase();
          const filePath = getPathArg(output.args as Record<string, unknown> | undefined);
          const state = readStatusState(statusPath);

          if (state === "INDEXING" && (tool === "glob" || tool === "grep" || tool === "read")) {
            emitBuiltinDenial(tool, [], "INDEX_NOT_READY", false, "INDEXING", filePath);
          }

          if (tool === "glob") {
            emitBuiltinDenial("glob", ["qartez_map"], "SOURCE_CODE_REQUIRES_QARTEZ", true, state, filePath);
          }

          if (tool === "grep") {
            emitBuiltinDenial("grep", ["qartez_grep", "qartez_find"], "SOURCE_CODE_REQUIRES_QARTEZ", true, state, filePath);
          }

          if (tool === "read" && filePath && isCodeFile(filePath)) {
            emitBuiltinDenial("read", ["qartez_read", "qartez_outline"], "SOURCE_CODE_REQUIRES_QARTEZ", true, state, filePath);
          }

          if (!isEditTool(tool)) return;
          if (!filePath || !isCodeFile(filePath)) return;

          const relPath = isAbsolute(filePath) ? relative(projectRoot, filePath) : filePath;

          if (tool === "write" && output.args && typeof output.args === "object" && (output.args as Record<string, unknown>).qartez_ack === true) {
            touchAck(projectRoot, relPath);
            return;
          }

          if (ackIsFresh(projectRoot, relPath)) return;

          const info = queryFileInfo(dbPath, relPath);
          if (!info) return;

          const prFired = info.pagerank >= PAGERANK_MIN;
          const blastFired = info.blast >= BLAST_MIN;
          if (!prFired && !blastFired) return;

          emitPayloadError(
            `Qartez modification guard: ${relPath} is load-bearing. Run qartez_impact first, then retry edit.`,
            "RISK_ACK_REQUIRED",
            {
              tool_attempted: tool,
              file_path: relPath,
              replacement: ["qartez_impact"],
              reason_code: "LOAD_BEARING_FILE",
              pagerank: info.pagerank,
              blast_radius: info.blast,
              retryable: true,
              ack_ttl_secs: ACK_TTL_SECS,
            }
          );
        },
      },
    },
  };
};
